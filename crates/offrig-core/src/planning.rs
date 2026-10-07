//! What a plan may rent. `offrig_plan` reads the live offers and narrows the profile's
//! GPU list by price, memory and fallback; the result is stored on the plan, and the
//! launch rents only from it (issues #9 and #10). This is pure: offers in, choice out.

use crate::config::Profile;
use crate::error::{Error, Result};
use crate::runpod::GpuOffer;

/// What the caller asked of one plan, beyond the profile's own `min_vram_gb`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Limits {
    /// Total $/hr for the pod (all GPUs), the same unit `offrig_offers` prints. Offers
    /// above it are never chosen, and the worst case is priced from it.
    pub max_price_hr: Option<f64>,
    /// Rent only the profile's first GPU family; never fall back to the later ones.
    pub no_fallback: bool,
}

/// The GPU list a plan will rent from, and the price it is held to.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// Priority order, as in the profile.
    pub gpu_types: Vec<String>,
    /// `min(max_price_hr, highest listed price among gpu_types)`, or just the highest
    /// listed price with no cap. The plan's worst case is this times its hours.
    pub max_price_hr: f64,
    /// The dearest price listed among the remaining types, before any cap.
    pub highest_listed: f64,
    /// Every type left out, and why.
    pub dropped: Vec<(String, String)>,
}

/// The GPU family of a type id: the id without `NVIDIA ` and without an edition suffix,
/// so the two RTX PRO 6000 Blackwell editions (listed as a pair) are one family. The
/// A100 SXM and PCIe cards are different families on purpose: they are different
/// hosts (NVLink board against a PCIe card), and `no_fallback` is about not changing
/// the hardware.
pub fn gpu_family(id: &str) -> String {
    let mut s = id.strip_prefix("NVIDIA ").unwrap_or(id).trim().to_string();
    for suffix in [
        "Max-Q Workstation Edition",
        "Workstation Edition",
        "Server Edition",
    ] {
        if let Some(head) = s.strip_suffix(suffix) {
            s = head.trim().to_string();
            break;
        }
    }
    s
}

/// Narrow a profile's GPU list by the limits and the live offers, or refuse with the
/// reason every type was left out.
pub fn choose(profile: &Profile, offers: &[GpuOffer], limits: &Limits) -> Result<Choice> {
    if let Some(cap) = limits.max_price_hr
        && !(cap > 0.0 && cap.is_finite())
    {
        return Err(Error::Refused(
            "max_price_hr must be a positive $/hr; read it from offrig_offers".into(),
        ));
    }
    let offer = |id: &str| offers.iter().find(|o| o.id == id);
    let mut dropped: Vec<(String, String)> = Vec::new();
    let mut types: Vec<String> = profile.gpu_type_ids.clone();

    if limits.no_fallback
        && let Some(first) = types.first().map(|t| gpu_family(t))
    {
        narrow(&mut types, &mut dropped, |t| {
            (gpu_family(t) != first).then(|| format!("fallback is off; only {first} is allowed"))
        });
    }
    if let Some(min) = profile.min_vram_gb {
        narrow(&mut types, &mut dropped, |t| match offer(t) {
            None => Some("RunPod lists no memory for it, so min_vram_gb cannot be met".into()),
            Some(o) if o.total_vram_gb() < min => Some(format!(
                "{} GB in total is below the profile's min_vram_gb {min}",
                o.total_vram_gb()
            )),
            Some(_) => None,
        });
    }
    if let Some(cap) = limits.max_price_hr {
        narrow(&mut types, &mut dropped, |t| {
            match offer(t).and_then(|o| o.price_per_hr) {
                None => Some(
                    "no price is listed (none free now), so it cannot be held to max_price_hr"
                        .into(),
                ),
                Some(p) if p > cap + 1e-9 => {
                    Some(format!("${p:.2}/hr is above max_price_hr ${cap:.2}"))
                }
                Some(_) => None,
            }
        });
    }

    if types.is_empty() {
        let reasons: Vec<String> = dropped.iter().map(|(t, w)| format!("{t}: {w}")).collect();
        return Err(Error::Refused(format!(
            "no GPU type of profile {} is left after the limits ({})",
            profile.name,
            reasons.join("; ")
        )));
    }
    let highest = types
        .iter()
        .filter_map(|t| offer(t).and_then(|o| o.price_per_hr))
        .reduce(f64::max);
    let Some(highest) = highest else {
        return Err(Error::Refused(format!(
            "none of the {} profile's remaining GPUs ({}) has {} free right now, so there is no price to plan against",
            profile.name,
            types.join(" | "),
            profile.gpu_count
        )));
    };
    let max_price_hr = limits.max_price_hr.map_or(highest, |c| c.min(highest));
    Ok(Choice {
        gpu_types: types,
        max_price_hr,
        highest_listed: highest,
        dropped,
    })
}

