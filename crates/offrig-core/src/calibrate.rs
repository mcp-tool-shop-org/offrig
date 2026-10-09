//! Calibration metrics: how a verifier model did on a gold set. Pure functions over
//! gold claims and the verdicts a model gave; no files, no network. Design:
//! docs/verifier-design.md, "Selection protocol" and section 6.
//!
//! A model earns the default only on measured error, per check type: the false-accept
//! rate (an unsupported claim judged supported) with a Wilson 95% interval (Wilson
//! 1927), the abstain rate, and balanced accuracy on the claims it decided. The
//! coverage floor and the abstain cap stop a model that always says `cannot_tell`
//! from winning on a clean false-accept rate.

use std::collections::HashMap;

use serde::Serialize;

use crate::verify::{CheckType, Claim, Label, Verdict, VerdictKind};

/// The z value for a 95% interval.
const Z95: f64 = 1.959_963_984_540_054;

/// The default rule's thresholds.
pub const MAX_FALSE_ACCEPT_UPPER: f64 = 0.10;
pub const MAX_ABSTAIN: f64 = 0.20;
pub const MIN_BALANCED_ACCURACY: f64 = 0.80;
pub const MIN_UNSUPPORTED: usize = 100;

/// What a model said about one gold claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub claim_id: String,
    pub verdict: VerdictKind,
}

impl From<&Verdict> for Outcome {
    fn from(v: &Verdict) -> Self {
        Self {
            claim_id: v.claim_id.clone(),
            verdict: v.verdict,
        }
    }
}

/// The Wilson score interval for `x` successes in `n` trials. With no trials nothing is
/// known, so it is the whole range.
pub fn wilson_ci(x: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let n = n as f64;
    let p = x as f64 / n;
    let z2 = Z95 * Z95;
    let denom = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denom;
    let half = Z95 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denom;
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// A rate with its Wilson interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rate {
    pub hits: usize,
    pub of: usize,
    /// None when `of` is 0.
    pub rate: Option<f64>,
    pub low: f64,
    pub high: f64,
}

impl Rate {
    fn new(hits: usize, of: usize) -> Self {
        let (low, high) = wilson_ci(hits, of);
        Self {
            hits,
            of,
            rate: (of > 0).then(|| hits as f64 / of as f64),
            low,
            high,
        }
    }
}

/// How the model did on gold `cannot_tell` claims: ones the evidence cannot decide.
/// Accepting one is a false accept; saying `unsupported` or `cannot_tell` is right.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CannotTellGold {
    /// Gold `cannot_tell` claims the model answered.
    pub n: usize,
    /// Judged `supported`, of `n`.
    pub false_accept: Rate,
    /// Judged `cannot_tell`, of `n`: the model stayed silent where it should.
    pub said_cannot_tell: Rate,
    /// Judged `unsupported`, of `n`: right in effect, but it claimed a contradiction.
    pub said_unsupported: usize,
}

/// One check type's row of the calibration table.
///
/// Scoring choice for gold `cannot_tell` (R&D review, 2026-10-08): a model that accepts
/// one is wrong exactly as if it accepted an unsupported claim, so the primary
/// false-accept rate counts gold `unsupported` and gold `cannot_tell` together. Their
/// answers `unsupported` and `cannot_tell` are both correct, and neither is an abstain.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckMetrics {
    pub check_type: CheckType,
    /// Labelled gold claims of this type that the model answered.
    pub n: usize,
    pub supported_n: usize,
    pub unsupported_n: usize,
    /// Gold `cannot_tell` claims answered.
    pub cannot_tell_n: usize,
    /// Gold claims the model gave no outcome for. Any makes the rule fail.
    pub missing: usize,
    /// PRIMARY: (gold unsupported + gold cannot_tell) judged supported, of all of
    /// them. `cannot_tell` as an answer is not accepted. The default rule uses this.
    pub false_accept: Rate,
    /// The rule as first written: gold unsupported judged supported, of gold unsupported.
    pub false_accept_unsupported_only: Rate,
    /// The unsupported-only rate over planted near-misses only.
    pub subtle_false_accept: Rate,
    /// The row for gold `cannot_tell` claims alone.
    pub cannot_tell_gold: CannotTellGold,
    /// Model `cannot_tell` of gold supported + unsupported answered. Gold `cannot_tell`
    /// claims are left out, so answering `cannot_tell` everywhere still fails.
    pub abstain: Rate,
    /// Mean of two recalls over the claims the model decided: supported claims judged
    /// supported, and not-supported claims (unsupported and cannot_tell together)
    /// judged unsupported. None when a class had no decided claim.
    pub decided_balanced_accuracy: Option<f64>,
    pub passes_default_rule: bool,
}

