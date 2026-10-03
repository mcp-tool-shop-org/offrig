//! Deterministic acceptance checks on a handoff's output, decided by code.
//!
//! Evidence (docs/sidecar-design.md, phase 3a): judges favour their own outputs, even
//! on binary rubrics, and self-critique without an external signal makes work worse.
//! So the runner revises only against these checks, and a handoff completes on its own
//! only when they pass and its author said they cover the acceptance check.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case", deny_unknown_fields)]
pub enum Check {
    /// A heading containing `text` (case-insensitive): a Markdown `#` line, or a line
    /// that is bold on its own (`**Text**` or `**Text:**`).
    Heading { text: String },
    /// At least `min` list items directly under the heading containing `heading`.
    Items { heading: String, min: usize },
    /// `text` appears at least `min` times (case-insensitive; `min` defaults to 1).
    Contains {
        text: String,
        #[serde(default = "one")]
        min: usize,
    },
    /// `text` does not appear (case-insensitive).
    Absent { text: String },
    /// The word count is within the bounds given.
    Words {
        #[serde(default)]
        min: Option<usize>,
        #[serde(default)]
        max: Option<usize>,
    },
}

fn one() -> usize {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub check: Check,
    pub pass: bool,
    /// What was found, phrased so a revision turn can act on it.
    pub detail: String,
}

/// Parse checks given as JSON values (the MCP surface), naming the bad one.
pub fn parse(values: &[serde_json::Value]) -> Result<Vec<Check>> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            serde_json::from_value::<Check>(v.clone()).map_err(|e| {
                Error::Refused(format!(
                    "check {} is not valid ({e}); use one of heading{{text}}, items{{heading,min}}, \
                     contains{{text,min?}}, absent{{text}}, words{{min?,max?}}, each with a \"check\" field",
                    i + 1
                ))
            })
        })
        .collect()
}

pub fn evaluate(text: &str, checks: &[Check]) -> Vec<Outcome> {
    checks
        .iter()
        .map(|c| {
            let (pass, detail) = run(text, c);
            Outcome {
                check: c.clone(),
                pass,
                detail,
            }
        })
        .collect()
}

pub fn all_pass(outcomes: &[Outcome]) -> bool {
    outcomes.iter().all(|o| o.pass)
}

/// The failed checks as a list a revision turn can work from.
pub fn feedback(outcomes: &[Outcome]) -> String {
    outcomes
        .iter()
        .filter(|o| !o.pass)
        .map(|o| format!("- {}\n", o.detail))
        .collect()
}

fn run(text: &str, c: &Check) -> (bool, String) {
    let lower = text.to_lowercase();
    match c {
        Check::Heading { text: want } => {
            let found = headings(text)
                .iter()
                .any(|(_, h)| h.to_lowercase().contains(&want.to_lowercase()));
            (
                found,
                if found {
                    format!("heading \"{want}\" present")
                } else {
                    format!("needs a heading containing \"{want}\"; none found")
                },
            )
        }
        Check::Items { heading, min } => match items_under(text, heading) {
            None => (
                false,
                format!(
                    "needs at least {min} list items under a heading containing \"{heading}\"; that heading is missing"
                ),
            ),
            Some(n) => (
                n >= *min,
                if n >= *min {
                    format!("{n} list items under \"{heading}\" (at least {min})")
                } else {
                    format!(
                        "needs at least {min} list items under the heading \"{heading}\"; found {n}"
                    )
                },
            ),
        },
        Check::Contains { text: want, min } => {
            let n = lower.matches(&want.to_lowercase()).count();
            (
                n >= *min,
                if n >= *min {
                    format!("\"{want}\" appears {n} time(s)")
                } else {
                    format!("needs \"{want}\" at least {min} time(s); found {n}")
                },
            )
        }
        Check::Absent { text: want } => {
            let n = lower.matches(&want.to_lowercase()).count();
            (
                n == 0,
                if n == 0 {
                    format!("\"{want}\" absent")
                } else {
                    format!("must not contain \"{want}\"; found {n} time(s)")
                },
            )
        }
        Check::Words { min, max } => {
            let n = text.split_whitespace().count();
            let low = min.is_none_or(|m| n >= m);
            let high = max.is_none_or(|m| n <= m);
            let bounds = match (min, max) {
                (Some(a), Some(b)) => format!("{a} to {b}"),
                (Some(a), None) => format!("at least {a}"),
                (None, Some(b)) => format!("at most {b}"),
                (None, None) => "any number of".into(),
            };
            (
                low && high,
                if low && high {
                    format!("{n} words ({bounds})")
                } else {
                    format!("needs {bounds} words; has {n}")
                },
            )
        }
    }
}

/// Heading lines as (line index, heading text).
fn headings(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter_map(|(i, l)| heading_text(l).map(|h| (i, h)))
        .collect()
}

