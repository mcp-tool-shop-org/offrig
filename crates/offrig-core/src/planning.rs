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

/// The most container disk offrig will ask a pod for, in GB. This is offrig's own sanity
/// bound against a typo; RunPod's own limit is not checked here.
pub const MAX_CONTAINER_DISK_GB: u32 = 2000;

/// A plan's `container_disk_gb` override: a whole number of GB, at least 1.
pub fn validate_container_disk(gb: u32) -> Result<u32> {
    if (1..=MAX_CONTAINER_DISK_GB).contains(&gb) {
        Ok(gb)
    } else {
        Err(Error::Refused(format!(
            "container_disk_gb {gb} must be between 1 and {MAX_CONTAINER_DISK_GB}"
        )))
    }
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
    // An empty catalog CUDA list is the copied-list fallback, so those types stay.
    // A type the catalog did not return is not dropped here: there is no list to
    // hold it to. A non-empty list that cannot meet the pin, or the floor, is dropped.
    // A pin and a min_cuda floor are never both set. cuda_runtime may sit beside a
    // pin when every pinned version meets that runtime's host floor; the pin is
    // the constraint this filter applies.
    if let Some(pin) = profile.cuda_versions.as_ref().filter(|v| !v.is_empty()) {
        let list = pin.join(", ");
        narrow(&mut types, &mut dropped, |t| match offer(t) {
            Some(o) if !o.cuda.is_empty() && !o.meets_cuda_allow(pin) => Some(format!(
                "no available catalog CUDA version is in the profile's cuda_versions ({list})"
            )),
            _ => None,
        });
    } else if let Some(floor) = profile.effective_min_cuda() {
        narrow(&mut types, &mut dropped, |t| match offer(t) {
            Some(o) if !o.cuda.is_empty() && !o.meets_cuda_floor(&floor) => Some(format!(
                "no available catalog CUDA version is at or above the floor {floor}"
            )),
            _ => None,
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
    /// Where `cuda_version` came from: `"nvidia-smi"` (measured on the pod through the
    /// lane) or `"pod API"`. `None` when it is unknown.
    pub cuda_source: Option<&'static str>,
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
            "cuda_source": self.cuda_source,
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
        cuda_source: cuda.as_ref().map(|_| "pod API"),
        cuda_version: cuda,
        min_cuda: min_cuda.map(str::to_string),
        cost_per_hr: pod.cost_per_hr,
        plan_max_price_hr,
        warnings,
        notes,
    }
}

/// What asking the pod itself for its CUDA version came to, once ssh is up.
#[derive(Debug, Clone, PartialEq)]
pub enum HostCuda {
    /// Not asked (before ssh is up).
    NotAsked,
    /// `nvidia-smi` through the lane printed this version.
    Measured(String),
    /// `nvidia-smi` could not be run or printed no version; the reason.
    Unavailable(String),
}

/// [`audit_rental`], with the host's CUDA taken from `nvidia-smi` on the pod when that
/// was measured. The pod API does not report the host's CUDA, so the floor check had
/// nothing to compare (issue #15); a measured version fills it and the loud warning
/// fires as before. A measured version wins over the API's if they differ, and says so.
pub fn audit_rental_with_host(
    pod: &crate::runpod::Pod,
    host: &HostCuda,
    plan_gpu_types: &[String],
    plan_max_price_hr: f64,
    min_cuda: Option<&str>,
) -> Rental {
    let mut seen = pod.clone();
    let mut extra_note = None;
    let mut measured = false;
    if let HostCuda::Measured(v) = host {
        if let Some(api) = pod.host_cuda()
            && api != v
        {
            extra_note = Some(format!(
                "the pod API reported host CUDA {api} but nvidia-smi on the pod says {v}; using {v}"
            ));
        }
        seen.machine
            .get_or_insert_with(Default::default)
            .cuda_version = Some(v.clone());
        measured = true;
    }
    let mut r = audit_rental(&seen, plan_gpu_types, plan_max_price_hr, min_cuda);
    if measured {
        r.cuda_source = Some("nvidia-smi");
    }
    if let HostCuda::Unavailable(why) = host
        && r.cuda_version.is_none()
    {
        r.notes
            .retain(|n| !n.contains("did not report the host's CUDA"));
        if let Some(need) = min_cuda {
            r.notes.push(format!(
                "neither the pod API nor nvidia-smi on the pod gave the host's CUDA version ({why}), so the plan's floor of {need} is unchecked"
            ));
        }
    }
    r.notes.extend(extra_note);
    r
}

/// Whether `nvidia-smi` was asked for a card count. Status does not ask; a job asks
/// once ssh is up, in the same reading that produces the CUDA version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasuredGpus {
    /// ssh is not up, this view does not run commands, or `nvidia-smi` could not be run.
    /// A failed ssh is not a count of zero.
    NotRead,
    /// `nvidia-smi` ran. `None` means it printed no card rows.
    Read(Option<u32>),
}

/// Which count disagreed with the plan. Present only when [`GpuReport::gpu_mismatch`]
/// is `Some(true)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuMismatchSource {
    /// The pod record differs from the plan. The measurement was not read, or it matches.
    Record,
    /// `nvidia-smi` differs from the plan. The record matches, or it omitted the count.
    Measured,
    /// The record and the measurement both differ from the plan.
    Both,
}

