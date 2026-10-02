//! Role blocks for handoffs, rendered from Role OS data.
//!
//! Role OS is the source of truth: a role's built dossier (`dossier/examples/<id>.json`,
//! schema `roleos-dossier/v0.2`) gives its charter, aptitudes (0-5, disposition
//! applied) and operating profile (voice and behaviour text); its starter-pack card
//! (`starter-pack/agents/<area>/<id>.md`) gives required output, quality bar and
//! escalation triggers. Game roles Role OS lacks ship here in the same two formats so
//! they can move upstream unchanged.
//!
//! Evidence (docs/sidecar-design.md): roles shape focus and style, not accuracy, so the
//! block renders concrete behaviours, never identity claims, and correctness stays with
//! the handoff's acceptance check. Agreeable roles drift into sycophancy on small open
//! models, so every block carries an explicit candor line.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Aptitudes {
    pub rigor: i32,
    pub pace: i32,
    pub range: i32,
    pub skepticism: i32,
    pub autonomy: i32,
    pub candor: i32,
}

/// Role OS writes this object's keys in snake_case (`prompt_delta`) although the
/// dossier around it is camelCase (`operatingProfile`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OperatingProfile {
    pub active: String,
    #[serde(default)]
    pub prompt_delta: String,
    #[serde(default)]
    pub voice: String,
}

/// The fields of a Role OS dossier that a role block uses.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dossier {
    pub id: String,
    pub role: String,
    #[serde(default)]
    pub specialization: String,
    #[serde(default)]
    pub archetype: String,
    pub aptitudes: Aptitudes,
    #[serde(default)]
    pub ideal_source: String,
    pub operating_profile: OperatingProfile,
    pub charter: String,
}

/// The sections of a starter-pack card that a role block uses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Card {
    pub mission: String,
    pub required_output: Vec<String>,
    pub quality_bar: Vec<String>,
    pub escalation: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Role {
    pub dossier: Dossier,
    pub card: Card,
    /// Where it came from: `role-os:<dir>` or `offrig:builtin`.
    pub origin: String,
}

/// Game roles Role OS does not have yet, in Role OS's own formats.
const BUILTIN: &[(&str, &str, &str)] = &[
    (
        "game-designer",
        include_str!("../roles/game-designer.json"),
        include_str!("../roles/game-designer.md"),
    ),
    (
        "systems-designer",
        include_str!("../roles/systems-designer.json"),
        include_str!("../roles/systems-designer.md"),
    ),
    (
        "narrative-designer",
        include_str!("../roles/narrative-designer.json"),
        include_str!("../roles/narrative-designer.md"),
    ),
    (
        "lore-keeper",
        include_str!("../roles/lore-keeper.json"),
        include_str!("../roles/lore-keeper.md"),
    ),
];

pub fn builtin_ids() -> Vec<&'static str> {
    BUILTIN.iter().map(|(id, _, _)| *id).collect()
}

/// Parse a starter-pack card: `## Mission` paragraph plus bullet sections.
pub fn parse_card(md: &str) -> Card {
    let mut card = Card::default();
    let mut section = String::new();
    for line in md.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            section = h.trim().to_ascii_lowercase();
            continue;
        }
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let bullet = t.strip_prefix("- ").map(str::trim);
        match (section.as_str(), bullet) {
            ("mission", _) => {
                if !card.mission.is_empty() {
                    card.mission.push(' ');
                }
                card.mission.push_str(bullet.unwrap_or(t));
            }
            ("required output", Some(b)) => card.required_output.push(b.to_string()),
            ("quality bar", Some(b)) => card.quality_bar.push(b.to_string()),
            ("escalation triggers", Some(b)) => card.escalation.push(b.to_string()),
            _ => {}
        }
    }
    card
}

fn parse_dossier(json: &str, what: &str) -> Result<Dossier> {
    serde_json::from_str(json).map_err(|e| Error::decode(what, e))
}

/// Find a role: Role OS first (when its directory is given and has the role), then
/// offrig's built-in game roles.
pub fn load(id: &str, role_os_dir: Option<&Path>) -> Result<Role> {
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || id.is_empty()
    {
        return Err(Error::Refused(format!(
            "{id:?} is not a role id (lowercase, digits, dashes)"
        )));
    }
    if let Some(dir) = role_os_dir
        && let Some(role) = load_role_os(id, dir)?
    {
        return Ok(role);
    }
    if let Some((_, json, md)) = BUILTIN.iter().find(|(b, _, _)| *b == id) {
        return Ok(Role {
            dossier: parse_dossier(json, &format!("built-in role {id}"))?,
            card: parse_card(md),
            origin: "offrig:builtin".into(),
        });
    }
    Err(Error::Refused(format!(
        "no role {id:?}; use a Role OS role id or one of: {}",
        builtin_ids().join(", ")
    )))
}