/// Per check type metrics for `outcomes` against `gold`, in a stable order (grounded,
/// reasoning, knowledge). Gold claims without a label are not scored; a check type with
/// no labelled claim has no row. An outcome for a claim not in the gold set is ignored.
pub fn metrics(gold: &[Claim], outcomes: &[Outcome]) -> Vec<CheckMetrics> {
    let said: HashMap<&str, VerdictKind> = outcomes
        .iter()
        .map(|o| (o.claim_id.as_str(), o.verdict))
        .collect();
    [
        CheckType::Grounded,
        CheckType::Reasoning,
        CheckType::Knowledge,
    ]
    .into_iter()
    .filter_map(|ct| {
        let rows: Vec<&Claim> = gold
            .iter()
            .filter(|c| c.check_type == ct && c.label.is_some())
            .collect();
        (!rows.is_empty()).then(|| row(ct, &rows, &said))
    })
    .collect()
}

fn row(ct: CheckType, gold: &[&Claim], said: &HashMap<&str, VerdictKind>) -> CheckMetrics {
    let (mut n, mut supported_n, mut unsupported_n, mut ct_n, mut missing) = (0, 0, 0, 0, 0);
    let (mut fa_unsup, mut subtle_n, mut subtle_fa, mut abstained) = (0, 0, 0, 0);
    let (mut ct_fa, mut ct_said_ct, mut ct_said_unsup) = (0, 0, 0);
    // Decided claims: supported (right, decided) and not-supported (right, decided).
    let (mut sup_right, mut sup_decided, mut neg_right, mut neg_decided) = (0, 0, 0, 0);
    for c in gold {
        let Some(&v) = said.get(c.id.as_str()) else {
            missing += 1;
            continue;
        };
        n += 1;
        let truth = c.label.expect("filtered to labelled claims");
        let subtle = c.subtle == Some(true);
        match truth {
            Label::Supported => {
                supported_n += 1;
                match v {
                    VerdictKind::CannotTell => abstained += 1,
                    VerdictKind::Supported => {
                        sup_decided += 1;
                        sup_right += 1;
                    }
                    VerdictKind::Unsupported => sup_decided += 1,
                }
            }
            Label::Unsupported => {
                unsupported_n += 1;
                if subtle {
                    subtle_n += 1;
                }
                match v {
                    VerdictKind::CannotTell => abstained += 1,
                    VerdictKind::Supported => {
                        fa_unsup += 1;
                        if subtle {
                            subtle_fa += 1;
                        }
                        neg_decided += 1;
                    }
                    VerdictKind::Unsupported => {
                        neg_decided += 1;
                        neg_right += 1;
                    }
                }
            }
            Label::CannotTell => {
                ct_n += 1;
                match v {
                    VerdictKind::CannotTell => ct_said_ct += 1,
                    VerdictKind::Supported => {
                        ct_fa += 1;
                        neg_decided += 1;
                    }
                    VerdictKind::Unsupported => {
                        ct_said_unsup += 1;
                        neg_decided += 1;
                        neg_right += 1;
                    }
                }
            }
        }
    }
    let false_accept = Rate::new(fa_unsup + ct_fa, unsupported_n + ct_n);
    let abstain = Rate::new(abstained, supported_n + unsupported_n);
    let decided_balanced_accuracy = (sup_decided > 0 && neg_decided > 0).then(|| {
        (sup_right as f64 / sup_decided as f64 + neg_right as f64 / neg_decided as f64) / 2.0
    });
    let passes_default_rule = missing == 0
        && unsupported_n >= MIN_UNSUPPORTED
        && false_accept.high < MAX_FALSE_ACCEPT_UPPER
        && abstain.rate.is_some_and(|a| a <= MAX_ABSTAIN)
        && decided_balanced_accuracy.is_some_and(|b| b >= MIN_BALANCED_ACCURACY);
    CheckMetrics {
        check_type: ct,
        n,
        supported_n,
        unsupported_n,
        cannot_tell_n: ct_n,
        missing,
        false_accept,
        false_accept_unsupported_only: Rate::new(fa_unsup, unsupported_n),
        subtle_false_accept: Rate::new(subtle_fa, subtle_n),
        cannot_tell_gold: CannotTellGold {
            n: ct_n,
            false_accept: Rate::new(ct_fa, ct_n),
            said_cannot_tell: Rate::new(ct_said_ct, ct_n),
            said_unsupported: ct_said_unsup,
        },
        abstain,
        decided_balanced_accuracy,
        passes_default_rule,
    }
}

