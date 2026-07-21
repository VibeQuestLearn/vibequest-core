use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use url::Url;
use zcash_protocol::value::MAX_MONEY;
use zip321::{TransactionRequest, Zip321Error};

use super::{
    address::{
        AddressError, AddressInspection, ReceiverPolicy, ZcashNetwork, inspect_parsed_address,
    },
    report::{RuleResult, VerificationReport},
};

pub const PAYMENT_REQUEST_VERIFIER_ID: &str = "zcash-zip321-payment-request";
pub const PAYMENT_REQUEST_VERIFIER_VERSION: &str = "1.0.0";
pub const MAX_LAB_TOTAL_ZATOSHIS: u64 = 100 * 100_000_000;
const MAX_PAYMENT_REQUEST_BYTES: usize = 16_384;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PaymentRequestPolicy {
    pub expected_network: ZcashNetwork,
    pub receiver_policy: ReceiverPolicy,
    pub require_amounts: bool,
    pub require_positive_amounts: bool,
    pub max_payments: usize,
    pub max_total_zatoshis: u64,
    pub allow_optional_parameters: bool,
    pub max_optional_parameters: usize,
}

impl PaymentRequestPolicy {
    pub const fn shielded_checkout_testnet() -> Self {
        Self {
            expected_network: ZcashNetwork::Testnet,
            receiver_policy: ReceiverPolicy::shielded_recipient(),
            require_amounts: true,
            require_positive_amounts: true,
            max_payments: 16,
            max_total_zatoshis: MAX_LAB_TOTAL_ZATOSHIS,
            allow_optional_parameters: true,
            max_optional_parameters: 8,
        }
    }

