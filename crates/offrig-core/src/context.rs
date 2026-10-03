//! Builds a turn's context from the project database, never from chat history, so a
//! compaction or a restart loses nothing. Order and budget follow the evidence in
//! docs/sidecar-design.md: what must not be missed goes first and last, a small
//! retrieved block goes in the middle, and constraints are never trimmed.

use crate::store::{Handoff, Record};

pub struct Parts<'a> {
    pub role_block: &'a str,
    pub briefs: &'a [Record],
    pub constraints: &'a [Record],
    pub handoff: &'a Handoff,
    /// Best match first; trimmed from the end when over budget.
    pub retrieved: &'a [Record],
    pub checkpoint: Option<&'a Record>,
    /// Results of the handoffs this one depends on.
    pub inputs: &'a [Input],
    pub instruction: &'a str,
}

/// A finished dependency's result, given to the handoffs that wait on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub handoff_id: i64,
    pub mission: String,
    pub body: String,
}

/// Each input is cut to this many characters so one long result cannot crowd out
/// the rest of the context.
pub const INPUT_CHARS: usize = 6_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Assembled {
    pub text: String,
    /// Retrieved records actually included, by id.
    pub included: Vec<i64>,
    /// Retrieved records dropped to fit the budget, by id.
    pub dropped: Vec<i64>,
    /// True when the fixed parts alone exceed the budget (they are kept anyway:
    /// constraints are never cut; the caller should raise the budget).
    pub over_budget: bool,
}

/// One record as the model sees it: id, kind, source and date, so every fact
/// carries its provenance.
pub fn line(r: &Record) -> String {
    let date = crate::cost::date_utc(r.created_at);
    let src = r
        .source
        .as_deref()
        .map(|s| format!(" · {s}"))
        .unwrap_or_default();
    format!(
        "[#{} {}{} · {}] {}",
        r.id,
        r.kind.as_str(),
        src,
        date,
        r.body.trim()
    )
}

