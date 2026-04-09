//! Runtime capability gating.
//!
//! The conformance suite runs against every kind of broker, but no single
//! broker supports every optional feature in the MQTT v5 specification. Tests
//! that require optional features declare their preconditions as a list of
//! [`crate::capabilities::Requirement`] values; this module checks those
//! preconditions against a live [`crate::sut::SutHandle`] and reports the
//! first unmet requirement as a [`SkipReason`].
//!
//! Use [`skip_if_missing`] at the top of a test:
//!
//! ```ignore
//! use mqtt5_conformance::capabilities::Requirement;
//! use mqtt5_conformance::skip::skip_if_missing;
//!
//! if let Err(reason) = skip_if_missing(sut, &[Requirement::SharedSubscriptionAvailable]) {
//!     println!("skipping: {reason}");
//!     return;
//! }
//! ```

use crate::capabilities::Requirement;
use crate::sut::SutHandle;

/// A description of why a test was skipped.
///
/// Carries the unmet [`Requirement`] so reports can group skipped tests by
/// which capability is missing on the SUT.
#[derive(Debug, Clone)]
pub struct SkipReason {
    pub requirement: Requirement,
    pub message: String,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SkipReason {}

/// Returns `Ok(())` if `sut` meets every requirement in `requirements`,
/// otherwise returns a [`SkipReason`] naming the first unmet requirement.
///
/// # Errors
/// Returns a [`SkipReason`] describing the first unmet requirement.
pub fn skip_if_missing(sut: &SutHandle, requirements: &[Requirement]) -> Result<(), SkipReason> {
    let capabilities = sut.capabilities();
    for &requirement in requirements {
        if !requirement.matches(capabilities) {
            return Err(SkipReason {
                message: format!(
                    "SUT does not satisfy required capability `{}`",
                    requirement.label()
                ),
                requirement,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::Capabilities;
    use crate::sut::{SutDescriptor, SutHandle};

    fn external_sut(caps: Capabilities) -> SutHandle {
        SutHandle::External(Box::new(SutDescriptor {
            capabilities: caps,
            ..SutDescriptor::default()
        }))
    }

    #[test]
    fn skip_returns_ok_when_all_requirements_met() {
        let caps = Capabilities::default();
        let sut = external_sut(caps);
        assert!(skip_if_missing(&sut, &[Requirement::MinQos(1)]).is_ok());
    }

    #[test]
    fn skip_returns_reason_when_requirement_missing() {
        let caps = Capabilities {
            max_qos: 0,
            ..Capabilities::default()
        };
        let sut = external_sut(caps);
        let err = skip_if_missing(&sut, &[Requirement::MinQos(1)]).unwrap_err();
        assert_eq!(err.requirement, Requirement::MinQos(1));
        assert!(err.message.contains("max_qos >= 1"));
    }

    #[test]
    fn skip_returns_first_unmet_requirement() {
        let caps = Capabilities {
            shared_subscription_available: false,
            ..Capabilities::default()
        };
        let sut = external_sut(caps);
        let err = skip_if_missing(
            &sut,
            &[
                Requirement::TransportTcp,
                Requirement::TransportTls,
                Requirement::SharedSubscriptionAvailable,
            ],
        )
        .unwrap_err();
        assert_eq!(err.requirement, Requirement::TransportTls);
    }

    #[test]
    fn skip_reason_is_display() {
        let reason = SkipReason {
            requirement: Requirement::Acl,
            message: "SUT does not satisfy required capability `acl`".to_owned(),
        };
        assert_eq!(
            reason.to_string(),
            "SUT does not satisfy required capability `acl`"
        );
    }
}
