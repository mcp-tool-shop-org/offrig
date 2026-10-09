//! `offrig budget` in a terminal: a small menu to raise or lower the project's cap,
//! or stop new spending. Money already given to a run is never taken back: the cap
//! can't go below spent plus committed (`Budget::floor`), so a running plan always
//! keeps its allocation. Scripts and agents (no terminal on stdout) get the plain
//! budget lines, and `offrig budget <USD>` still sets the overall cap directly, under
//! the same floor. The menu manages the overall cap only; per-provider caps are set
//! with `offrig budget --provider runpod|openrouter <USD>`.

use std::io::{BufRead, Write};

use anyhow::Result;
use offrig_core::balances::{self, Balance};
use offrig_core::store::{Budget, Provider, Store};

/// One line: cap, committed, spent, remaining.
pub fn line(b: &Budget) -> String {
    format!(
        "cap ${:.2}  committed ${:.2}  spent ${:.2}  remaining ${:.2}",
        b.cap, b.committed, b.spent, b.remaining
    )
}

/// The budget report: one line per provider (cap, committed, spent, remaining and the
/// balance the provider itself reports), the overall line, then a WARNING line for each
/// provider whose cap is above its reported balance. `balance` is called once per
/// provider and is best effort: an unknown balance is shown with its reason.
pub fn report(store: &Store, balance: &dyn Fn(Provider) -> Balance) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    for p in Provider::ALL {
        let b = store.budget_for(p)?;
        let bal = balance(p);
        let own = if store.provider_cap_is_explicit(p)? {
            ""
        } else {
            " (overall cap)"
        };
        lines.push(format!(
            "{:<10}  {}{own}  balance {}",
            p.as_str(),
            line(&b),
            bal.text()
        ));
        if let Some(w) = balances::cap_exceeds_balance(p, b.cap, &bal) {
            warnings.push(w);
        }
    }
    let kind = if store.has_overall_ceiling()? {
        ""
    } else {
        " (no ceiling; sum of provider caps)"
    };
    lines.push(format!(
        "{:<10}  {}{kind}",
        "overall",
        line(&store.budget()?)
    ));
    lines.extend(warnings);
    Ok(lines)
}

