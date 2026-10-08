//! The runner's decisions, as pure functions so every rule is tested. The process
//! that acts on them lives in offrig-mcp (`--runner`).
//!
//! Evidence (docs/sidecar-design.md, phase 3a): revisions help only against an external
//! signal and mostly in the first round; self-grading drifts upward; work already under
//! way should finish before new work starts; the critical path goes first.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::checks::{self, Check, Outcome};
use crate::context::{self, Assembled, Input, Parts};
use crate::error::Result;
use crate::index;
use crate::roles;
use crate::store::{Handoff, Kind, Output, Query, State, Store};

/// One turn's prompt, built from the project store, and what went into it.
pub struct Prepared {
    pub assembled: Assembled,
    pub role_block: String,
    /// Brief and constraint records, always injected.
    pub injected: Vec<i64>,
}

/// Build a turn's prompt for a handoff: role block, brief, constraints, the handoff,
/// retrieved decisions and facts, results of its dependencies, the last checkpoint,
/// then the instruction. `offrig_ask` and the runner both use this.
pub fn prepare(
    store: &Store,
    role_os_dir: Option<&Path>,
    h: &Handoff,
    instruction: &str,
    budget_chars: usize,
) -> Result<Prepared> {
    let role = roles::load(&h.role_id, role_os_dir)?;
    let role_block = roles::render(&role);
    let briefs = store.active(Kind::Brief)?;
    let constraints = store.active(Kind::Constraint)?;
    // Hybrid ranking when the project index has embeddings, keywords otherwise.
    let retrieved: Vec<_> = index::records_for_prompt(
        store,
        &Query {
            text: h.mission.clone(),
            limit: 8,
            ..Default::default()
        },
    )?
    .into_iter()
    .filter(|r| matches!(r.kind, Kind::Decision | Kind::Fact))
    .collect();
    let checkpoint = store.latest_checkpoint(h.id)?;
    let inputs = inputs(store, h)?;
    let assembled = context::assemble(
        &Parts {
            role_block: &role_block,
            briefs: &briefs,
            constraints: &constraints,
            handoff: h,
            retrieved: &retrieved,
            checkpoint: checkpoint.as_ref(),
            inputs: &inputs,
            instruction,
        },
        budget_chars,
    );
    Ok(Prepared {
        assembled,
        role_block,
        injected: briefs.iter().chain(&constraints).map(|r| r.id).collect(),
    })
}

/// The best result of each completed dependency.
pub fn inputs(store: &Store, h: &Handoff) -> Result<Vec<Input>> {
    let mut out = Vec::new();
    for d in &h.depends_on {
        let Some(dep) = store.handoff(*d)? else {
            continue;
        };
        if dep.state != State::Complete {
            continue;
        }
        if let Some(o) = best(&store.outputs(dep.id)?) {
            out.push(Input {
                handoff_id: dep.id,
                mission: dep.mission.clone(),
                body: o.body.clone(),
            });
        }
    }
    Ok(out)
}

/// Revision turns after the draft. Gains are largest in round one and plateau or
/// regress after two.
pub const MAX_REVISIONS: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Run a revision turn against the failed checks.
    Revise,
    /// The checks pass and cover the acceptance check.
    Complete,
    /// The orchestrating agent judges it: the checks do not cover acceptance, there
    /// are none, or revisions ran out with checks still failing.
    Review { why: String },
}

/// What follows turn `turn` (1 = the draft) given its check outcomes.
pub fn after_turn(h: &Handoff, turn: i64, outcomes: &[Outcome]) -> Next {
    if h.checks.is_empty() {
        return Next::Review {
            why: "no checks: the acceptance check needs judgement".into(),
        };
    }
    let failed = outcomes.iter().filter(|o| !o.pass).count();
    if failed == 0 {
        return if h.accept_on_checks {
            Next::Complete
        } else {
            Next::Review {
                why: "checks pass; the rest of the acceptance check needs judgement".into(),
            }
        };
    }
    if turn <= MAX_REVISIONS {
        Next::Revise
    } else {
        Next::Review {
            why: format!("{failed} check(s) still failing after {MAX_REVISIONS} revisions"),
        }
    }
}

/// The turn to keep: most checks passed, the later turn on a tie. A revision that
/// breaks what passed before does not replace the better draft.
pub fn best(outputs: &[Output]) -> Option<&Output> {
    outputs
        .iter()
        .max_by_key(|o| (o.outcomes.iter().filter(|c| c.pass).count(), o.turn))
}

/// The ready handoff to start next, among those not already running: the one with
/// the longest chain of handoffs waiting on it, then the oldest.
pub fn pick(ready: &[Handoff], all: &[Handoff], busy: &HashSet<i64>) -> Option<i64> {
    let depth = chain_depths(all);
    ready
        .iter()
        .filter(|h| !busy.contains(&h.id))
        .max_by_key(|h| {
            (
                depth.get(&h.id).copied().unwrap_or(0),
                std::cmp::Reverse(h.id),
            )
        })
        .map(|h| h.id)
}