/// Remove every type `why` gives a reason for, recording the reason.
fn narrow(
    types: &mut Vec<String>,
    dropped: &mut Vec<(String, String)>,
    why: impl Fn(&str) -> Option<String>,
) {
    types.retain(|t| match why(t) {
        Some(w) => {
            dropped.push((t.clone(), w));
            false
        }
        None => true,
    });
}

/// What a plan actually rented, set against what the plan asked for. Nothing here
/// terminates a pod: a mismatch is reported loudly and the human or the caller decides.
#[derive(Debug, Clone, PartialEq)]
pub struct Rental {
    pub gpu: Option<String>,
    pub cuda_version: Option<String>,
    pub min_cuda: Option<String>,
    pub cost_per_hr: f64,
    pub plan_max_price_hr: f64,
    /// Problems the caller must look at before relying on the pod.
    pub warnings: Vec<String>,
    /// Things the pod API did not say, so a check could not be made.
    pub notes: Vec<String>,
}

impl Rental {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "gpu": self.gpu,
            "cuda_version": self.cuda_version,
            "min_cuda": self.min_cuda,
            "cost_per_hr": self.cost_per_hr,
            "plan_max_price_hr": self.plan_max_price_hr,
            "warnings": self.warnings,
            "notes": self.notes,
        })
    }
}