/// Run the menu until the person quits (or input ends). `project` is a display name
/// only, never a path.
pub fn run(
    store: &Store,
    project: &str,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<()> {
    loop {
        let b = store.budget()?;
        writeln!(out)?;
        writeln!(out, "offrig budget: {project}")?;
        writeln!(out, "  {}", line(&b))?;
        let open = store.open_plans()?;
        for p in &open {
            writeln!(
                out,
                "  live: plan {} ({}), worst case ${:.2}",
                p.id, p.profile, p.worst_case
            )?;
        }
        writeln!(out)?;
        writeln!(out, "  1) Set a new cap")?;
        writeln!(
            out,
            "  2) Stop new spending: lower the cap to ${:.2} (spent + committed)",
            b.floor()
        )?;
        writeln!(out, "  3) Quit")?;
        write!(out, "Choose [1-3, Enter quits]: ")?;
        out.flush()?;
        let Some(choice) = read(input)? else {
            return Ok(());
        };
        match choice.as_str() {
            "1" => set_new(store, &b, open.len(), input, out)?,
            "2" => {
                let floor = b.floor();
                if confirm(input, out, &format!("Lower the cap to ${floor:.2}?"))? {
                    store.set_budget_cap(floor)?;
                    writeln!(
                        out,
                        "Cap set to ${floor:.2}. Nothing is left for a new paid session."
                    )?;
                    live_note(out, open.len())?;
                } else {
                    writeln!(out, "Unchanged.")?;
                }
            }
            "" | "3" | "q" | "Q" => return Ok(()),
            other => writeln!(out, "{other:?} isn't a choice; pick 1, 2 or 3.")?,
        }
    }
}

fn set_new(
    store: &Store,
    b: &Budget,
    live: usize,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<()> {
    write!(out, "New cap in USD (now ${:.2}): ", b.cap)?;
    out.flush()?;
    let Some(text) = read(input)? else {
        return Ok(());
    };
    let cap = match parse_usd(&text) {
        Some(c) => c,
        None => {
            writeln!(out, "{text:?} isn't an amount; nothing changed.")?;
            return Ok(());
        }
    };
    if cap + 1e-9 < b.floor() {
        writeln!(
            out,
            "The cap can't go below ${:.2}: ${:.2} is spent and ${:.2} is committed to running plans. Nothing changed.",
            b.floor(),
            b.spent,
            b.committed
        )?;
        return Ok(());
    }
    if confirm(input, out, &format!("Set the cap to ${cap:.2}?"))? {
        store.set_budget_cap(cap)?;
        writeln!(out, "Cap set to ${cap:.2}.")?;
        if cap < b.cap {
            live_note(out, live)?;
        }
    } else {
        writeln!(out, "Unchanged.")?;
    }
    Ok(())
}

/// A lower cap stops new plans; it never stops one already running.
fn live_note(out: &mut impl Write, live: usize) -> Result<()> {
    if live > 0 {
        writeln!(
            out,
            "Running plans keep the money they were given and go on to their deadline or shutdown. To stop a pod now: offrig down, or offrig_shutdown from its session."
        )?;
    }
    Ok(())
}

/// A dollar amount: digits with an optional `$` and decimal point, never negative.
pub fn parse_usd(text: &str) -> Option<f64> {
    let t = text.trim().trim_start_matches('$').trim();
    let v: f64 = t.parse().ok()?;
    (v.is_finite() && v >= 0.0).then_some(v)
}

fn confirm(input: &mut impl BufRead, out: &mut impl Write, question: &str) -> Result<bool> {
    write!(out, "{question} [y/N]: ")?;
    out.flush()?;
    Ok(matches!(
        read(input)?.as_deref(),
        Some("y") | Some("Y") | Some("yes")
    ))
}

/// One trimmed line, or None at end of input.
fn read(input: &mut impl BufRead) -> Result<Option<String>> {
    let mut s = String::new();
    if input.read_line(&mut s)? == 0 {
        return Ok(None);
    }
    Ok(Some(s.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempdir::Dir, Store) {
        let d = tempdir::Dir::new();
        let s = Store::open(&d.0.join("offrig.db")).expect("store");
        s.set_budget_cap(50.0).expect("cap");
        (d, s)
    }

    fn menu(s: &Store, keys: &str) -> String {
        let mut out = Vec::new();
        run(s, "demo", &mut keys.as_bytes(), &mut out).expect("menu");
        String::from_utf8(out).expect("utf8")
    }

    #[test]
    fn a_new_cap_is_set_only_after_a_yes() {
        let (_d, s) = store();
        let text = menu(&s, "1\n80\ny\n3\n");
        assert_eq!(s.budget().expect("b").cap, 80.0);
        assert!(text.contains("Cap set to $80.00."), "{text}");
        let text = menu(&s, "1\n20\nn\n\n");
        assert_eq!(s.budget().expect("b").cap, 80.0, "a no changes nothing");
        assert!(text.contains("Unchanged."), "{text}");
    }

    #[test]
    fn stopping_new_spending_lowers_the_cap_to_the_floor() {
        let (_d, s) = store();
        let text = menu(
            &s, "2
y
",
        );
        assert_eq!(
            s.budget().expect("b").cap,
            0.0,
            "nothing spent or committed: floor 0"
        );
        assert!(
            text.contains("Nothing is left for a new paid session"),
            "{text}"
        );
        assert!(text.contains("offrig budget: demo"));
        assert!(text.contains("cap $0.00"), "the menu shows the new cap");
    }

    #[test]
    fn a_running_plan_keeps_its_money() {
        let (_d, s) = store();
        let p = s
            .create_plan(offrig_core::store::NewPlan {
                profile: "job".into(),
                gpu_count: 1,
                gpu_types: vec![],
                max_hours: 4.0,
                max_price_hr: 2.5,
                note: None,
            })
            .expect("plan");
        s.commit_plan(p.id).expect("commit");
        let text = menu(
            &s, "1
3
2
y
",
        );
        assert!(text.contains("can't go below $10.00"), "{text}");
        assert!(text.contains("live: plan"), "{text}");
        assert!(text.contains("keep the money they were given"), "{text}");
        let b = s.budget().expect("b");
        assert_eq!(b.cap, 10.0, "stopped at the floor, not zero");
        assert!(
            (b.committed - 10.0).abs() < 1e-9 && b.remaining.abs() < 1e-9,
            "{b:?}"
        );
    }

    #[test]
    fn bad_input_changes_nothing() {
        let (_d, s) = store();
        let text = menu(&s, "9\n1\n-5\n1\nlots\n1\n");
        assert_eq!(s.budget().expect("b").cap, 50.0);
        assert!(text.contains("isn't a choice"), "{text}");
        assert!(text.contains("\"-5\" isn't an amount"), "{text}");
        assert!(text.contains("\"lots\" isn't an amount"), "{text}");
    }

    #[test]
    fn end_of_input_quits_cleanly() {
        let (_d, s) = store();
        menu(&s, "");
        menu(&s, "1\n");
        menu(&s, "1\n70\n");
        assert_eq!(
            s.budget().expect("b").cap,
            50.0,
            "no confirmation, no change"
        );
    }

    #[test]
    fn the_report_has_a_line_per_provider_then_the_overall_and_warns_above_the_balance() {
        let (_d, s) = store();
        s.set_provider_cap(Provider::RunPod, 30.0).expect("rp");
        s.set_provider_cap(Provider::OpenRouter, 20.0).expect("or");
        let bal = |p: Provider| match p {
            Provider::RunPod => Balance::Known(31.1),
            Provider::OpenRouter => Balance::Known(8.25),
        };
        let r = report(&s, &bal).expect("report");
        assert_eq!(r.len(), 4, "{r:#?}");
        assert!(
            r[0].starts_with("runpod") && r[0].contains("cap $30.00"),
            "{}",
            r[0]
        );
        assert!(r[0].contains("balance $31.10") && !r[0].contains("overall cap"));
        assert!(r[1].starts_with("openrouter") && r[1].contains("balance $8.25"));
        assert!(r[2].starts_with("overall") && r[2].contains("cap $50.00"));
        assert!(
            r[3].starts_with("WARNING") && r[3].contains("openrouter"),
            "{}",
            r[3]
        );
        assert!(
            !r.iter()
                .any(|l| l.starts_with("WARNING") && l.contains("runpod"))
        );
    }

    #[test]
    fn an_unknown_balance_shows_why_and_a_fallback_cap_says_so() {
        let (_d, s) = store();
        let none = |_: Provider| Balance::Unknown("OPENROUTER_API_KEY is not set".into());
        let r = report(&s, &none).expect("report");
        assert!(r[0].contains("(overall cap)"), "{}", r[0]);
        assert!(r[1].contains("balance unknown (OPENROUTER_API_KEY is not set)"));
        assert_eq!(r.len(), 3, "no warning without a known balance");
        s.set_provider_cap(Provider::RunPod, 1.0).expect("rp");
        s.set_provider_cap(Provider::OpenRouter, 1.0).expect("or");
        s.clear_budget_cap().expect("clear");
        let r = report(&s, &none).expect("report");
        assert!(
            r[2].contains("no ceiling") && r[2].contains("cap $2.00"),
            "{}",
            r[2]
        );
    }

    #[test]
    fn amounts_parse_with_or_without_a_dollar_sign() {
        assert_eq!(parse_usd("$12.50"), Some(12.5));
        assert_eq!(parse_usd(" 85 "), Some(85.0));
        assert_eq!(parse_usd("0"), Some(0.0));
        assert_eq!(parse_usd("-1"), None);
        assert_eq!(parse_usd("NaN"), None);
        assert_eq!(parse_usd("inf"), None);
        assert_eq!(parse_usd(""), None);
    }

    /// A temp directory removed on drop, without a new dependency.
    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                use std::sync::atomic::{AtomicU32, Ordering};
                static N: AtomicU32 = AtomicU32::new(0);
                let p = std::env::temp_dir().join(format!(
                    "offrig-budget-menu-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&p).expect("temp dir");
                Dir(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