/// For each handoff, the length of the longest chain of handoffs that depend on it.
fn chain_depths(all: &[Handoff]) -> HashMap<i64, usize> {
    let mut dependents: HashMap<i64, Vec<i64>> = HashMap::new();
    for h in all {
        for d in &h.depends_on {
            dependents.entry(*d).or_default().push(h.id);
        }
    }
    fn depth(
        id: i64,
        dependents: &HashMap<i64, Vec<i64>>,
        memo: &mut HashMap<i64, usize>,
        seen: &mut HashSet<i64>,
    ) -> usize {
        if let Some(d) = memo.get(&id) {
            return *d;
        }
        if !seen.insert(id) {
            return 0; // a cycle; add_handoff only allows existing ids, but stay safe
        }
        let d = dependents.get(&id).map_or(0, |ds| {
            ds.iter()
                .map(|c| 1 + depth(*c, dependents, memo, seen))
                .max()
                .unwrap_or(0)
        });
        memo.insert(id, d);
        d
    }
    let mut memo = HashMap::new();
    for h in all {
        depth(h.id, &dependents, &mut memo, &mut HashSet::new());
    }
    memo
}

/// A check as the model is told it in advance.
pub fn describe(c: &Check) -> String {
    match c {
        Check::Heading { text } => format!("a heading containing \"{text}\""),
        Check::Items { heading, min } => {
            format!("at least {min} list items under a heading containing \"{heading}\"")
        }
        Check::Contains { text, min } if *min > 1 => format!("\"{text}\" at least {min} times"),
        Check::Contains { text, .. } => format!("the text \"{text}\""),
        Check::Absent { text } => format!("no \"{text}\" anywhere"),
        Check::NoRepeats => "no line repeated".into(),
        Check::Words { min, max } => match (min, max) {
            (Some(a), Some(b)) => format!("{a} to {b} words"),
            (Some(a), None) => format!("at least {a} words"),
            (None, Some(b)) => format!("at most {b} words"),
            (None, None) => "any length".into(),
        },
    }
}

pub fn draft_instruction(h: &Handoff) -> String {
    let mut s = String::from(
        "Produce the deliverable for this handoff now. Return only the deliverable, in Markdown, \
         with no preamble and no closing remarks.",
    );
    if !h.checks.is_empty() {
        s.push_str("\nIt will be checked by code for:\n");
        for c in &h.checks {
            s.push_str(&format!("- {}\n", describe(c)));
        }
    }
    s
}

/// True when a revision came back unchanged: the model is not acting on the
/// feedback, so the handoff goes to review instead of spending another turn.
/// Measured 2026-10-03: three identical 97-token turns on one handoff.
pub fn stalled(previous: &str, current: &str) -> bool {
    previous.trim() == current.trim()
}

/// The checks the runner evaluates: the handoff's own, plus the padding guard.
pub fn effective_checks(h: &Handoff) -> Vec<Check> {
    let mut c = h.checks.clone();
    if !c.is_empty() && !c.contains(&Check::NoRepeats) {
        c.push(Check::NoRepeats);
    }
    c
}

