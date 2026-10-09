//! `offrig budget` in a terminal: a small menu to raise or lower the project's cap,
//! or stop new spending. Money already given to a run is never taken back: the cap
//! can't go below spent plus committed (`Budget::floor`), so a running plan always
//! keeps its allocation. Scripts and agents (no terminal on stdout) get the plain
//! budget lines, and `offrig budget <USD>` still sets the overall cap directly, under
//! the same floor. The menu manages the overall cap only; per-provider caps are set
//! with `offrig budget --provider runpod|openrouter <USD>`.

use std::io::{BufRead, Write};

use anyhow::Result;
use offrig_core::account::{self, Scope};
use offrig_core::balances::{self, Balance};
use offrig_core::lanes::project_key;
use offrig_core::store::{Budget, CapSource, Provider, Store};

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
///
/// With a `scope`, one `account <provider>` line per provider follows the overall line:
/// what the caps of every known project promise against what the account holds, a
/// WARNING when the unspent part is more than the balance, and a note for each project
/// that could not be read.
pub fn report(
    store: &Store,
    balance: &dyn Fn(Provider) -> Balance,
    scope: Option<&Scope>,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    let mut balances_seen = Vec::new();
    for p in Provider::ALL {
        let b = store.budget_for(p)?;
        let bal = balance(p);
        let own = match store.cap_source(p)? {
            CapSource::Own => "",
            CapSource::Overall => " (overall cap)",
            CapSource::NotSet => " (not set)",
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
        balances_seen.push(bal);
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
    let mut notes = Vec::new();
    if let Some(scope) = scope {
        let (keys, known_notes) = scope.known();
        notes.extend(
            known_notes
                .iter()
                .map(|c| account::registry_note_text(c).to_string()),
        );
        let me = project_key(&scope.current);
        for (p, bal) in Provider::ALL.into_iter().zip(&balances_seen) {
            let t = account::account_totals(p, &keys, Some((&me, store)));
            lines.push(t.line(bal));
            warnings.extend(t.warning(bal));
            for n in t.notes() {
                if !notes.contains(&n) {
                    notes.push(n);
                }
            }
        }
    }
    lines.extend(warnings);
    lines.extend(notes.into_iter().map(|n| format!("note: {n}")));
    Ok(lines)
}

fn num(v: f64) -> serde_json::Value {
    serde_json::Number::from_f64((v * 100.0).round() / 100.0)
        .map_or(serde_json::Value::Null, serde_json::Value::Number)
}

/// The budget report as one versioned JSON object (`offrig budget --show --json`).
/// Numbers are numbers or null, never strings. No path and no free text: `project` is
/// what the caller passes (a folder name, or "."), skipped projects are `<folder>: <code>`
/// (see `ReadSkip::code`), and a balance that is unknown carries a code (`no_key`,
/// `http_error`, `no_number`).
pub fn report_json(
    store: &Store,
    balance: &dyn Fn(Provider) -> Balance,
    scope: Option<&Scope>,
    project: &str,
) -> Result<serde_json::Value> {
    use serde_json::{Value, json};
    let mut providers = serde_json::Map::new();
    let mut account = serde_json::Map::new();
    let (keys, registry_notes) = scope.map_or_else(|| (Vec::new(), Vec::new()), Scope::known);
    let me = scope.map(|s| project_key(&s.current));
    for p in Provider::ALL {
        let b = store.budget_for(p)?;
        let bal = balance(p);
        providers.insert(
            p.as_str().into(),
            json!({
                "cap": num(b.cap),
                "cap_source": store.cap_source(p)?.as_str(),
                "committed": num(b.committed),
                "spent": num(b.spent),
                "remaining": num(b.remaining),
                "balance": bal.known().map_or(Value::Null, num),
                "balance_note": bal.code(),
                "warning": balances::cap_exceeds_balance(p, b.cap, &bal),
            }),
        );
        let a = match (scope, &me) {
            (Some(_), Some(me)) => {
                let t = account::account_totals(p, &keys, Some((me, store)));
                let mut anotes = registry_notes.clone();
                anotes.extend(t.note_codes());
                json!({
                    "caps": num(t.caps),
                    "projects": t.projects,
                    "committed": num(t.committed),
                    "unspent": num(t.unspent),
                    "balance": bal.known().map_or(Value::Null, num),
                    "balance_note": bal.code(),
                    "warning": t.warning(&bal),
                    "notes": anotes,
                })
            }
            _ => json!({
                "caps": null, "projects": null, "committed": null, "unspent": null,
                "balance": bal.known().map_or(Value::Null, num),
                "balance_note": bal.code(), "warning": null,
                "notes": ["account_view_unavailable"],
            }),
        };
        account.insert(p.as_str().into(), a);
    }
    let o = store.budget()?;
    Ok(json!({
        "version": 1,
        "project": project,
        "providers": providers,
        "overall": {
            "cap": num(o.cap),
            "ceiling": store.has_overall_ceiling()?,
            "committed": num(o.committed),
            "spent": num(o.spent),
            "remaining": num(o.remaining),
        },
        "account": account,
        "uncounted": [account::UNCOUNTED_MANUAL],
    }))
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
        let r = report(&s, &bal, None).expect("report");
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
        let r = report(&s, &none, None).expect("report");
        assert!(r[0].contains("(overall cap)"), "{}", r[0]);
        assert!(r[1].contains("balance unknown (OPENROUTER_API_KEY is not set)"));
        assert_eq!(r.len(), 3, "no warning without a known balance");
        s.set_provider_cap(Provider::RunPod, 1.0).expect("rp");
        s.set_provider_cap(Provider::OpenRouter, 1.0).expect("or");
        s.clear_budget_cap().expect("clear");
        let r = report(&s, &none, None).expect("report");
        assert!(
            r[2].contains("no ceiling") && r[2].contains("cap $2.00"),
            "{}",
            r[2]
        );
    }

    #[test]
    fn a_provider_with_no_cap_anywhere_says_not_set_and_a_fallback_says_overall() {
        let d = tempdir::Dir::new();
        let s = Store::open(&d.0.join("offrig.db")).expect("store");
        let none = |_: Provider| Balance::Unknown("no key".into());
        // Nothing set at all: not a fallback, because there is nothing to fall back to.
        let r = report(&s, &none, None).expect("report");
        assert!(
            r[0].contains("cap $0.00") && r[0].contains("(not set)") && !r[0].contains("overall"),
            "{}",
            r[0]
        );
        assert!(r[1].contains("(not set)"), "{}", r[1]);
        // Only a provider cap: the other provider is still not set.
        s.set_provider_cap(Provider::OpenRouter, 5.0).expect("or");
        let r = report(&s, &none, None).expect("report");
        assert!(r[0].contains("(not set)"), "{}", r[0]);
        assert!(!r[1].contains("(not set)") && !r[1].contains("(overall cap)"));
        // An overall cap: the label is true again.
        s.set_budget_cap(20.0).expect("overall");
        let r = report(&s, &none, None).expect("report");
        assert!(
            r[0].contains("(overall cap)") && !r[0].contains("(not set)"),
            "{}",
            r[0]
        );
    }

    #[test]
    fn the_account_lines_follow_the_overall_line_with_a_warning_and_notes() {
        let d = tempdir::Dir::new();
        let mk = |name: &str, cap: f64| {
            let p = d.0.join(name);
            std::fs::create_dir_all(p.join(".offrig")).expect("dir");
            let s = Store::open(&p.join(".offrig").join("offrig.db")).expect("store");
            s.set_provider_cap(Provider::RunPod, cap).expect("rp");
            s.set_provider_cap(Provider::OpenRouter, 1.0).expect("or");
            (p, s)
        };
        let (pa, a) = mk("a", 30.0);
        let (pb, _b) = mk("b", 20.0);
        let scope = Scope {
            current: pa.clone(),
            projects: offrig_core::account::ProjectsRegistry::at(d.0.join("cfg")),
            lanes: offrig_core::lanes::Registry::at(d.0.join("cfg")),
        };
        scope.projects.add(&pb).expect("b");
        scope.projects.add(&d.0.join("gone")).expect("gone");
        let bal = |p: Provider| match p {
            Provider::RunPod => Balance::Known(40.0),
            Provider::OpenRouter => Balance::Known(100.0),
        };
        let r = report(&a, &bal, Some(&scope)).expect("report");
        let acct: Vec<&String> = r.iter().filter(|l| l.starts_with("account ")).collect();
        assert_eq!(acct.len(), 2, "{r:#?}");
        assert!(
            acct[0].contains("account runpod")
                && acct[0].contains("caps $50.00 across 2 projects")
                && acct[0].contains("unspent $50.00")
                && acct[0].contains("balance $40.00"),
            "{}",
            acct[0]
        );
        assert!(
            r.iter().any(|l| l.starts_with(
                "WARNING: runpod caps across projects promise $50.00 unspent but the account holds $40.00"
            )),
            "{r:#?}"
        );
        assert!(
            r.iter()
                .any(|l| l.starts_with("note: project ") && l.contains("no offrig store")),
            "{r:#?}"
        );
        let overall = r
            .iter()
            .position(|l| l.starts_with("overall"))
            .expect("overall");
        let first = r
            .iter()
            .position(|l| l.starts_with("account "))
            .expect("acct");
        assert!(first > overall, "{r:#?}");
    }

    #[test]
    fn the_json_report_parses_and_has_the_versioned_keys_and_no_paths() {
        let d = tempdir::Dir::new();
        let p = d.0.join("a");
        std::fs::create_dir_all(p.join(".offrig")).expect("dir");
        let s = Store::open(&p.join(".offrig").join("offrig.db")).expect("store");
        s.set_provider_cap(Provider::RunPod, 30.0).expect("rp");
        let scope = Scope {
            current: p.clone(),
            projects: offrig_core::account::ProjectsRegistry::at(d.0.join("cfg")),
            lanes: offrig_core::lanes::Registry::at(d.0.join("cfg")),
        };
        scope.projects.add(&d.0.join("gone")).expect("gone");
        let bal = |p: Provider| match p {
            Provider::RunPod => Balance::Known(10.0),
            Provider::OpenRouter => Balance::Unknown("no key".into()),
        };
        let v = report_json(&s, &bal, Some(&scope), "demo").expect("json");
        let text = serde_json::to_string(&v).expect("text");
        let v: serde_json::Value = serde_json::from_str(&text).expect("parses");
        assert_eq!(v["version"], 1);
        assert_eq!(v["project"], "demo");
        for k in [
            "cap",
            "cap_source",
            "committed",
            "spent",
            "remaining",
            "balance",
            "balance_note",
            "warning",
        ] {
            assert!(v["providers"]["runpod"].get(k).is_some(), "{k}");
            assert!(v["providers"]["openrouter"].get(k).is_some(), "{k}");
        }
        for k in ["cap", "ceiling", "committed", "spent", "remaining"] {
            assert!(v["overall"].get(k).is_some(), "{k}");
        }
        for k in [
            "caps",
            "projects",
            "committed",
            "unspent",
            "balance",
            "warning",
            "notes",
        ] {
            assert!(v["account"]["runpod"].get(k).is_some(), "{k}");
            assert!(v["account"]["openrouter"].get(k).is_some(), "{k}");
        }
        assert_eq!(v["providers"]["runpod"]["cap"], 30.0);
        assert_eq!(v["providers"]["runpod"]["cap_source"], "own");
        assert_eq!(v["providers"]["openrouter"]["cap_source"], "not set");
        assert!(v["providers"]["openrouter"]["balance"].is_null());
        assert_eq!(v["providers"]["openrouter"]["balance_note"], "no_key");
        assert!(v["providers"]["runpod"]["warning"].is_string());
        assert_eq!(
            v["uncounted"],
            serde_json::json!([account::UNCOUNTED_MANUAL])
        );
        assert_eq!(v["account"]["runpod"]["notes"][0], "gone: no_store");
        assert_eq!(v["account"]["runpod"]["projects"], 1);
        assert!(v["account"]["runpod"]["caps"].is_number());
        let root = d.0.to_string_lossy().replace('\\', "/").to_lowercase();
        assert!(!text.to_lowercase().contains(&root), "{text}");
        assert!(!text.contains("\\\\"), "{text}");
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