/// Compare a rented pod with its plan: the GPU type is one the plan listed, the price
/// is within the plan's, and the host driver's CUDA meets the plan's floor.
pub fn audit_rental(
    pod: &crate::runpod::Pod,
    plan_gpu_types: &[String],
    plan_max_price_hr: f64,
    min_cuda: Option<&str>,
) -> Rental {
    let mut warnings = Vec::new();
    let mut notes = Vec::new();
    let gpu = pod.gpu_type().map(str::to_string);
    let cuda = pod.host_cuda().map(str::to_string);
    match (&gpu, plan_gpu_types.is_empty()) {
        (Some(g), false) if !plan_gpu_types.contains(g) => warnings.push(format!(
            "RENTED A GPU THE PLAN DID NOT LIST: {g} is not in [{}]",
            plan_gpu_types.join(" | ")
        )),
        (None, _) => notes.push("the pod API did not say which GPU type was rented".into()),
        _ => {}
    }
    if pod.cost_per_hr > plan_max_price_hr + 0.005 {
        warnings.push(format!(
            "RENTED ABOVE THE PLAN'S PRICE: ${:.2}/hr against the plan's ${:.2}/hr, so the committed worst case no longer covers it",
            pod.cost_per_hr, plan_max_price_hr
        ));
    }
    if let Some(need) = min_cuda {
        match cuda.as_deref() {
            Some(have) => match crate::config::cuda_meets(have, need) {
                Some(false) => warnings.push(format!(
                    "HOST CUDA TOO OLD: the host driver supports CUDA {have} and the plan needs {need} or newer. The work will probably fail after setup (\"driver too old\"). Nothing was terminated: offrig_shutdown stops the rent, or carry on knowingly."
                )),
                Some(true) => {}
                None => notes.push(format!(
                    "host CUDA {have:?} could not be compared with the plan's {need}"
                )),
            },
            None => notes.push(format!(
                "the pod API did not report the host's CUDA version, so the plan's floor of {need} is unchecked; run nvidia-smi through offrig_exec before installing anything"
            )),
        }
    }
    Rental {
        gpu,
        cuda_version: cuda,
        min_cuda: min_cuda.map(str::to_string),
        cost_per_hr: pod.cost_per_hr,
        plan_max_price_hr,
        warnings,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runpod::{Machine, Pod};

    const RTX_S: &str = "NVIDIA RTX PRO 6000 Blackwell Server Edition";
    const RTX_W: &str = "NVIDIA RTX PRO 6000 Blackwell Workstation Edition";
    const A100: &str = "NVIDIA A100-SXM4-80GB";
    const A100_PCIE: &str = "NVIDIA A100 80GB PCIe";
    const H100: &str = "NVIDIA H100 80GB HBM3";

    fn offer(id: &str, gb: u32, price: Option<f64>) -> GpuOffer {
        GpuOffer {
            id: id.into(),
            display_name: id.into(),
            memory_gb: gb,
            gpu_count: 1,
            price_per_hr: price,
            stock: None,
        }
    }

    /// The numbers from issue #10: the RTX PRO 6000 at about $2.09 and the dearest
    /// fallback at $3.49, the profile's top price.
    fn market() -> Vec<GpuOffer> {
        vec![
            offer(RTX_S, 96, Some(2.09)),
            offer(RTX_W, 96, Some(1.99)),
            offer(A100, 80, Some(1.89)),
            offer(A100_PCIE, 80, Some(1.64)),
            offer("NVIDIA H100 NVL", 94, Some(3.07)),
            offer(H100, 80, Some(3.49)),
        ]
    }

    fn job() -> Profile {
        Config::default().profile("job").expect("job").clone()
    }

    #[test]
    fn with_no_limits_the_worst_case_is_the_profiles_top_price_as_before() {
        let c = choose(&job(), &market(), &Limits::default()).expect("choice");
        assert_eq!(c.gpu_types, job().gpu_type_ids);
        assert_eq!(c.max_price_hr, 3.49);
        assert!(c.dropped.is_empty());
    }

    #[test]
    fn max_price_hr_leaves_out_dearer_offers_and_prices_the_worst_case_from_what_is_left() {
        let limits = Limits {
            max_price_hr: Some(2.2),
            no_fallback: false,
        };
        let c = choose(&job(), &market(), &limits).expect("choice");
        assert_eq!(c.gpu_types, [RTX_S, RTX_W, A100, A100_PCIE]);
        assert!(
            c.dropped
                .iter()
                .any(|(t, w)| t == H100 && w.contains("above"))
        );
        assert_eq!(c.highest_listed, 2.09);
        assert_eq!(c.max_price_hr, 2.09, "min(2.20, 2.09)");
        // The issue's arithmetic: 3.5 h commits $7.32 here, not $12.22.
        let worst = (c.max_price_hr * 3.5 * 100.0).ceil() / 100.0;
        assert_eq!(worst, 7.32);
        let top = (3.49f64 * 3.5 * 100.0).ceil() / 100.0;
        assert_eq!(top, 12.22);
    }

    #[test]
    fn a_cap_above_every_listed_price_never_raises_the_price() {
        let limits = Limits {
            max_price_hr: Some(10.0),
            no_fallback: false,
        };
        let c = choose(&job(), &market(), &limits).expect("choice");
        assert_eq!(c.max_price_hr, 3.49, "min(10, 3.49)");
        assert_eq!(c.gpu_types.len(), 6);
    }

    #[test]
    fn a_type_with_no_price_cannot_be_held_to_a_cap() {
        let mut m = market();
        m[0] = offer(RTX_S, 96, None); // none free now
        let limits = Limits {
            max_price_hr: Some(2.2),
            no_fallback: false,
        };
        let c = choose(&job(), &m, &limits).expect("choice");
        assert!(!c.gpu_types.contains(&RTX_S.to_string()));
        assert!(
            c.dropped
                .iter()
                .any(|(t, w)| t == RTX_S && w.contains("no price"))
        );
        // Without a cap the unpriced type stays: the plan may wait for it.
        let open = choose(&job(), &m, &Limits::default()).expect("choice");
        assert!(open.gpu_types.contains(&RTX_S.to_string()));
    }

    #[test]
    fn no_fallback_keeps_the_first_family_with_both_rtx_editions_together() {
        let limits = Limits {
            max_price_hr: None,
            no_fallback: true,
        };
        let c = choose(&job(), &market(), &limits).expect("choice");
        assert_eq!(c.gpu_types, [RTX_S, RTX_W]);
        assert_eq!(c.max_price_hr, 2.09, "the 3.49 H100 is not in the plan");
        assert_eq!(gpu_family(RTX_S), gpu_family(RTX_W));
        assert_ne!(gpu_family(A100), gpu_family(A100_PCIE));
        assert_eq!(gpu_family("NVIDIA A40"), "A40");
    }

    #[test]
    fn no_fallback_with_the_first_family_not_free_is_refused_not_widened() {
        let mut m = market();
        m[0] = offer(RTX_S, 96, None);
        m[1] = offer(RTX_W, 96, None);
        let limits = Limits {
            max_price_hr: None,
            no_fallback: true,
        };
        let e = choose(&job(), &m, &limits).expect_err("nothing priced");
        assert!(e.to_string().contains("no price to plan against"), "{e}");
    }

    #[test]
    fn min_vram_gb_drops_smaller_cards_and_unknown_ones() {
        let mut p = job();
        p.min_vram_gb = Some(90);
        let c = choose(&p, &market(), &Limits::default()).expect("choice");
        assert_eq!(c.gpu_types, [RTX_S, RTX_W, "NVIDIA H100 NVL"]);
        assert!(
            c.dropped
                .iter()
                .any(|(t, w)| t == A100 && w.contains("80 GB"))
        );
        // A type RunPod does not list at all has no memory to check.
        let no_rtx: Vec<GpuOffer> = market().into_iter().skip(2).collect();
        let c = choose(&p, &no_rtx, &Limits::default()).expect("choice");
        assert!(
            c.dropped
                .iter()
                .any(|(t, w)| t == RTX_S && w.contains("no memory"))
        );
    }

    #[test]
    fn min_vram_gb_counts_all_the_profiles_gpus() {
        let mut p = job();
        p.gpu_count = 2;
        let two = |id: &str, gb, price| GpuOffer {
            gpu_count: 2,
            ..offer(id, gb, price)
        };
        let m = vec![two(RTX_S, 96, Some(4.18)), two(A100, 80, Some(3.78))];
        p.min_vram_gb = Some(160);
        let c = choose(&p, &m, &Limits::default()).expect("choice");
        assert_eq!(c.gpu_types, [RTX_S, A100], "2 x 80 GB is exactly 160");
        p.min_vram_gb = Some(161);
        let c = choose(&p, &m, &Limits::default()).expect("choice");
        assert_eq!(c.gpu_types, [RTX_S], "only 2 x 96 = 192 clears 161");
        p.min_vram_gb = Some(193);
        assert!(choose(&p, &m, &Limits::default()).is_err());
    }

    #[test]
    fn nothing_remaining_is_refused_with_every_reason() {
        let limits = Limits {
            max_price_hr: Some(0.5),
            no_fallback: true,
        };
        let e = choose(&job(), &market(), &limits).expect_err("refused");
        let text = e.to_string();
        assert!(text.contains("no GPU type"), "{text}");
        assert!(text.contains("fallback is off"), "{text}");
        assert!(text.contains("above max_price_hr"), "{text}");
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let l = Limits {
                max_price_hr: Some(bad),
                no_fallback: false,
            };
            assert!(choose(&job(), &market(), &l).is_err(), "{bad}");
        }
    }

    fn pod(gpu: Option<&str>, cuda: Option<&str>, cost: f64) -> Pod {
        serde_json::from_value(serde_json::json!({
            "id": "p1",
            "costPerHr": cost,
            "machine": {"gpuTypeId": gpu, "cudaVersion": cuda},
        }))
        .expect("pod")
    }

    #[test]
    fn a_host_older_than_the_plans_cuda_floor_is_flagged_loudly() {
        let types = vec![A100.to_string()];
        let r = audit_rental(
            &pod(Some(A100), Some("12.8"), 1.89),
            &types,
            3.49,
            Some("13.0"),
        );
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].starts_with("HOST CUDA TOO OLD"));
        assert!(r.warnings[0].contains("12.8") && r.warnings[0].contains("13.0"));
        assert_eq!(r.gpu.as_deref(), Some(A100));
        // 12.10 is newer than 12.8 (numeric compare, not text).
        let r = audit_rental(
            &pod(Some(A100), Some("12.10"), 1.0),
            &types,
            3.49,
            Some("12.8"),
        );
        assert!(r.warnings.is_empty());
        let ok = audit_rental(
            &pod(Some(A100), Some("13.0"), 1.0),
            &types,
            3.49,
            Some("13.0"),
        );
        assert!(ok.warnings.is_empty() && ok.notes.is_empty());
    }

    #[test]
    fn a_pod_that_does_not_report_cuda_is_a_note_not_a_pass() {
        let types = vec![A100.to_string()];
        let r = audit_rental(&pod(Some(A100), None, 1.0), &types, 3.49, Some("13.0"));
        assert!(r.warnings.is_empty());
        assert!(r.notes.iter().any(|n| n.contains("did not report")));
        let none = audit_rental(&pod(Some(A100), None, 1.0), &types, 3.49, None);
        assert!(none.notes.is_empty(), "no floor, nothing to check");
        assert_eq!(Machine::default().cuda_version, None);
    }

    #[test]
    fn a_gpu_outside_the_plan_or_a_price_above_it_is_flagged() {
        let types = vec![RTX_S.to_string()];
        let r = audit_rental(&pod(Some(A100), None, 3.0), &types, 2.09, None);
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("DID NOT LIST"));
        assert!(r.warnings[1].contains("ABOVE THE PLAN"));
        let fine = audit_rental(&pod(Some(RTX_S), None, 2.09), &types, 2.09, None);
        assert!(fine.warnings.is_empty());
    }

    #[test]
    fn the_cuda_version_may_arrive_as_a_number() {
        let p: Pod = serde_json::from_value(
            serde_json::json!({"id": "p", "machine": {"cudaVersion": 12.8}}),
        )
        .expect("pod");
        assert_eq!(p.host_cuda(), Some("12.8"));
    }
}