pub fn revise_instruction(previous: &str, outcomes: &[Outcome]) -> String {
    format!(
        "Your previous output failed these checks:\n{}\nRevise it to fix exactly these. Keep \
         everything else that already works. Restructure in place rather than appending: \
         return the complete deliverable once, with every item appearing a single time, in \
         Markdown, with no preamble.\n\nYour previous output:\n\n{}",
        checks::feedback(outcomes),
        previous.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::State;

    fn h(id: i64, deps: Vec<i64>, checks: Vec<Check>, accept: bool) -> Handoff {
        Handoff {
            id,
            role_id: "game-designer".into(),
            mission: format!("m{id}"),
            acceptance: "a".into(),
            scope: vec![],
            depends_on: deps,
            state: State::Pending,
            attempts: 0,
            branch: None,
            model: None,
            role_hash: None,
            prompt_hash: None,
            result_record: None,
            created_at: 0,
            updated_at: 0,
            checks,
            accept_on_checks: accept,
        }
    }

    fn outcome(pass: bool) -> Outcome {
        Outcome {
            check: Check::Heading {
                text: "Verbs".into(),
            },
            pass,
            detail: if pass {
                "ok".into()
            } else {
                "needs a heading containing \"Verbs\"".into()
            },
        }
    }

    fn out(turn: i64, passes: &[bool]) -> Output {
        Output {
            id: turn,
            handoff_id: 1,
            turn,
            body: format!("turn {turn}"),
            outcomes: passes.iter().map(|p| outcome(*p)).collect(),
            model: "m".into(),
            tokens: None,
            created_at: 0,
        }
    }

    #[test]
    fn revisions_run_only_against_failed_checks_and_stop_after_two() {
        let checked = h(
            1,
            vec![],
            vec![Check::Heading {
                text: "Verbs".into(),
            }],
            true,
        );
        assert_eq!(after_turn(&checked, 1, &[outcome(false)]), Next::Revise);
        assert_eq!(after_turn(&checked, 2, &[outcome(false)]), Next::Revise);
        assert!(matches!(
            after_turn(&checked, 3, &[outcome(false)]),
            Next::Review { .. }
        ));
        assert_eq!(after_turn(&checked, 1, &[outcome(true)]), Next::Complete);
    }

    #[test]
    fn nothing_completes_without_checks_that_cover_acceptance() {
        let unchecked = h(1, vec![], vec![], false);
        assert!(
            matches!(after_turn(&unchecked, 1, &[]), Next::Review { .. }),
            "no self-critique turn"
        );
        let partial = h(
            2,
            vec![],
            vec![Check::Heading {
                text: "Verbs".into(),
            }],
            false,
        );
        assert!(matches!(
            after_turn(&partial, 1, &[outcome(true)]),
            Next::Review { .. }
        ));
    }

    #[test]
    fn a_revision_that_breaks_passing_checks_does_not_win() {
        let outs = [
            out(1, &[true, false]),
            out(2, &[false, false]),
            out(3, &[true, false]),
        ];
        assert_eq!(
            best(&outs).expect("some").turn,
            3,
            "ties go to the later turn"
        );
        let outs = [out(1, &[true, true]), out(2, &[true, false])];
        assert_eq!(best(&outs).expect("some").turn, 1);
        assert!(best(&[]).is_none());
    }

    #[test]
    fn the_critical_path_starts_first() {
        // 1 <- 3 <- 4 (chain of two waits on 1); 2 has nothing waiting.
        let all = [
            h(1, vec![], vec![], false),
            h(2, vec![], vec![], false),
            h(3, vec![1], vec![], false),
            h(4, vec![3], vec![], false),
        ];
        let ready = [all[1].clone(), all[0].clone()];
        assert_eq!(pick(&ready, &all, &HashSet::new()), Some(1));
        assert_eq!(
            pick(&ready, &all, &HashSet::from([1])),
            Some(2),
            "busy ones are skipped"
        );
        let flat = [h(5, vec![], vec![], false), h(6, vec![], vec![], false)];
        assert_eq!(
            pick(&flat, &flat, &HashSet::new()),
            Some(5),
            "oldest on a tie"
        );
    }

    #[test]
    fn an_unchanged_revision_is_a_stall() {
        assert!(stalled("Barks\n- a", "Barks\n- a\n"));
        assert!(!stalled("Barks\n- a", "## Barks\n- a"));
    }

    #[test]
    fn checked_handoffs_also_get_the_padding_guard() {
        let checked = h(
            1,
            vec![],
            vec![Check::Heading {
                text: "Verbs".into(),
            }],
            true,
        );
        assert_eq!(effective_checks(&checked).last(), Some(&Check::NoRepeats));
        assert!(
            effective_checks(&h(2, vec![], vec![], false)).is_empty(),
            "unchecked stays unchecked"
        );
        let r = revise_instruction("old", &[outcome(false)]);
        assert!(r.contains("rather than appending"));
    }

    #[test]
    fn instructions_name_the_checks() {
        let x = h(
            1,
            vec![],
            vec![
                Check::Items {
                    heading: "Verbs".into(),
                    min: 3,
                },
                Check::Words {
                    min: None,
                    max: Some(400),
                },
            ],
            true,
        );
        let d = draft_instruction(&x);
        assert!(
            d.contains("at least 3 list items under a heading containing \"Verbs\""),
            "{d}"
        );
        assert!(d.contains("at most 400 words"));
        let r = revise_instruction("old body", &[outcome(false), outcome(true)]);
        assert!(r.contains("needs a heading containing") && r.contains("old body"));
        assert_eq!(r.matches("- ").count(), 1, "only failures are quoted");
    }

    fn mem_store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    fn note(s: &Store, kind: Kind, body: &str) -> i64 {
        s.record(crate::store::NewRecord {
            kind: Some(kind),
            body: body.into(),
            author: "test".into(),
            ..Default::default()
        })
        .expect("record")
    }

    fn new_handoff(s: &Store, mission: &str, deps: Vec<i64>) -> i64 {
        s.add_handoff(crate::store::NewHandoff {
            role_id: "game-designer".into(),
            mission: mission.into(),
            acceptance: "has a Verbs heading".into(),
            depends_on: deps,
            ..Default::default()
        })
        .expect("handoff")
    }

    #[test]
    fn a_prompt_carries_the_brief_constraints_retrieved_notes_and_the_instruction() {
        let s = mem_store();
        let brief = note(&s, Kind::Brief, "A frontier JRPG about relays.");
        let rule = note(&s, Kind::Constraint, "Never touch the solver crate.");
        let _decision = note(&s, Kind::Decision, "Combat speed uses initiative bands.");
        let _unrelated = note(&s, Kind::Fact, "Completely unrelated fish trivia.");
        let id = new_handoff(&s, "design combat initiative", vec![]);
        let h = s.handoff(id).expect("read").expect("exists");

        let p = prepare(&s, None, &h, "Write it now.", 200_000).expect("prepare");
        assert_eq!(p.injected, vec![brief, rule]);
        assert!(!p.role_block.is_empty());
        let text = format!("{:?}", p.assembled);
        for needle in [
            "A frontier JRPG about relays.",
            "Never touch the solver crate.",
            "initiative bands",
            "design combat initiative",
            "Write it now.",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in {text}");
        }
        assert!(!text.contains("fish trivia"), "unrelated notes stay out");
    }

    #[test]
    fn an_unknown_role_stops_the_prompt() {
        let s = mem_store();
        let id = new_handoff(&s, "m", vec![]);
        let mut h = s.handoff(id).expect("read").expect("exists");
        h.role_id = "no-such-role".into();
        assert!(prepare(&s, None, &h, "go", 1000).is_err());
    }

    #[test]
    fn a_handoff_is_fed_the_best_output_of_each_completed_dependency() {
        let s = mem_store();
        let done = new_handoff(&s, "write the verbs", vec![]);
        let pending = new_handoff(&s, "not finished", vec![]);
        let empty = new_handoff(&s, "complete but no output", vec![]);
        for (id, turn, body, passes) in [
            (done, 1, "weak draft", vec![false]),
            (done, 2, "strong revision", vec![true]),
            (pending, 1, "ignored: not complete", vec![true]),
        ] {
            let outcomes: Vec<Outcome> = passes.into_iter().map(outcome).collect();
            s.add_output(crate::store::NewOutput {
                handoff_id: id,
                turn,
                body,
                outcomes: &outcomes,
                model: "m",
                tokens: Some(10),
            })
            .expect("output");
        }
        for id in [done, empty] {
            s.transition(id, State::Dispatched, "go", None)
                .expect("dispatch");
            s.transition(id, State::Complete, "done", None)
                .expect("complete");
        }
        let child = new_handoff(&s, "uses them", vec![done, pending, empty]);
        let mut h = s.handoff(child).expect("read").expect("exists");
        h.depends_on.push(9_999);
        let ins = inputs(&s, &h).expect("inputs");
        assert_eq!(
            ins.len(),
            1,
            "pending, output-less and unknown deps are skipped"
        );
        assert_eq!(ins[0].handoff_id, done);
        assert_eq!(ins[0].body, "strong revision");
        assert_eq!(ins[0].mission, "write the verbs");
    }

    #[test]
    fn every_check_is_described_in_words_before_the_model_writes() {
        let words = |min, max| describe(&Check::Words { min, max });
        assert_eq!(words(Some(5), Some(9)), "5 to 9 words");
        assert_eq!(words(Some(5), None), "at least 5 words");
        assert_eq!(words(None, Some(9)), "at most 9 words");
        assert_eq!(words(None, None), "any length");
        assert_eq!(
            describe(&Check::Contains {
                text: "x".into(),
                min: 3
            }),
            "\"x\" at least 3 times"
        );
        assert_eq!(
            describe(&Check::Contains {
                text: "x".into(),
                min: 1
            }),
            "the text \"x\""
        );
        assert_eq!(
            describe(&Check::Absent {
                text: "TODO".into()
            }),
            "no \"TODO\" anywhere"
        );
        assert_eq!(describe(&Check::NoRepeats), "no line repeated");
        assert_eq!(
            describe(&Check::Heading {
                text: "Verbs".into()
            }),
            "a heading containing \"Verbs\""
        );
    }

    #[test]
    fn passing_checks_that_cover_acceptance_complete_it_and_otherwise_wait_for_review() {
        let c = vec![Check::NoRepeats];
        let covered = h(1, vec![], c.clone(), true);
        assert_eq!(after_turn(&covered, 1, &[outcome(true)]), Next::Complete);
        let partial = h(2, vec![], c, false);
        match after_turn(&partial, 1, &[outcome(true)]) {
            Next::Review { why } => assert!(why.contains("needs judgement"), "{why}"),
            other => panic!("{other:?}"),
        }
        match after_turn(&covered, 3, &[outcome(false), outcome(true)]) {
            Next::Review { why } => assert!(why.starts_with("1 check(s) still failing"), "{why}"),
            other => panic!("{other:?}"),
        }
    }
}