fn load_role_os(id: &str, dir: &Path) -> Result<Option<Role>> {
    let dossier_path = dir
        .join("dossier")
        .join("examples")
        .join(format!("{id}.json"));
    let json = match std::fs::read_to_string(&dossier_path) {
        Ok(j) => j,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(format!("reading {}", dossier_path.display()), e)),
    };
    let dossier = parse_dossier(&json, &dossier_path.display().to_string())?;
    let card = find_card(&dir.join("starter-pack").join("agents"), id)
        .map(|p| std::fs::read_to_string(&p).map(|md| parse_card(&md)))
        .transpose()
        .map_err(|e| Error::io(format!("reading the card for {id}"), e))?
        .unwrap_or_default();
    Ok(Some(Role {
        dossier,
        card,
        origin: format!("role-os:{}", dir.display()),
    }))
}

fn find_card(agents: &Path, id: &str) -> Option<PathBuf> {
    std::fs::read_dir(agents).ok()?.flatten().find_map(|area| {
        let p = area.path().join(format!("{id}.md"));
        p.is_file().then_some(p)
    })
}

/// Concrete behaviours for the extreme ends of each axis (Role OS's `maps_to`
/// meanings). Middle scores add nothing, which keeps blocks short.
pub fn behaviours(a: &Aptitudes) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut push = |score: i32, high: &'static str, low: &'static str| {
        if score >= 4 {
            out.push(high);
        } else if score <= 1 {
            out.push(low);
        }
    };
    push(
        a.rigor,
        "Demand evidence before accepting work: check each claim against the code, test or source before relying on it.",
        "Accept work once it plausibly meets the brief; spend effort on progress, not re-verification.",
    );
    push(
        a.pace,
        "Work in short, complete steps and reach a usable result early.",
        "Work deliberately: take extra passes rather than rushing to an answer.",
    );
    push(
        a.range,
        "Before committing, lay out two or three distinct approaches and say why you chose one.",
        "Execute the brief as given; do not widen the scope or invent alternatives.",
    );
    push(
        a.skepticism,
        "Challenge claims before crediting them, your own included, and name what would prove you wrong.",
        "Trust the provided context and contracts unless something plainly contradicts them.",
    );
    push(
        a.autonomy,
        "Run the task to completion without asking; record each judgement call as a decision.",
        "Stop and escalate at real ambiguity instead of guessing.",
    );
    push(
        a.candor,
        "State problems plainly and contrastively (\"X holds, but Y fails because...\"); never soften a failing result.",
        "Report results briefly, without commentary.",
    );
    out
}

/// The role block that opens every handoff.
pub fn render(role: &Role) -> String {
    let d = &role.dossier;
    let mut s = String::new();
    let title = if d.specialization.is_empty() {
        d.role.clone()
    } else {
        format!("{} ({})", d.role, d.specialization)
    };
    s.push_str(&format!("## Role: {title}\n"));
    s.push_str(&format!("Charter: {}\n", d.charter.trim()));
    if !d.operating_profile.voice.is_empty() {
        s.push_str(&format!(
            "Voice: \"{}\"\n",
            d.operating_profile.voice.trim()
        ));
    }
    s.push_str("How you work:\n");
    if !d.operating_profile.prompt_delta.is_empty() {
        s.push_str(&format!("- {}\n", d.operating_profile.prompt_delta.trim()));
    }
    for b in behaviours(&d.aptitudes) {
        s.push_str(&format!("- {b}\n"));
    }
    s.push_str("- If the evidence is weak or the task is ambiguous, say so plainly instead of guessing or agreeing.\n");
    let list = |s: &mut String, head: &str, items: &[String]| {
        if !items.is_empty() {
            s.push_str(&format!("{head}:\n"));
            for i in items {
                s.push_str(&format!("- {i}\n"));
            }
        }
    };
    list(&mut s, "Required output", &role.card.required_output);
    list(&mut s, "Quality bar", &role.card.quality_bar);
    list(&mut s, "Escalate when", &role.card.escalation);
    s
}

