pub mod address;
pub mod lifecycle;
pub mod payment;
pub mod report;
pub mod sources;
pub mod viewing;

pub use address::{
    AddressError, AddressInspection, ParsedAddressKind, ReceiverCapabilities, ReceiverKind,
    ReceiverPolicy, ZcashNetwork, inspect_address, verify_address,
};
pub use lifecycle::{
    LifecycleError, PaymentLifecycleFixture, PaymentLifecycleState, evaluate_lifecycle,
    verify_lifecycle,
};
pub use payment::{
    MAX_LAB_TOTAL_ZATOSHIS, PaymentRequestError, PaymentRequestPolicy, PaymentRequestSummary,
    PaymentSummary, ValidatedPaymentRequest, validate_payment_request, verify_payment_request,
};
pub use report::{RuleResult, RuleStatus, VerificationReport};
pub use sources::{
    LIBRUSTZCASH_REVISION, SOURCE_MANIFEST_VERSION, SourceManifest, SourceReference,
    source_manifest,
};
pub use viewing::{
    ViewingAuthority, ViewingCapabilities, ViewingKeyError, ViewingKeyInspection,
    inspect_viewing_key, verify_viewing_key,
};

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde::Deserialize;
    use std::collections::BTreeSet;
    use zcash_address::{
        ZcashAddress,
        unified::{Address, Encoding, Fvk, Ivk, Receiver, Ufvk, Uivk},
    };
    use zcash_protocol::{consensus::NetworkType, value::Zatoshis};
    use zip321::{Payment, TransactionRequest};

    #[derive(Debug, Deserialize)]
    struct ProtocolCases {
        fixture_version: String,
        sources: Vec<String>,
        mainnet_unified_address: String,
        testnet_sapling_address: String,
        testnet_transparent_address: String,
        zip321: Zip321Cases,
    }

    #[derive(Debug, Deserialize)]
    struct Zip321Cases {
        valid_single: String,
        valid_multi: String,
        unknown_optional: String,
        unknown_required: String,
        amount_at_protocol_max: String,
        amount_above_protocol_max: String,
        amount_too_precise: String,
        transparent_memo: String,
        amount_missing: String,
    }

    fn protocol_cases() -> ProtocolCases {
        serde_json::from_str(include_str!("../../fixtures/zcash/v1/protocol-cases.json"))
            .expect("reviewed protocol cases")
    }

    fn lifecycle_cases() -> Vec<PaymentLifecycleFixture> {
        serde_json::from_str(include_str!(
            "../../fixtures/zcash/v1/payment-lifecycle.json"
        ))
        .expect("reviewed lifecycle cases")
    }

    #[test]
    fn source_manifest_pins_one_cohesive_official_revision() {
        let manifest = source_manifest();
        assert_eq!(manifest.manifest_version, SOURCE_MANIFEST_VERSION);
        assert_eq!(manifest.sources.len(), 6);
        let ids = manifest
            .sources
            .iter()
            .map(|source| source.source_id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), manifest.sources.len());
        for source_id in [
            "zcash-address-0.12.0",
            "zcash-protocol-0.9.0",
            "zip321-0.8.0",
        ] {
            let source = manifest
                .sources
                .iter()
                .find(|source| source.source_id == source_id)
                .expect("pinned crate source");
            assert_eq!(source.revision, LIBRUSTZCASH_REVISION);
            assert!(
                source
                    .url
                    .starts_with("https://github.com/zcash/librustzcash/")
            );
        }
    }

    #[test]
    fn official_unified_address_vectors_enforce_network_and_receiver_policy() {
        let cases = protocol_cases();
        assert_eq!(cases.fixture_version, "zcash-protocol-cases-2026-07-21.1");
        assert!(cases.sources.iter().any(|source| source == "zip-0316"));

        let inspection = inspect_address(
            &cases.mainnet_unified_address,
            ZcashNetwork::Mainnet,
            ReceiverPolicy::shielded_checkout(),
        )
        .expect("official mainnet UA");
        assert_eq!(inspection.kind, ParsedAddressKind::Unified);
        assert!(inspection.receivers.orchard);
        assert!(inspection.receivers.sapling);
        assert!(inspection.receivers.transparent);
        assert_eq!(
            inspection.preferred_supported_receiver,
            Some(ReceiverKind::Orchard)
        );
        assert_eq!(
            inspect_address(
                &cases.mainnet_unified_address,
                ZcashNetwork::Testnet,
                ReceiverPolicy::shielded_checkout(),
            ),
            Err(AddressError::WrongNetwork)
        );

        let (_, mainnet_ua) = Address::decode(&cases.mainnet_unified_address)
            .expect("official vector decodes as Unified Address");
        let testnet_ua = mainnet_ua.encode(&NetworkType::Test);
        assert!(
            inspect_address(
                &testnet_ua,
                ZcashNetwork::Testnet,
                ReceiverPolicy::shielded_checkout(),
            )
            .is_ok()
        );
        assert_eq!(
            inspect_address(
                &testnet_ua,
                ZcashNetwork::Mainnet,
                ReceiverPolicy::shielded_checkout(),
            ),
            Err(AddressError::WrongNetwork)
        );
    }

    #[test]
    fn receiver_policy_distinguishes_unified_shielded_and_transparent_inputs() {
        let cases = protocol_cases();
        assert_eq!(
            inspect_address(
                &cases.testnet_sapling_address,
                ZcashNetwork::Testnet,
                ReceiverPolicy::shielded_checkout(),
            ),
            Err(AddressError::UnifiedRequired)
        );
        assert!(
            inspect_address(
                &cases.testnet_sapling_address,
                ZcashNetwork::Testnet,
                ReceiverPolicy::shielded_recipient(),
            )
            .is_ok()
        );
        assert_eq!(
            inspect_address(
                &cases.testnet_transparent_address,
                ZcashNetwork::Testnet,
                ReceiverPolicy::shielded_recipient(),
            ),
            Err(AddressError::ShieldedReceiverRequired)
        );

        let (_, ua) = Address::decode(&cases.mainnet_unified_address).expect("official UA");
        let testnet_ua = ua.encode(&NetworkType::Test);
        let mut no_transparent = ReceiverPolicy::shielded_checkout();
        no_transparent.forbid_transparent_receiver = true;
        assert_eq!(
            inspect_address(&testnet_ua, ZcashNetwork::Testnet, no_transparent),
            Err(AddressError::TransparentReceiverForbidden)
        );

        let unknown_ua = Address::try_from_items(vec![
            Receiver::Orchard([7; 43]),
            Receiver::Unknown {
                typecode: 65_536,
                data: vec![9; 32],
            },
        ])
        .expect("valid forward-compatible UA")
        .encode(&NetworkType::Test);
        assert_eq!(
            inspect_address(
                &unknown_ua,
                ZcashNetwork::Testnet,
                ReceiverPolicy::shielded_checkout(),
            ),
            Err(AddressError::UnknownReceiverUnsupported)
        );
    }

    #[test]
    fn zip321_single_request_round_trips_without_exposing_payload_in_debug() {
        let cases = protocol_cases();
        let validated = validate_payment_request(
            &cases.zip321.valid_single,
            PaymentRequestPolicy::shielded_checkout_testnet(),
        )
        .expect("official ZIP-321 request");
        assert_eq!(validated.summary().payment_count, 1);
        assert_eq!(validated.summary().total_zatoshis, Some(100_000_000));
        assert_eq!(validated.summary().memo_count, 1);

        let canonical = validated.to_sensitive_uri();
        let reparsed = validate_payment_request(
            &canonical,
            PaymentRequestPolicy::shielded_checkout_testnet(),
        )
        .expect("canonical request reparses");
        assert_eq!(validated.summary(), reparsed.summary());
        assert!(!format!("{validated:?}").contains(&cases.testnet_sapling_address));
    }

    #[test]
    fn zip321_repeated_payments_and_unknown_parameter_rules_are_deterministic() {
        let cases = protocol_cases();
        let multi = validate_payment_request(
            &cases.zip321.valid_multi,
            PaymentRequestPolicy::protocol_compatible(ZcashNetwork::Testnet),
        )
        .expect("official repeated payment request");
        assert_eq!(multi.summary().payment_count, 2);
        assert_eq!(multi.summary().memo_count, 1);

        let optional = validate_payment_request(
            &cases.zip321.unknown_optional,
            PaymentRequestPolicy::shielded_checkout_testnet(),
        )
        .expect("unknown optional parameter is permitted");
        assert_eq!(optional.summary().optional_parameter_count, 1);
        assert_eq!(
            validate_payment_request(
                &cases.zip321.unknown_required,
                PaymentRequestPolicy::shielded_checkout_testnet(),
            )
            .unwrap_err(),
            PaymentRequestError::UnknownRequiredParameter
        );
    }

    #[test]
    fn zip321_amount_memo_and_required_amount_boundaries_fail_closed() {
        let cases = protocol_cases();
        assert!(
            validate_payment_request(
                &cases.zip321.amount_at_protocol_max,
                PaymentRequestPolicy::protocol_compatible(ZcashNetwork::Testnet),
            )
            .is_ok()
        );
        for invalid in [
            &cases.zip321.amount_above_protocol_max,
            &cases.zip321.amount_too_precise,
        ] {
            assert_eq!(
                validate_payment_request(
                    invalid,
                    PaymentRequestPolicy::protocol_compatible(ZcashNetwork::Testnet),
                )
                .unwrap_err(),
                PaymentRequestError::InvalidRequest
            );
        }
        assert_eq!(
            validate_payment_request(
                &cases.zip321.amount_at_protocol_max,
                PaymentRequestPolicy::shielded_checkout_testnet(),
            )
            .unwrap_err(),
            PaymentRequestError::AmountLimitExceeded
        );
        assert_eq!(
            validate_payment_request(
                &cases.zip321.transparent_memo,
                PaymentRequestPolicy::protocol_compatible(ZcashNetwork::Testnet),
            )
            .unwrap_err(),
            PaymentRequestError::MemoNotAllowed
        );
        assert_eq!(
            validate_payment_request(
                &cases.zip321.amount_missing,
                PaymentRequestPolicy::shielded_checkout_testnet(),
            )
            .unwrap_err(),
            PaymentRequestError::AmountRequired
        );
        assert!(
            validate_payment_request(
                &cases.zip321.amount_missing,
                PaymentRequestPolicy::protocol_compatible(ZcashNetwork::Testnet),
            )
            .is_ok()
        );
    }

    #[test]
    fn viewing_key_boundary_accepts_view_only_authority_and_rejects_spending_material() {
        let ufvk = Ufvk::try_from_items(vec![Fvk::Orchard([1; 96]), Fvk::Sapling([2; 128])])
            .expect("synthetic reviewed UFVK");
        let encoded_ufvk = ufvk.encode(&NetworkType::Test);
        let full =
            inspect_viewing_key(&encoded_ufvk, ZcashNetwork::Testnet).expect("valid testnet UFVK");
        assert_eq!(full.authority, ViewingAuthority::Full);
        assert!(full.can_view_incoming);
        assert!(full.can_view_outgoing);
        assert!(!full.can_spend);
        assert_eq!(
            inspect_viewing_key(&encoded_ufvk, ZcashNetwork::Mainnet),
            Err(ViewingKeyError::WrongNetwork)
        );

        let uivk =
            Uivk::try_from_items(vec![Ivk::Orchard([3; 64])]).expect("synthetic reviewed UIVK");
        let encoded_uivk = uivk.encode(&NetworkType::Test);
        let incoming =
            inspect_viewing_key(&encoded_uivk, ZcashNetwork::Testnet).expect("valid testnet UIVK");
        assert_eq!(incoming.authority, ViewingAuthority::Incoming);
        assert!(incoming.can_view_incoming);
        assert!(!incoming.can_view_outgoing);
        assert!(!incoming.can_spend);

        assert_eq!(
            inspect_viewing_key("secret-extended-key-main1forbidden", ZcashNetwork::Mainnet,),
            Err(ViewingKeyError::SpendingMaterialRejected)
        );
    }

    #[test]
    fn lifecycle_corpus_covers_every_required_state() {
        let fixtures = lifecycle_cases();
        let mut states = BTreeSet::new();
        for fixture in &fixtures {
            let actual = evaluate_lifecycle(fixture).expect("valid lifecycle fixture");
            assert_eq!(actual, fixture.expected_state, "{}", fixture.case_id);
            assert!(verify_lifecycle(fixture).valid);
            states.insert(actual);
        }
        assert_eq!(
            states,
            BTreeSet::from([
                PaymentLifecycleState::Pending,
                PaymentLifecycleState::Confirmed,
                PaymentLifecycleState::Reorged,
                PaymentLifecycleState::Duplicated,
                PaymentLifecycleState::Expired,
                PaymentLifecycleState::Mismatched,
            ])
        );
    }

    #[test]
    fn verifier_errors_and_reports_redact_sensitive_inputs() {
        let sensitive = "private-address-and-memo-sentinel";
        let address_error = inspect_address(
            sensitive,
            ZcashNetwork::Testnet,
            ReceiverPolicy::shielded_checkout(),
        )
        .unwrap_err();
        assert!(!format!("{address_error:?}").contains(sensitive));
        assert!(!address_error.to_string().contains(sensitive));

        let uri = format!("zcash:{sensitive}?message={sensitive}");
        let report =
            verify_payment_request(&uri, PaymentRequestPolicy::shielded_checkout_testnet());
        let encoded_report = serde_json::to_string(&report).expect("serializable report");
        assert!(!encoded_report.contains(sensitive));

        let viewing_error = inspect_viewing_key(sensitive, ZcashNetwork::Testnet).unwrap_err();
        assert!(!format!("{viewing_error:?}").contains(sensitive));
        assert!(!viewing_error.to_string().contains(sensitive));
    }

    proptest! {
        #[test]
        fn arbitrary_ascii_parser_inputs_never_panic_or_leak(
            input in proptest::string::string_regex("[ -~]{0,512}").expect("valid regex")
        ) {
            let address_report = verify_address(
                &input,
                ZcashNetwork::Testnet,
                ReceiverPolicy::shielded_checkout(),
            );
            let payment_report = verify_payment_request(
                &input,
                PaymentRequestPolicy::shielded_checkout_testnet(),
            );
            let reports = serde_json::to_string(&(address_report, payment_report))
                .expect("reports serialize");
            if input.len() >= 24 {
                prop_assert!(!reports.contains(&input));
            }
        }

        #[test]
        fn bounded_zip321_amounts_round_trip(amount in 1u64..=MAX_LAB_TOTAL_ZATOSHIS) {
            let cases = protocol_cases();
            let address = ZcashAddress::try_from_encoded(&cases.testnet_sapling_address)
                .expect("official test address");
            let zatoshis = Zatoshis::from_u64(amount).expect("bounded amount");
            let request = TransactionRequest::new(vec![Payment::without_memo(address, zatoshis)])
                .expect("valid generated request");
            let uri = request.to_uri();
            let validated = validate_payment_request(
                &uri,
                PaymentRequestPolicy::shielded_checkout_testnet(),
            )
            .expect("generated request validates");
            prop_assert_eq!(validated.summary().total_zatoshis, Some(amount));
        }
    }
}
