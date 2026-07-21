use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::report::{RuleResult, VerificationReport};

pub const LIFECYCLE_VERIFIER_ID: &str = "zcash-payment-lifecycle";
pub const LIFECYCLE_VERIFIER_VERSION: &str = "1.0.0";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PaymentLifecycleState {
    Pending,
    Confirmed,
    Reorged,
    Duplicated,
    Expired,
    Mismatched,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PaymentLifecycleFixture {
    pub case_id: String,
    pub current_height: u32,
    pub expires_at_height: u32,
    pub required_confirmations: u32,
    pub observation_count: u16,
    pub current_confirmations: u32,
    pub previously_confirmed: bool,
    pub recipient_matches: bool,
    pub amount_matches: bool,
    pub expected_state: PaymentLifecycleState,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LifecycleError {
    #[error("lifecycle fixture is invalid")]
    InvalidFixture,
}

pub fn evaluate_lifecycle(
    fixture: &PaymentLifecycleFixture,
) -> Result<PaymentLifecycleState, LifecycleError> {
    if fixture.case_id.is_empty()
        || fixture.required_confirmations == 0
        || fixture.expires_at_height == 0
        || fixture.current_confirmations > 0 && fixture.observation_count == 0
    {
        return Err(LifecycleError::InvalidFixture);
    }

    if fixture.observation_count > 0 && (!fixture.recipient_matches || !fixture.amount_matches) {
        return Ok(PaymentLifecycleState::Mismatched);
    }
    if fixture.observation_count > 1 {
        return Ok(PaymentLifecycleState::Duplicated);
    }
    if fixture.previously_confirmed && fixture.observation_count == 0 {
        return Ok(PaymentLifecycleState::Reorged);
    }
    if fixture.observation_count == 1
        && fixture.current_confirmations >= fixture.required_confirmations
    {
        return Ok(PaymentLifecycleState::Confirmed);
    }
    if fixture.current_height > fixture.expires_at_height {
        return Ok(PaymentLifecycleState::Expired);
    }

    Ok(PaymentLifecycleState::Pending)
}

pub fn verify_lifecycle(fixture: &PaymentLifecycleFixture) -> VerificationReport {
    match evaluate_lifecycle(fixture) {
        Ok(actual) if actual == fixture.expected_state => VerificationReport::passed(
            LIFECYCLE_VERIFIER_ID,
            LIFECYCLE_VERIFIER_VERSION,
            vec![RuleResult::passed(
                "payment.lifecycle.state",
                vec!["zip-0321"],
                "The observed payment state matches the reviewed fixture outcome.",
            )],
        ),
        Ok(_) => VerificationReport::failed(
            LIFECYCLE_VERIFIER_ID,
            LIFECYCLE_VERIFIER_VERSION,
            RuleResult::failed(
                "payment.lifecycle.state",
                vec!["zip-0321"],
                "The observed payment state does not match the expected outcome.",
            ),
        ),
        Err(_) => VerificationReport::failed(
            LIFECYCLE_VERIFIER_ID,
            LIFECYCLE_VERIFIER_VERSION,
            RuleResult::failed(
                "payment.lifecycle.fixture",
                vec!["zip-0321"],
                "The lifecycle fixture is structurally invalid.",
            ),
        ),
    }
}