impl GpuMismatchSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Measured => "measured",
            Self::Both => "both",
        }
    }
}

/// The pod record's card count beside the plan's, and the count `nvidia-smi` measured
/// when that reading exists. Nothing here terminates a pod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuReport {
    /// The pod record's count. `None` when the record omitted it (JSON default 0).
    pub gpu_count: Option<u32>,
    /// `2x <type>` from the pod record. `None` when the count was omitted.
    pub gpus: Option<String>,
    /// The plan's count. `None` when this pod is not one of the caller's open plans,
    /// or the stored count is 0.
    pub planned_gpu_count: Option<u32>,
    /// `Some(true)` when the record or the measurement differs from the plan.
    /// `Some(false)` when every known count matches, or there is no plan to check.
    /// `None` when a plan exists, the record omitted the count, and nothing has been
    /// measured: unknown, not a confirmed match.
    pub gpu_mismatch: Option<bool>,
    /// `record`, `measured`, or `both` when `gpu_mismatch` is `Some(true)`.
    pub gpu_mismatch_source: Option<GpuMismatchSource>,
    /// The card count from `nvidia-smi`, once it was asked. `Some(0)` when it ran and
    /// listed no cards against a plan above 0.
    pub measured_gpu_count: Option<u32>,
    /// True once `nvidia-smi` returned, including a table with no card rows.
    pub measured: bool,
    /// Loud lines for `next_action`. Empty when there is nothing to look at.
    pub warnings: Vec<String>,
}