/// True when the model earns the default: both `grounded` and `reasoning` pass the
/// rule. `knowledge` is reported but is not part of it.
pub fn passes_default(rows: &[CheckMetrics]) -> bool {
    [CheckType::Grounded, CheckType::Reasoning]
        .into_iter()
        .all(|ct| {
            rows.iter()
                .any(|r| r.check_type == ct && r.passes_default_rule)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gold(id: &str, ct: CheckType, label: Label, subtle: bool) -> Claim {
        Claim {
            id: id.into(),
            check_type: ct,
            claim: "c".into(),
            context: vec![],
            evidence_paths: vec![],
            high_stakes: false,
            label: Some(label),
            subtle: Some(subtle),
            split: None,
            origin: None,
        }
    }

    fn out(id: &str, verdict: VerdictKind) -> Outcome {
        Outcome {
            claim_id: id.into(),
            verdict,
        }
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 5e-4, "{a} vs {b}");
    }

    #[test]
    fn wilson_matches_published_values() {
        // 0/100: the upper bound is z^2 / (n + z^2) = 3.8415 / 103.8415.
        let (lo, hi) = wilson_ci(0, 100);
        close(lo, 0.0);
        close(hi, 0.0370);
        // 4/100 is the largest count whose upper bound stays under 10%.
        let (lo, hi) = wilson_ci(4, 100);
        close(lo, 0.0157);
        close(hi, 0.0984);
        assert!(hi < MAX_FALSE_ACCEPT_UPPER);
        let (lo, hi) = wilson_ci(5, 100);
        close(lo, 0.0215);
        close(hi, 0.1118);
        assert!(hi > MAX_FALSE_ACCEPT_UPPER);
        // 10/20 is symmetric about one half.
        let (lo, hi) = wilson_ci(10, 20);
        close(lo, 0.299);
        close(hi, 0.701);
        let (lo, hi) = wilson_ci(20, 20);
        close(lo, 0.8389);
        assert_eq!(hi, 1.0);
        assert_eq!(wilson_ci(0, 0), (0.0, 1.0));
    }

    /// 100 unsupported claims (the first `subtle` of them near-misses) and 100
    /// supported ones, the model answering by the given closures.
    fn gold_set(ct: CheckType, subtle: usize) -> Vec<Claim> {
        let mut g = vec![];
        for i in 0..100 {
            g.push(gold(&format!("u{i}"), ct, Label::Unsupported, i < subtle));
            g.push(gold(&format!("s{i}"), ct, Label::Supported, false));
        }
        g
    }

    #[test]
    fn a_good_model_passes_the_default_rule() {
        let g = gold_set(CheckType::Grounded, 40);
        let mut o = vec![];
        for i in 0..100 {
            // 3 false accepts (2 of them near-misses), 3 abstentions, the rest right.
            let u = match i {
                0 | 1 | 50 => VerdictKind::Supported,
                2..=4 => VerdictKind::CannotTell,
                _ => VerdictKind::Unsupported,
            };
            // 2 wrong (called unsupported), 3 abstentions, the rest right.
            let s = match i {
                0 | 1 => VerdictKind::Unsupported,
                2..=4 => VerdictKind::CannotTell,
                _ => VerdictKind::Supported,
            };
            o.push(out(&format!("u{i}"), u));
            o.push(out(&format!("s{i}"), s));
        }
        let rows = metrics(&g, &o);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            (r.n, r.supported_n, r.unsupported_n, r.missing),
            (200, 100, 100, 0)
        );
        assert_eq!((r.false_accept.hits, r.false_accept.of), (3, 100));
        close(r.false_accept.rate.expect("rate"), 0.03);
        close(r.false_accept.high, 0.0845);
        // Near-misses are the first 40; u0 and u1 are among them, u50 is not.
        assert_eq!(
            (r.subtle_false_accept.hits, r.subtle_false_accept.of),
            (2, 40)
        );
        // 6 abstentions of 200.
        close(r.abstain.rate.expect("abstain"), 0.03);
        // Decided: unsupported 97 decided, 94 right; supported 97 decided, 95 right.
        close(
            r.decided_balanced_accuracy.expect("balanced"),
            (94.0 / 97.0 + 95.0 / 97.0) / 2.0,
        );
        assert!(r.passes_default_rule);
        // Knowledge is reported but the default needs reasoning as well.
        assert!(!passes_default(&rows));
    }

    #[test]
    fn five_false_accepts_in_a_hundred_fail_on_the_interval() {
        let g = gold_set(CheckType::Reasoning, 0);
        let o: Vec<Outcome> = (0..100)
            .flat_map(|i| {
                let u = if i < 5 {
                    VerdictKind::Supported
                } else {
                    VerdictKind::Unsupported
                };
                [
                    out(&format!("u{i}"), u),
                    out(&format!("s{i}"), VerdictKind::Supported),
                ]
            })
            .collect();
        let r = &metrics(&g, &o)[0];
        assert!(r.false_accept.high > 0.10);
        assert!(!r.passes_default_rule);
    }

    #[test]
    fn a_model_that_always_abstains_fails_the_rule() {
        let g = gold_set(CheckType::Grounded, 0);
        let o: Vec<Outcome> = g
            .iter()
            .map(|c| out(&c.id, VerdictKind::CannotTell))
            .collect();
        let r = &metrics(&g, &o)[0];
        // A perfect false-accept rate, since nothing was accepted...
        assert_eq!(r.false_accept.rate, Some(0.0));
        assert!(r.false_accept.high < 0.10);
        // ...but it decided nothing.
        assert_eq!(r.abstain.rate, Some(1.0));
        assert_eq!(r.decided_balanced_accuracy, None);
        assert!(!r.passes_default_rule);
    }

    #[test]
    fn too_few_unsupported_claims_or_missing_answers_fail_the_rule() {
        // 99 unsupported claims, all right: not enough to bound the error.
        let mut g = gold_set(CheckType::Grounded, 0);
        g.retain(|c| c.id != "u99");
        let o: Vec<Outcome> = g
            .iter()
            .map(|c| {
                let v = if c.label == Some(Label::Supported) {
                    VerdictKind::Supported
                } else {
                    VerdictKind::Unsupported
                };
                out(&c.id, v)
            })
            .collect();
        let r = &metrics(&g, &o)[0];
        assert_eq!(r.unsupported_n, 99);
        assert!(!r.passes_default_rule);
        // With all 100 and every answer present it passes...
        let g = gold_set(CheckType::Grounded, 0);
        let mut o: Vec<Outcome> = g
            .iter()
            .map(|c| {
                let v = if c.label == Some(Label::Supported) {
                    VerdictKind::Supported
                } else {
                    VerdictKind::Unsupported
                };
                out(&c.id, v)
            })
            .collect();
        assert!(metrics(&g, &o)[0].passes_default_rule);
        // ...and drops out when one answer is missing.
        o.pop();
        let r = &metrics(&g, &o)[0];
        assert_eq!(r.missing, 1);
        assert!(!r.passes_default_rule);
    }

    #[test]
    fn rows_are_per_check_type_and_unlabelled_claims_are_not_scored() {
        let mut g = vec![
            gold("k1", CheckType::Knowledge, Label::Unsupported, false),
            gold("g1", CheckType::Grounded, Label::Supported, false),
            gold("g2", CheckType::Grounded, Label::Unsupported, true),
        ];
        let mut free = gold("g3", CheckType::Grounded, Label::Supported, false);
        free.label = None;
        g.push(free);
        let o = [
            out("k1", VerdictKind::Supported),
            out("g1", VerdictKind::Supported),
            out("g2", VerdictKind::Supported),
            out("g3", VerdictKind::Supported),
            out("not-in-gold", VerdictKind::Supported),
        ];
        let rows = metrics(&g, &o);
        let kinds: Vec<CheckType> = rows.iter().map(|r| r.check_type).collect();
        assert_eq!(kinds, [CheckType::Grounded, CheckType::Knowledge]);
        assert_eq!(rows[0].n, 2);
        assert_eq!(rows[0].subtle_false_accept.hits, 1);
        assert_eq!(rows[1].false_accept.hits, 1);
        assert_eq!(rows[0].decided_balanced_accuracy, Some(0.5));
        assert!(metrics(&[], &[]).is_empty());
        assert!(!passes_default(&rows));
    }

    #[test]
    fn the_default_needs_grounded_and_reasoning_and_ignores_knowledge() {
        let pass = |ct| {
            let g = gold_set(ct, 0);
            let o: Vec<Outcome> = g
                .iter()
                .map(|c| {
                    out(
                        &c.id,
                        if c.label == Some(Label::Supported) {
                            VerdictKind::Supported
                        } else {
                            VerdictKind::Unsupported
                        },
                    )
                })
                .collect();
            metrics(&g, &o)
        };
        let mut rows = pass(CheckType::Grounded);
        assert!(!passes_default(&rows));
        rows.extend(pass(CheckType::Reasoning));
        assert!(passes_default(&rows));
        // A failing knowledge row changes nothing.
        let mut k = pass(CheckType::Knowledge);
        k[0].passes_default_rule = false;
        rows.extend(k);
        assert!(passes_default(&rows));
    }

    fn ct_gold(prefix: &str, n: usize) -> Vec<Claim> {
        (0..n)
            .map(|i| {
                gold(
                    &format!("{prefix}{i}"),
                    CheckType::Grounded,
                    Label::CannotTell,
                    false,
                )
            })
            .collect()
    }

    /// Supported and unsupported gold answered right.
    fn all_right(g: &[Claim]) -> Vec<Outcome> {
        g.iter()
            .map(|c| {
                out(
                    &c.id,
                    if c.label == Some(Label::Supported) {
                        VerdictKind::Supported
                    } else {
                        VerdictKind::Unsupported
                    },
                )
            })
            .collect()
    }

    #[test]
    fn gold_cannot_tell_is_scored_as_the_review_decided() {
        // 100 supported, 100 unsupported (first 10 near-misses), 40 cannot_tell.
        let mut g = gold_set(CheckType::Grounded, 10);
        g.extend(ct_gold("t", 40));
        let mut o = vec![];
        for i in 0..100 {
            // 2 false accepts (one a near-miss), 3 abstains, 95 right.
            let u = match i {
                0 | 50 => VerdictKind::Supported,
                1..=3 => VerdictKind::CannotTell,
                _ => VerdictKind::Unsupported,
            };
            // 1 judged unsupported, 4 abstains, 95 right.
            let s = match i {
                0 => VerdictKind::Unsupported,
                1..=4 => VerdictKind::CannotTell,
                _ => VerdictKind::Supported,
            };
            o.push(out(&format!("u{i}"), u));
            o.push(out(&format!("s{i}"), s));
        }
        for i in 0..40 {
            // 4 accepted, 10 said unsupported, 26 cannot_tell.
            let t = match i {
                0..=3 => VerdictKind::Supported,
                4..=13 => VerdictKind::Unsupported,
                _ => VerdictKind::CannotTell,
            };
            o.push(out(&format!("t{i}"), t));
        }
        let r = &metrics(&g, &o)[0];
        assert_eq!((r.n, r.cannot_tell_n, r.missing), (240, 40, 0));
        // Primary: (2 + 4) of (100 + 40).
        assert_eq!((r.false_accept.hits, r.false_accept.of), (6, 140));
        let (lo, hi) = wilson_ci(6, 140);
        close(r.false_accept.low, lo);
        close(r.false_accept.high, hi);
        // The rule as first written: 2 of 100, with its own interval.
        let u = &r.false_accept_unsupported_only;
        assert_eq!((u.hits, u.of), (2, 100));
        close(u.high, wilson_ci(2, 100).1);
        assert_eq!(
            (r.subtle_false_accept.hits, r.subtle_false_accept.of),
            (1, 10)
        );
        // The cannot_tell row.
        let c = &r.cannot_tell_gold;
        assert_eq!((c.n, c.false_accept.hits, c.said_unsupported), (40, 4, 10));
        close(c.said_cannot_tell.rate.expect("rate"), 26.0 / 40.0);
        // Abstains: 3 + 4 of 200 supported and unsupported; the 26 are not abstains.
        assert_eq!((r.abstain.hits, r.abstain.of), (7, 200));
        // Decided supported: 96, 95 right. Decided not-supported: unsupported 97
        // (95 right) + cannot_tell decided 14 (10 right) = 111, 105 right.
        close(
            r.decided_balanced_accuracy.expect("balanced"),
            (95.0 / 96.0 + 105.0 / 111.0) / 2.0,
        );
        // The primary upper bound for 6 of 140 is under 10%, and so the model passes.
        assert!(r.false_accept.high < 0.10);
        assert!(r.passes_default_rule);
    }

    #[test]
    fn accepting_cannot_tell_claims_can_fail_a_model_the_old_rule_passed() {
        let mut g = gold_set(CheckType::Grounded, 0);
        g.extend(ct_gold("t", 40));
        let mut o = all_right(&g[..200]);
        // Accepts 10 of 40 cannot_tell claims.
        for i in 0..40 {
            let v = if i < 10 {
                VerdictKind::Supported
            } else {
                VerdictKind::CannotTell
            };
            o.push(out(&format!("t{i}"), v));
        }
        let r = &metrics(&g, &o)[0];
        assert_eq!(r.false_accept_unsupported_only.hits, 0);
        assert!(r.false_accept_unsupported_only.high < 0.10);
        assert_eq!((r.false_accept.hits, r.false_accept.of), (10, 140));
        assert!(r.false_accept.high > 0.10);
        assert!(!r.passes_default_rule);
        // Their silence is not an abstain.
        assert_eq!(r.abstain.hits, 0);
    }

    #[test]
    fn answering_cannot_tell_everywhere_still_fails_on_abstain() {
        let mut g = gold_set(CheckType::Grounded, 0);
        g.extend(ct_gold("t", 400));
        let o: Vec<Outcome> = g
            .iter()
            .map(|c| out(&c.id, VerdictKind::CannotTell))
            .collect();
        let r = &metrics(&g, &o)[0];
        // The 400 right answers do not dilute abstain: it is over the other 200.
        assert_eq!((r.abstain.hits, r.abstain.of), (200, 200));
        assert_eq!(r.abstain.rate, Some(1.0));
        assert_eq!(r.false_accept.hits, 0);
        assert_eq!(r.cannot_tell_gold.said_cannot_tell.rate, Some(1.0));
        assert_eq!(r.decided_balanced_accuracy, None);
        assert!(!r.passes_default_rule);
    }

    #[test]
    fn a_missing_cannot_tell_answer_counts_as_missing() {
        let mut g = gold_set(CheckType::Grounded, 0);
        g.extend(ct_gold("t", 1));
        let o = all_right(&g[..200]);
        let r = &metrics(&g, &o)[0];
        assert_eq!((r.missing, r.cannot_tell_n), (1, 0));
        assert!(!r.passes_default_rule);
    }

    #[test]
    fn an_outcome_comes_from_a_verdict_record() {
        let v = crate::verify::sample_verdict("c9", VerdictKind::Unsupported);
        assert_eq!(Outcome::from(&v), out("c9", VerdictKind::Unsupported));
    }
}