pub fn assemble(p: &Parts<'_>, budget_chars: usize) -> Assembled {
    let mut head = String::new();
    head.push_str(p.role_block.trim_end());
    head.push_str("\n\n## Project\n");
    for b in p.briefs {
        head.push_str(&line(b));
        head.push('\n');
    }
    if !p.constraints.is_empty() {
        head.push_str("\n## Constraints (binding; ask before breaking one)\n");
        for c in p.constraints {
            head.push_str(&format!("- {}\n", line(c)));
        }
    }
    let h = p.handoff;
    head.push_str(&format!(
        "\n## Handoff #{}\nMission: {}\nDone when: {}\n",
        h.id,
        h.mission.trim(),
        h.acceptance.trim()
    ));
    if !h.scope.is_empty() {
        head.push_str(&format!(
            "Scope (change nothing else): {}\n",
            h.scope.join(", ")
        ));
    }

    for i in p.inputs {
        head.push_str(&format!(
            "
## Input from handoff #{} ({})
{}
",
            i.handoff_id,
            i.mission.trim(),
            clip(i.body.trim(), INPUT_CHARS)
        ));
    }

    let mut tail = String::new();
    if let Some(cp) = p.checkpoint {
        tail.push_str("\n## Where you left off\n");
        tail.push_str(&line(cp));
        tail.push('\n');
    }
    tail.push_str("\n## Now\n");
    tail.push_str(p.instruction.trim());
    tail.push('\n');

    let fixed = head.len() + tail.len();
    let over_budget = fixed > budget_chars;
    let mut room = budget_chars.saturating_sub(fixed);
    let mut middle = String::new();
    let (mut included, mut dropped) = (Vec::new(), Vec::new());
    let header = "\n## Relevant memory\n";
    for r in p.retrieved {
        let l = format!("- {}\n", line(r));
        let need = l.len() + if middle.is_empty() { header.len() } else { 0 };
        if need <= room {
            if middle.is_empty() {
                middle.push_str(header);
            }
            middle.push_str(&l);
            room -= need;
            included.push(r.id);
        } else {
            dropped.push(r.id);
        }
    }
    Assembled {
        text: format!("{head}{middle}{tail}"),
        included,
        dropped,
        over_budget,
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}
[... cut at {max} characters]",
        &s[..end]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Kind, State};

    fn r(id: i64, kind: Kind, body: &str) -> Record {
        Record {
            id,
            kind,
            body: body.into(),
            status: "active".into(),
            supersedes_id: None,
            superseded_by: None,
            reason: None,
            author: "t".into(),
            source: Some("docs/canon.md".into()),
            task_id: None,
            tags: String::new(),
            created_at: 1_790_000_000,
        }
    }

    fn handoff() -> Handoff {
        Handoff {
            id: 7,
            role_id: "builder".into(),
            mission: "add parser tests".into(),
            acceptance: "cargo test parser passes".into(),
            scope: vec!["src/parser.rs".into()],
            depends_on: vec![],
            state: State::Running,
            attempts: 1,
            branch: None,
            model: None,
            role_hash: None,
            prompt_hash: None,
            result_record: None,
            created_at: 0,
            updated_at: 0,
            checks: vec![],
            accept_on_checks: false,
        }
    }

    #[test]
    fn dependency_results_follow_the_handoff_and_are_clipped() {
        let h = handoff();
        let long = "é".repeat(INPUT_CHARS); // two bytes each: the cut lands mid-character
        let inputs = [
            Input {
                handoff_id: 3,
                mission: "duel verbs".into(),
                body: "- Draw
- Feint"
                    .into(),
            },
            Input {
                handoff_id: 4,
                mission: "lore".into(),
                body: long,
            },
        ];
        let a = assemble(
            &Parts {
                role_block: "## Role: Builder",
                briefs: &[],
                constraints: &[],
                handoff: &h,
                retrieved: &[],
                checkpoint: None,
                inputs: &inputs,
                instruction: "Go.",
            },
            50_000,
        );
        let at = |s: &str| a.text.find(s).unwrap_or_else(|| panic!("missing {s}"));
        assert!(at("## Handoff #7") < at("## Input from handoff #3 (duel verbs)"));
        assert!(at("## Input from handoff #3") < at("## Now"));
        assert!(a.text.contains("[... cut at 6000 characters]"));
    }

    #[test]
    fn order_puts_must_not_miss_parts_first_and_last() {
        let briefs = [r(1, Kind::Brief, "Saint's Mile is a frontier JRPG")];
        let cons = [r(2, Kind::Constraint, "never touch the solver crate")];
        let ret = [r(3, Kind::Fact, "ratatui 0.29 removed Frame::size")];
        let cp = r(4, Kind::Checkpoint, "done: lexer; next: parser tests");
        let h = handoff();
        let a = assemble(
            &Parts {
                role_block: "## Role: Builder",
                briefs: &briefs,
                constraints: &cons,
                handoff: &h,
                retrieved: &ret,
                checkpoint: Some(&cp),
                inputs: &[],
                instruction: "Write the tests.",
            },
            10_000,
        );
        let pos = |s: &str| {
            a.text
                .find(s)
                .unwrap_or_else(|| panic!("missing {s}:\n{}", a.text))
        };
        assert!(pos("## Role") < pos("## Project"));
        assert!(pos("## Project") < pos("## Constraints"));
        assert!(pos("## Constraints") < pos("## Handoff #7"));
        assert!(pos("## Handoff #7") < pos("## Relevant memory"));
        assert!(pos("## Relevant memory") < pos("## Where you left off"));
        assert!(pos("## Where you left off") < pos("## Now"));
        assert!(a.text.trim_end().ends_with("Write the tests."));
        assert!(
            a.text.contains("[#3 fact · docs/canon.md · 2026-09-21]"),
            "provenance on every line:\n{}",
            a.text
        );
        assert_eq!(a.included, [3]);
    }

    #[test]
    fn budget_drops_retrieved_records_never_constraints() {
        let cons = [r(2, Kind::Constraint, "never touch the solver crate")];
        let ret: Vec<Record> = (10..20)
            .map(|i| r(i, Kind::Fact, &"x".repeat(200)))
            .collect();
        let h = handoff();
        let with = |retrieved: &[Record], budget: usize| {
            assemble(
                &Parts {
                    role_block: "## Role: Builder",
                    briefs: &[],
                    constraints: &cons,
                    handoff: &h,
                    retrieved,
                    checkpoint: None,
                    inputs: &[],
                    instruction: "Go.",
                },
                budget,
            )
        };
        let fixed = with(&[], usize::MAX).text.len();
        let a = with(&ret, fixed + 700);
        assert!(!a.included.is_empty() && !a.dropped.is_empty(), "{a:?}");
        assert_eq!(a.included.first(), Some(&10), "best matches kept first");
        assert!(a.text.contains("never touch the solver crate"));
        let tiny = with(&ret, 10);
        assert!(tiny.over_budget);
        assert!(
            tiny.text.contains("never touch the solver crate"),
            "constraints survive any budget"
        );
        assert!(tiny.included.is_empty());
    }
}