/// `reported` of 0 is the JSON default when a pod record omits `gpuCount`. RunPod's
/// count is at least 1, so 0 means "not reported".
pub fn describe_gpu_count(
    pod: &crate::runpod::Pod,
    planned: Option<u32>,
    measured: MeasuredGpus,
) -> GpuReport {
    let gpu_count = (pod.gpu_count > 0).then_some(pod.gpu_count);
    let gpus = gpu_count.map(
        |n| match pod.gpu_type().map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => format!("{n}x {t}"),
            None => format!("{n}x"),
        },
    );
    let planned_gpu_count = planned.filter(|n| *n > 0);
    // A reading that ran and listed no cards is zero cards once a plan asked for some.
    // That is the worst mismatch, not "not measured".
    let no_cards = matches!(measured, MeasuredGpus::Read(None)) && planned_gpu_count.is_some();
    let (measured_flag, measured_gpu_count) = match measured {
        MeasuredGpus::NotRead => (false, None),
        MeasuredGpus::Read(Some(n)) => (true, Some(n)),
        MeasuredGpus::Read(None) if no_cards => (true, Some(0)),
        MeasuredGpus::Read(None) => (true, None),
    };
    let record_differs = matches!(
        (gpu_count, planned_gpu_count),
        (Some(have), Some(want)) if have != want
    );
    let measured_differs = matches!(
        (measured_gpu_count, planned_gpu_count),
        (Some(seen), Some(want)) if seen != want
    );
    // A missing record must not look like a confirmed match before anything is measured.
    let unknown = planned_gpu_count.is_some() && gpu_count.is_none() && !measured_flag;
    let (gpu_mismatch, gpu_mismatch_source) = if record_differs && measured_differs {
        (Some(true), Some(GpuMismatchSource::Both))
    } else if record_differs {
        (Some(true), Some(GpuMismatchSource::Record))
    } else if measured_differs {
        (Some(true), Some(GpuMismatchSource::Measured))
    } else if unknown {
        (None, None)
    } else {
        (Some(false), None)
    };
    let mut warnings = Vec::new();
    if let Some(source) = gpu_mismatch_source {
        warnings.push(gpu_mismatch_warning(
            &pod.id,
            gpus.as_deref().unwrap_or("an unknown count"),
            planned_gpu_count.unwrap_or(0),
            measured_gpu_count,
            no_cards,
            source,
        ));
    }
    if unknown && let Some(want) = planned_gpu_count {
        warnings.push(format!(
            "the pod record gives no GPU count; it can't be checked against the plan's {want} until offrig_job measures it. Nothing was shut down."
        ));
    }
    // The record and the measurement can disagree even when one of them matches the plan.
    if let (Some(seen), Some(api)) = (measured_gpu_count, gpu_count)
        && seen != api
    {
        warnings.push(format!(
            "nvidia-smi on the pod counts {seen} GPU(s) and the pod record says {api}. Nothing was shut down."
        ));
    }
    GpuReport {
        gpu_count,
        gpus,
        planned_gpu_count,
        gpu_mismatch,
        gpu_mismatch_source,
        measured_gpu_count,
        measured: measured_flag,
        warnings,
    }
}

fn gpu_mismatch_warning(
    pod_id: &str,
    reports: &str,
    planned: u32,
    seen: Option<u32>,
    no_cards: bool,
    source: GpuMismatchSource,
) -> String {
    let shut = "Nothing was shut down.";
    let measurement = if no_cards {
        "nvidia-smi lists no GPUs".to_string()
    } else {
        format!("nvidia-smi counts {}", seen.unwrap_or(0))
    };
    match source {
        GpuMismatchSource::Record => format!(
            "GPU COUNT MISMATCH: pod {pod_id} reports {reports} and the plan asked for {planned} (source record). {shut}"
        ),
        GpuMismatchSource::Measured => format!(
            "GPU COUNT MISMATCH: {measurement} on pod {pod_id} and the plan asked for {planned} (source measured). {shut}"
        ),
        GpuMismatchSource::Both => format!(
            "GPU COUNT MISMATCH: pod {pod_id} reports {reports} and {measurement}; the plan asked for {planned} (source both). {shut}"
        ),
    }
}