/// FNV-1a 64-bit, hex: a stable fingerprint of a rendered block for the handoff row.
pub fn fingerprint(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_game_role_parses_and_renders_fully() {
        for id in builtin_ids() {
            let r = load(id, None).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(r.dossier.id, id);
            assert!(
                r.dossier.ideal_source.contains("awaiting Role OS"),
                "{id} must not claim panel tuning"
            );
            assert!(
                !r.dossier.operating_profile.prompt_delta.is_empty(),
                "{id} lost its disposition text"
            );
            assert!(
                render(&r).contains(r.dossier.operating_profile.prompt_delta.trim()),
                "{id} block omits it"
            );
            assert!(
                !r.card.required_output.is_empty()
                    && !r.card.quality_bar.is_empty()
                    && !r.card.escalation.is_empty(),
                "{id} card"
            );
            let block = render(&r);
            for part in [
                "## Role:",
                "Charter:",
                "Voice:",
                "How you work:",
                "Required output:",
                "Quality bar:",
                "Escalate when:",
            ] {
                assert!(block.contains(part), "{id} block lacks {part}:\n{block}");
            }
            for a in [&r.dossier.aptitudes] {
                for v in [a.rigor, a.pace, a.range, a.skepticism, a.autonomy, a.candor] {
                    assert!((0..=5).contains(&v), "{id} aptitude {v} out of 0-5");
                }
            }
        }
    }

    #[test]
    fn blocks_state_behaviours_not_identity() {
        let r = load("lore-keeper", None).expect("builtin");
        let block = render(&r);
        assert!(
            !block.contains("You are "),
            "identity claims do not raise accuracy; render behaviours"
        );
        assert!(
            block.contains("say so plainly instead of guessing"),
            "candor guardrail is always present"
        );
        assert!(block.contains("Demand evidence"), "lore keeper is rigor 5");
    }

    #[test]
    fn only_extreme_scores_add_lines() {
        let mid = Aptitudes {
            rigor: 3,
            pace: 3,
            range: 2,
            skepticism: 3,
            autonomy: 2,
            candor: 3,
        };
        assert!(behaviours(&mid).is_empty());
        let edges = Aptitudes {
            rigor: 5,
            pace: 0,
            range: 4,
            skepticism: 1,
            autonomy: 5,
            candor: 4,
        };
        assert_eq!(behaviours(&edges).len(), 6);
    }

    #[test]
    fn unknown_and_malformed_role_ids_are_refused() {
        assert!(load("no-such-role", None).is_err());
        assert!(load("../etc/passwd", None).is_err());
        assert!(load("", None).is_err());
    }

    #[test]
    fn role_os_layout_is_read_when_present() {
        let dir = std::env::temp_dir().join(format!("offrig-roleos-{}", std::process::id()));
        let ex = dir.join("dossier").join("examples");
        let agents = dir.join("starter-pack").join("agents").join("engineering");
        std::fs::create_dir_all(&ex).expect("dirs");
        std::fs::create_dir_all(&agents).expect("dirs");
        std::fs::write(
            ex.join("backend-engineer.json"),
            r#"{"schema":"roleos-dossier/v0.2","id":"backend-engineer","role":"Backend Engineer",
               "specialization":"Server-side Systems","archetype":"builder",
               "aptitudes":{"rigor":4,"pace":5,"range":3,"skepticism":2,"autonomy":4,"candor":3},
               "idealSource":"panel-tuned",
               "operatingProfile":{"active":"Builder","prompt_delta":"Bias toward a working artifact.","voice":"Working version first."},
               "charter":"Implement reliable server-side behavior."}"#,
        )
        .expect("dossier");
        std::fs::write(
            agents.join("backend-engineer.md"),
            "# Backend Engineer\n\n## Mission\nImplement server behavior.\n\n## Required Output\n- Code\n- Tests\n\n## Quality Bar\n- Tests pass\n\n## Escalation Triggers\n- Contract unclear\n",
        )
        .expect("card");
        let r = load("backend-engineer", Some(&dir)).expect("role os role");
        assert!(r.origin.starts_with("role-os:"));
        assert_eq!(r.card.required_output, ["Code", "Tests"]);
        let block = render(&r);
        assert!(block.contains("Bias toward a working artifact."));
        assert!(block.contains("Work in short, complete steps"), "pace 5");
        // A game role still resolves from the built-ins when Role OS lacks it.
        assert_eq!(
            load("game-designer", Some(&dir)).expect("builtin").origin,
            "offrig:builtin"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every real Role OS dossier must load and render. Skips where Role OS is not
    /// checked out (CI); on the studio rig it proves the format against live data.
    #[test]
    fn every_real_role_os_dossier_renders() {
        let Some(dir) = crate::config::default_role_os_dir().map(PathBuf::from) else {
            return;
        };
        let Ok(rd) = std::fs::read_dir(dir.join("dossier").join("examples")) else {
            return;
        };
        let mut n = 0;
        for entry in rd.flatten() {
            let id = entry.path().file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let r = load(&id, Some(&dir)).unwrap_or_else(|e| panic!("{id}: {e}"));
            let block = render(&r);
            assert!(block.contains("Charter:"), "{id}");
            assert!(
                r.dossier.operating_profile.prompt_delta.is_empty() || block.contains(r.dossier.operating_profile.prompt_delta.trim()),
                "{id} lost its disposition text"
            );
            n += 1;
        }
        assert!(n > 0, "Role OS checkout has no dossiers");
    }

    #[test]
    fn fingerprint_is_stable_and_sensitive() {
        assert_eq!(fingerprint("abc"), fingerprint("abc"));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
        assert_eq!(fingerprint(""), "cbf29ce484222325");
    }
}
