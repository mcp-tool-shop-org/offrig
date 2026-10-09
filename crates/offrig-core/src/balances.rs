//! What each provider says the account holds, read live and best effort. A balance is
//! shown next to the cap and never stored: it never becomes a cap, never blocks a plan,
//! and a missing key or a failed call is "balance unknown" with the reason.

use crate::store::Provider;

#[derive(Debug, Clone, PartialEq)]
pub enum Balance {
    Known(f64),
    Unknown(String),
}

impl Balance {
    pub fn known(&self) -> Option<f64> {
        match self {
            Balance::Known(v) => Some(*v),
            Balance::Unknown(_) => None,
        }
    }

    /// `$12.34` or `unknown (reason)`.
    pub fn text(&self) -> String {
        match self {
            Balance::Known(v) => format!("${v:.2}"),
            Balance::Unknown(why) => format!("unknown ({why})"),
        }
    }

    pub fn from_result(r: crate::Result<f64>) -> Balance {
        match r {
            Ok(v) if v.is_finite() => Balance::Known(v),
            Ok(_) => Balance::Unknown("the provider reported no usable number".into()),
            Err(e) => Balance::Unknown(e.to_string()),
        }
    }
}

/// The provider's own report of what the account holds. One network call, no retries.
pub fn read(provider: Provider) -> Balance {
    Balance::from_result(match provider {
        Provider::RunPod => crate::runpod::RunPod::from_env()
            .and_then(|rp| rp.account())
            .map(|a| a.client_balance),
        Provider::OpenRouter => crate::openrouter::OpenRouter::from_env()
            .and_then(|or| or.credits())
            .map(|c| c.balance()),
    })
}

/// A warning when a cap promises more than the provider says the account holds.
pub fn cap_exceeds_balance(provider: Provider, cap: f64, balance: &Balance) -> Option<String> {
    let have = balance.known()?;
    (cap > have + 1e-9).then(|| {
        format!(
            "WARNING: the {provider} cap ${cap:.2} is above the ${have:.2} {provider} reports; the account would run dry first"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    #[test]
    fn a_cap_above_the_balance_warns_and_unknown_never_does() {
        let w = cap_exceeds_balance(Provider::OpenRouter, 20.0, &Balance::Known(8.0));
        let w = w.expect("warns");
        assert!(w.starts_with("WARNING"), "{w}");
        assert!(w.contains("openrouter") && w.contains("$20.00") && w.contains("$8.00"));
        assert!(cap_exceeds_balance(Provider::RunPod, 8.0, &Balance::Known(8.0)).is_none());
        assert!(cap_exceeds_balance(Provider::RunPod, 5.0, &Balance::Known(30.0)).is_none());
        assert!(
            cap_exceeds_balance(Provider::RunPod, 50.0, &Balance::Unknown("x".into())).is_none()
        );
    }

    #[test]
    fn a_failed_read_is_unknown_with_the_reason() {
        let b = Balance::from_result(Err(Error::MissingOpenRouterKey));
        assert!(matches!(&b, Balance::Unknown(w) if !w.is_empty()), "{b:?}");
        assert!(b.text().starts_with("unknown ("));
        assert_eq!(Balance::from_result(Ok(4.5)).text(), "$4.50");
        assert!(matches!(
            Balance::from_result(Ok(f64::NAN)),
            Balance::Unknown(_)
        ));
    }
}