/// Write [`describe_gpu_count`] onto a rental object. Warnings are appended to the
/// rental's own `warnings`, which `offrig_job` already repeats in `next_action`.
/// Measured fields are omitted until `nvidia-smi` has been asked.
pub fn apply_gpu_count(
    rented: &mut serde_json::Value,
    pod: &crate::runpod::Pod,
    planned: Option<u32>,
    measured: MeasuredGpus,
) {
    let report = describe_gpu_count(pod, planned, measured);
    let Some(obj) = rented.as_object_mut() else {
        return;
    };
    obj.insert("gpu_count".into(), serde_json::json!(report.gpu_count));
    obj.insert("gpus".into(), serde_json::json!(report.gpus));
    obj.insert(
        "planned_gpu_count".into(),
        serde_json::json!(report.planned_gpu_count),
    );
    obj.insert(
        "gpu_mismatch".into(),
        serde_json::json!(report.gpu_mismatch),
    );
    obj.insert(
        "gpu_mismatch_source".into(),
        serde_json::json!(report.gpu_mismatch_source.map(GpuMismatchSource::as_str)),
    );
    if report.measured {
        obj.insert(
            "measured_gpu_count".into(),
            serde_json::json!(report.measured_gpu_count),
        );
    }
    if report.warnings.is_empty() {
        return;
    }
    if !obj.contains_key("warnings") {
        obj.insert("warnings".into(), serde_json::json!([]));
    }
    if let Some(arr) = obj.get_mut("warnings").and_then(|w| w.as_array_mut()) {
        for line in report.warnings {
            arr.push(serde_json::json!(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runpod::{CatalogCuda, Machine, Pod};

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
            cuda: Vec::new(),
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
    fn a_catalog_cuda_list_below_the_floor_is_dropped_and_an_omitted_list_is_kept() {
        let mut m = market();
        m[0].cuda = vec![CatalogCuda {
            version: "12.8".into(),
            available: true,
        }];
        m[1].cuda = vec![
            CatalogCuda {
                version: "13.0".into(),
                available: false,
            },
            CatalogCuda {
                version: "12.9".into(),
                available: true,
            },
        ];
        m[3].cuda = vec![CatalogCuda {
            version: "13.1".into(),
            available: true,
        }];
        let c = choose(&job(), &m, &Limits::default()).expect("choice");
        assert!(!c.gpu_types.iter().any(|t| t == RTX_S));
        assert!(!c.gpu_types.iter().any(|t| t == RTX_W));
        assert!(
            c.gpu_types.iter().any(|t| t == A100),
            "omitted list falls back"
        );
        assert!(
            c.gpu_types.iter().any(|t| t == A100_PCIE),
            "13.1 is newer than the floor"
        );
        assert!(
            c.dropped
                .iter()
                .any(|(t, w)| t == RTX_S && w.contains("13.0"))
        );
        // A type the catalog did not return is not dropped for CUDA.
        let no_rtx: Vec<GpuOffer> = market().into_iter().skip(2).collect();
        let c = choose(&job(), &no_rtx, &Limits::default()).expect("choice");
        assert!(c.gpu_types.iter().any(|t| t == RTX_S));
        assert!(c.dropped.iter().all(|(_, w)| !w.contains("CUDA")));
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
    fn nvidia_smi_fills_the_host_cuda_and_the_floor_warning_fires() {
        let types = vec![A100.to_string()];
        let smi = "| NVIDIA-SMI 570.124.06   Driver Version: 570.124.06   CUDA Version: 12.8   |";
        let host = HostCuda::Measured(crate::remote::parse_cuda_version(smi).expect("version"));
        let audit =
            |p: &Pod, h: &HostCuda| audit_rental_with_host(p, h, &types, 3.49, Some("13.0"));
        // The pod API said nothing; the measured version is below the floor.
        let r = audit(&pod(Some(A100), None, 1.89), &host);
        assert_eq!(r.cuda_version.as_deref(), Some("12.8"));
        assert_eq!(r.cuda_source, Some("nvidia-smi"));
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].starts_with("HOST CUDA TOO OLD"));
        assert!(r.notes.is_empty(), "{:?}", r.notes);
        assert_eq!(r.to_json()["cuda_version"], "12.8");
        assert_eq!(r.to_json()["cuda_source"], "nvidia-smi");
        // The newer header form meets the floor: no warning, still no note.
        let newer = HostCuda::Measured(
            crate::remote::parse_cuda_version("CUDA UMD Version: 13.4").expect("version"),
        );
        let ok = audit(&pod(Some(A100), None, 1.89), &newer);
        assert!(ok.warnings.is_empty() && ok.notes.is_empty());
        assert_eq!(ok.cuda_version.as_deref(), Some("13.4"));
        // A pod with no machine block at all still takes the measurement.
        let bare: Pod = serde_json::from_value(serde_json::json!({"id": "p"})).expect("pod");
        assert_eq!(audit(&bare, &host).cuda_version.as_deref(), Some("12.8"));
        // When the API and nvidia-smi disagree the measured one wins, and it says so.
        let r = audit(&pod(Some(A100), Some("13.0"), 1.0), &host);
        assert_eq!(r.cuda_version.as_deref(), Some("12.8"));
        assert_eq!(r.warnings.len(), 1);
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("nvidia-smi on the pod says 12.8"))
        );
    }

    #[test]
    fn an_nvidia_smi_that_gave_nothing_stays_a_note_never_a_pass() {
        let types = vec![A100.to_string()];
        let host = HostCuda::Unavailable("ssh: connection refused".into());
        let audit =
            |p: &Pod, floor: Option<&str>| audit_rental_with_host(p, &host, &types, 3.49, floor);
        let r = audit(&pod(Some(A100), None, 1.0), Some("13.0"));
        assert!(r.warnings.is_empty());
        assert_eq!(r.cuda_version, None);
        assert_eq!(r.cuda_source, None);
        assert_eq!(r.notes.len(), 1, "{:?}", r.notes);
        assert!(r.notes[0].contains("neither the pod API nor nvidia-smi"));
        assert!(r.notes[0].contains("connection refused"));
        // With no floor there is nothing to say.
        assert!(audit(&pod(Some(A100), None, 1.0), None).notes.is_empty());
        // An API-reported version still counts when nvidia-smi was unavailable.
        let api = audit(&pod(Some(A100), Some("12.8"), 1.0), Some("13.0"));
        assert_eq!(api.cuda_source, Some("pod API"));
        assert_eq!(api.warnings.len(), 1);
    }

    #[test]
    fn a_container_disk_override_must_be_a_sane_size() {
        assert_eq!(validate_container_disk(400).expect("ok"), 400);
        assert_eq!(validate_container_disk(1).expect("ok"), 1);
        assert_eq!(validate_container_disk(2000).expect("ok"), 2000);
        for bad in [0, 2001, u32::MAX] {
            assert!(validate_container_disk(bad).is_err(), "{bad}");
        }
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

    #[test]
    fn an_exact_cuda_list_keeps_only_an_overlapping_catalog_version() {
        let mut profile = job();
        profile.min_cuda = None;
        profile.job.as_mut().expect("job image").min_cuda = None;
        profile.cuda_versions = Some(vec!["13.0".into()]);
        let mut offers = market();
        let set = |offers: &mut [GpuOffer], id: &str, cuda: Vec<CatalogCuda>| {
            offers.iter_mut().find(|o| o.id == id).expect("offer").cuda = cuda;
        };
        set(
            &mut offers,
            RTX_S,
            vec![CatalogCuda {
                version: "13.0".into(),
                available: true,
            }],
        );
        set(
            &mut offers,
            A100,
            vec![
                CatalogCuda {
                    version: "12.8".into(),
                    available: true,
                },
                CatalogCuda {
                    version: "13.1".into(),
                    available: true,
                },
            ],
        );
        set(
            &mut offers,
            A100_PCIE,
            vec![CatalogCuda {
                version: "13.0".into(),
                available: false,
            }],
        );
        let choice = choose(&profile, &offers, &Limits::default()).expect("choice");
        assert!(choice.gpu_types.iter().any(|t| t == RTX_S));
        // An omitted catalog list is the copied-list fallback, so the type stays.
        assert!(choice.gpu_types.iter().any(|t| t == RTX_W));
        assert!(!choice.gpu_types.iter().any(|t| t == A100));
        assert!(!choice.gpu_types.iter().any(|t| t == A100_PCIE));
        let why = |id: &str| {
            choice
                .dropped
                .iter()
                .find(|(t, _)| t == id)
                .map(|(_, w)| w.as_str())
                .unwrap_or("")
        };
        assert!(
            why(A100).contains("cuda_versions"),
            "13.1 is not the pin: {}",
            why(A100)
        );
        assert!(
            why(A100_PCIE).contains("cuda_versions"),
            "{}",
            why(A100_PCIE)
        );
    }
}