fn heading_text(line: &str) -> Option<String> {
    let t = line.trim();
    if let Some(rest) = t.strip_prefix('#') {
        return Some(rest.trim_start_matches('#').trim().to_string());
    }
    // `**Text**`, `**Text:**` or `**Text**:`
    let inner = t.strip_prefix("**")?;
    let inner = inner
        .strip_suffix("**:")
        .or_else(|| inner.strip_suffix("**"))?;
    let inner = inner.strip_suffix(':').unwrap_or(inner);
    (!inner.is_empty() && !inner.contains("**")).then(|| inner.trim().to_string())
}

/// List items directly under the first heading containing `heading`, up to the next
/// heading. Only items at the shallowest indent count, so sub-bullets do not inflate.
fn items_under(text: &str, heading: &str) -> Option<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let want = heading.to_lowercase();
    let start = headings(text)
        .into_iter()
        .find(|(_, h)| h.to_lowercase().contains(&want))?
        .0;
    let section = lines[start + 1..]
        .iter()
        .take_while(|l| heading_text(l).is_none());
    let indents: Vec<usize> = section
        .filter(|l| is_item(l))
        .map(|l| l.len() - l.trim_start().len())
        .collect();
    let shallowest = indents.iter().copied().min().unwrap_or(0);
    Some(indents.iter().filter(|&&i| i == shallowest).count())
}

fn is_item(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SPEC: &str = "# Standoff duel\n\nIntro text.\n\n## Verbs\n- Draw\n  - quick draw\n- Feint\n- Hold\n\n**Failure states:**\n1. Flinch\n2) Overcommit\n";

    fn one_check(c: Check) -> Outcome {
        evaluate(SPEC, &[c]).remove(0)
    }

    #[test]
    fn headings_are_markdown_or_bold_lines() {
        assert!(
            one_check(Check::Heading {
                text: "verbs".into()
            })
            .pass
        );
        assert!(
            one_check(Check::Heading {
                text: "Failure states".into()
            })
            .pass
        );
        let miss = one_check(Check::Heading {
            text: "Rewards".into(),
        });
        assert!(!miss.pass);
        assert!(miss.detail.contains("needs a heading"), "{}", miss.detail);
        assert_eq!(
            heading_text("**bold** words after"),
            None,
            "inline bold is not a heading"
        );
    }

    #[test]
    fn items_count_only_the_top_level_under_their_heading() {
        let v = one_check(Check::Items {
            heading: "Verbs".into(),
            min: 3,
        });
        assert!(v.pass, "{}", v.detail);
        assert!(
            v.detail.starts_with("3 list items"),
            "sub-bullet not counted: {}",
            v.detail
        );
        let f = one_check(Check::Items {
            heading: "Failure".into(),
            min: 3,
        });
        assert!(!f.pass);
        assert!(
            f.detail.contains("found 2"),
            "numbered items count: {}",
            f.detail
        );
        let gone = one_check(Check::Items {
            heading: "Rewards".into(),
            min: 1,
        });
        assert!(gone.detail.contains("heading is missing"));
    }

    #[test]
    fn contains_absent_and_words() {
        assert!(
            one_check(Check::Contains {
                text: "OVERCOMMIT".into(),
                min: 1
            })
            .pass
        );
        assert!(
            !one_check(Check::Contains {
                text: "draw".into(),
                min: 3
            })
            .pass
        );
        assert!(
            one_check(Check::Absent {
                text: "lorem".into()
            })
            .pass
        );
        assert!(
            !one_check(Check::Absent {
                text: "feint".into()
            })
            .pass
        );
        assert!(
            one_check(Check::Words {
                min: Some(10),
                max: Some(40)
            })
            .pass
        );
        let long = one_check(Check::Words {
            min: None,
            max: Some(5),
        });
        assert!(!long.pass && long.detail.starts_with("needs at most 5 words"));
    }

    #[test]
    fn feedback_lists_only_failures() {
        let out = evaluate(
            SPEC,
            &[
                Check::Heading {
                    text: "Verbs".into(),
                },
                Check::Items {
                    heading: "Failure".into(),
                    min: 3,
                },
            ],
        );
        assert!(!all_pass(&out));
        let fb = feedback(&out);
        assert_eq!(fb.lines().count(), 1);
        assert!(fb.contains("found 2"));
    }

    #[test]
    fn parse_names_the_bad_check() {
        let ok = parse(&[
            json!({"check": "items", "heading": "Verbs", "min": 3}),
            json!({"check": "contains", "text": "x"}),
        ])
        .expect("valid");
        assert_eq!(
            ok[1],
            Check::Contains {
                text: "x".into(),
                min: 1
            }
        );
        let err = parse(&[
            json!({"check": "heading", "text": "a"}),
            json!({"check": "itmes", "min": 2}),
        ])
        .expect_err("typo");
        assert!(err.to_string().contains("check 2"), "{err}");
        assert!(
            parse(&[json!({"check": "words", "max": 5, "extra": 1})]).is_err(),
            "unknown fields refused"
        );
    }
}