    pub const fn protocol_compatible(expected_network: ZcashNetwork) -> Self {
        Self {
            expected_network,
            receiver_policy: ReceiverPolicy::protocol_compatible(),
            require_amounts: false,
            require_positive_amounts: false,
            max_payments: 9_999,
            max_total_zatoshis: MAX_MONEY,
            allow_optional_parameters: true,
            max_optional_parameters: 9_999,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PaymentSummary {
    pub payment_index: usize,
    pub amount_zatoshis: Option<u64>,
    pub has_memo: bool,
    pub has_label: bool,
    pub has_message: bool,
    pub optional_parameter_count: usize,
    pub recipient: AddressInspection,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PaymentRequestSummary {
    pub payment_count: usize,
    pub total_zatoshis: Option<u64>,
    pub memo_count: usize,
    pub optional_parameter_count: usize,
    pub payments: Vec<PaymentSummary>,
}

pub struct ValidatedPaymentRequest {
    request: TransactionRequest,
    summary: PaymentRequestSummary,
}

impl ValidatedPaymentRequest {
    pub fn summary(&self) -> &PaymentRequestSummary {
        &self.summary
    }

    /// Returns the canonical ZIP-321 URI. The caller must treat it as sensitive because it can
    /// contain recipient addresses and memo bytes.
    #[must_use]
    pub fn to_sensitive_uri(&self) -> String {
        self.request.to_uri()
    }
}

impl fmt::Debug for ValidatedPaymentRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedPaymentRequest")
            .field("request", &"[REDACTED]")
            .field("summary", &self.summary)
            .finish()
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PaymentRequestError {
    #[error("payment request input exceeds the verifier limit")]
    InputTooLong,
    #[error("payment request is malformed or unsupported")]
    InvalidRequest,
    #[error("payment request contains an unknown required parameter")]
    UnknownRequiredParameter,
    #[error("payment request contains no payments")]
    EmptyRequest,
    #[error("payment request exceeds the scenario payment count")]
    TooManyPayments,
    #[error("payment recipient is for a different network")]
    WrongNetwork,
    #[error("payment recipient does not satisfy the receiver policy")]
    UnsafeRecipient,
    #[error("payment amount is required")]
    AmountRequired,
    #[error("payment amount must be positive")]
    ZeroAmount,
    #[error("payment amount exceeds the scenario limit")]
    AmountLimitExceeded,
    #[error("memo is not valid for its recipient")]
    MemoNotAllowed,
    #[error("optional parameters are not allowed by this scenario")]
    OptionalParametersForbidden,
    #[error("payment request has too many optional parameters")]
    TooManyOptionalParameters,
}

impl PaymentRequestError {
    fn rule_result(&self) -> RuleResult {
        let (rule_id, sources, message) = match self {
            PaymentRequestError::InputTooLong
            | PaymentRequestError::InvalidRequest
            | PaymentRequestError::EmptyRequest
            | PaymentRequestError::TooManyPayments => (
                "zip321.request.parse",
                vec!["zip-0321", "zip321-0.8.0"],
                "Provide a valid bounded ZIP-321 payment request.",
            ),
            PaymentRequestError::UnknownRequiredParameter => (
                "zip321.parameters.required",
                vec!["zip-0321", "zip321-0.8.0"],
                "Unknown required parameters are not supported.",
            ),
            PaymentRequestError::WrongNetwork | PaymentRequestError::UnsafeRecipient => (
                "zip321.recipient.policy",
                vec!["zip-0316", "zip-0321", "zcash-address-0.12.0"],
                "Every recipient must match the network and receiver policy.",
            ),
            PaymentRequestError::AmountRequired
            | PaymentRequestError::ZeroAmount
            | PaymentRequestError::AmountLimitExceeded => (
                "zip321.amount.policy",
                vec!["zip-0321", "zcash-protocol-0.9.0"],
                "Every amount must satisfy the bounded scenario policy.",
            ),
            PaymentRequestError::MemoNotAllowed => (
                "zip321.memo.policy",
                vec!["zip-0321", "zip321-0.8.0"],
                "Memo data is allowed only for a memo-capable shielded recipient.",
            ),
            PaymentRequestError::OptionalParametersForbidden
            | PaymentRequestError::TooManyOptionalParameters => (
                "zip321.parameters.optional",
                vec!["zip-0321", "zip321-0.8.0"],
                "Optional parameters must satisfy the scenario policy.",
            ),
        };

        RuleResult::failed(rule_id, sources, message)
    }
}

pub fn validate_payment_request(
    uri: &str,
    policy: PaymentRequestPolicy,
) -> Result<ValidatedPaymentRequest, PaymentRequestError> {
    if uri.is_empty() || uri.len() > MAX_PAYMENT_REQUEST_BYTES {
        return Err(PaymentRequestError::InputTooLong);
    }
    let request = TransactionRequest::from_uri(uri).map_err(|error| {
        if contains_required_parameter(uri) {
            PaymentRequestError::UnknownRequiredParameter
        } else {
            map_zip321_error(error)
        }
    })?;
    if request.payments().is_empty() {
        return Err(PaymentRequestError::EmptyRequest);
    }
    if request.payments().len() > policy.max_payments {
        return Err(PaymentRequestError::TooManyPayments);
    }

    let mut memo_count = 0;
    let mut optional_parameter_count = 0;
    let mut payments = Vec::with_capacity(request.payments().len());

    for (index, payment) in request.payments() {
        let recipient = inspect_parsed_address(
            payment.recipient_address().clone(),
            policy.expected_network,
            policy.receiver_policy,
        )
        .map_err(map_address_error)?;
        let amount_zatoshis = payment.amount().map(u64::from);
        if policy.require_amounts && amount_zatoshis.is_none() {
            return Err(PaymentRequestError::AmountRequired);
        }
        if policy.require_positive_amounts && amount_zatoshis == Some(0) {
            return Err(PaymentRequestError::ZeroAmount);
        }

        let has_memo = payment.memo().is_some();
        memo_count += usize::from(has_memo);
        optional_parameter_count += payment.other_params().len();
        payments.push(PaymentSummary {
            payment_index: *index,
            amount_zatoshis,
            has_memo,
            has_label: payment.label().is_some(),
            has_message: payment.message().is_some(),
            optional_parameter_count: payment.other_params().len(),
            recipient,
        });
    }

    if !policy.allow_optional_parameters && optional_parameter_count > 0 {
        return Err(PaymentRequestError::OptionalParametersForbidden);
    }
    if optional_parameter_count > policy.max_optional_parameters {
        return Err(PaymentRequestError::TooManyOptionalParameters);
    }

    let total_zatoshis = request
        .total()
        .map_err(|_| PaymentRequestError::AmountLimitExceeded)?
        .map(u64::from);
    if total_zatoshis.is_some_and(|total| total > policy.max_total_zatoshis) {
        return Err(PaymentRequestError::AmountLimitExceeded);
    }

    Ok(ValidatedPaymentRequest {
        summary: PaymentRequestSummary {
            payment_count: payments.len(),
            total_zatoshis,
            memo_count,
            optional_parameter_count,
            payments,
        },
        request,
    })
}

pub fn verify_payment_request(uri: &str, policy: PaymentRequestPolicy) -> VerificationReport {
    match validate_payment_request(uri, policy) {
        Ok(_) => VerificationReport::passed(
            PAYMENT_REQUEST_VERIFIER_ID,
            PAYMENT_REQUEST_VERIFIER_VERSION,
            vec![
                RuleResult::passed(
                    "zip321.request.parse",
                    vec!["zip-0321", "zip321-0.8.0"],
                    "The request follows the supported ZIP-321 grammar.",
                ),
                RuleResult::passed(
                    "zip321.recipient.policy",
                    vec!["zip-0316", "zip-0321", "zcash-address-0.12.0"],
                    "Every recipient matches the network and receiver policy.",
                ),
                RuleResult::passed(
                    "zip321.amount.policy",
                    vec!["zip-0321", "zcash-protocol-0.9.0"],
                    "The requested value satisfies the scenario amount bounds.",
                ),
                RuleResult::passed(
                    "zip321.memo.policy",
                    vec!["zip-0321", "zip321-0.8.0"],
                    "Memo presence is compatible with each recipient.",
                ),
                RuleResult::passed(
                    "zip321.parameters.optional",
                    vec!["zip-0321", "zip321-0.8.0"],
                    "Optional parameters satisfy the scenario policy.",
                ),
            ],
        ),
        Err(error) => VerificationReport::failed(
            PAYMENT_REQUEST_VERIFIER_ID,
            PAYMENT_REQUEST_VERIFIER_VERSION,
            error.rule_result(),
        ),
    }
}

fn map_zip321_error(error: Zip321Error) -> PaymentRequestError {
    match error {
        Zip321Error::TooManyPayments(_) => PaymentRequestError::TooManyPayments,
        Zip321Error::TransparentMemo(_) => PaymentRequestError::MemoNotAllowed,
        Zip321Error::ZeroValuedTransparentOutput(_)
        | Zip321Error::DuplicateParameter(_, _)
        | Zip321Error::RecipientMissing(_)
        | Zip321Error::InvalidBase64(_)
        | Zip321Error::MemoBytesError(_)
        | Zip321Error::ParseError(_) => PaymentRequestError::InvalidRequest,
        _ => PaymentRequestError::InvalidRequest,
    }
}

fn contains_required_parameter(uri: &str) -> bool {
    Url::parse(uri).ok().is_some_and(|parsed| {
        parsed.query_pairs().any(|(name, _)| {
            name.split('.')
                .next()
                .is_some_and(|base| base.starts_with("req-"))
        })
    })
}

fn map_address_error(error: AddressError) -> PaymentRequestError {
    match error {
        AddressError::WrongNetwork => PaymentRequestError::WrongNetwork,
        AddressError::InputTooLong
        | AddressError::InvalidAddress
        | AddressError::UnifiedRequired
        | AddressError::SproutUnsupported
        | AddressError::ShieldedReceiverRequired
        | AddressError::TransparentReceiverForbidden
        | AddressError::UnknownReceiverUnsupported => PaymentRequestError::UnsafeRecipient,
    }
}
