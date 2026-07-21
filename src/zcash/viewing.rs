use serde::{Deserialize, Serialize};
use thiserror::Error;
use zcash_address::unified::{Container, Encoding, Fvk, Ivk, Ufvk, Uivk};
use zcash_protocol::{
    consensus::NetworkType,
    constants::{mainnet, regtest, testnet},
};

use super::{
    address::ZcashNetwork,
    report::{RuleResult, VerificationReport},
};

pub const VIEWING_KEY_VERIFIER_ID: &str = "zcash-viewing-key-boundary";
pub const VIEWING_KEY_VERIFIER_VERSION: &str = "1.0.0";
const MAX_VIEWING_KEY_INPUT_BYTES: usize = 8_192;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewingAuthority {
    Incoming,
    Full,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ViewingCapabilities {
    pub orchard: bool,
    pub sapling: bool,
    pub transparent: bool,
    pub unknown_component_count: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ViewingKeyInspection {
    pub network: ZcashNetwork,
    pub authority: ViewingAuthority,
    pub capabilities: ViewingCapabilities,
    pub can_view_incoming: bool,
    pub can_view_outgoing: bool,
    pub can_spend: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ViewingKeyError {
    #[error("viewing key input exceeds the verifier limit")]
    InputTooLong,
    #[error("spending material is forbidden")]
    SpendingMaterialRejected,
    #[error("viewing key is malformed or unsupported")]
    InvalidViewingKey,
    #[error("viewing key is for a different network")]
    WrongNetwork,
}

impl ViewingKeyError {
    fn rule_result(&self) -> RuleResult {
        let (rule_id, message) = match self {
            ViewingKeyError::InputTooLong | ViewingKeyError::InvalidViewingKey => (
                "zip316.viewing-key.parse",
                "Provide a supported Unified Viewing Key.",
            ),
            ViewingKeyError::SpendingMaterialRejected => (
                "zip316.viewing-key.non-custodial",
                "Spending keys and seed material are forbidden.",
            ),
            ViewingKeyError::WrongNetwork => (
                "zip316.viewing-key.network",
                "Use a viewing key for the configured network.",
            ),
        };

        RuleResult::failed(rule_id, vec!["zip-0316", "zcash-address-0.12.0"], message)
    }
}

pub fn inspect_viewing_key(
    encoded: &str,
    expected_network: ZcashNetwork,
) -> Result<ViewingKeyInspection, ViewingKeyError> {
    if encoded.is_empty() || encoded.len() > MAX_VIEWING_KEY_INPUT_BYTES {
        return Err(ViewingKeyError::InputTooLong);
    }
    if spending_prefixes()
        .iter()
        .any(|prefix| encoded.starts_with(prefix))
    {
        return Err(ViewingKeyError::SpendingMaterialRejected);
    }

    if let Ok((network, key)) = Ufvk::decode(encoded) {
        ensure_network(network, expected_network)?;
        return Ok(ViewingKeyInspection {
            network: expected_network,
            authority: ViewingAuthority::Full,
            capabilities: inspect_full_components(key.items()),
            can_view_incoming: true,
            can_view_outgoing: true,
            can_spend: false,
        });
    }
    if let Ok((network, key)) = Uivk::decode(encoded) {
        ensure_network(network, expected_network)?;
        return Ok(ViewingKeyInspection {
            network: expected_network,
            authority: ViewingAuthority::Incoming,
            capabilities: inspect_incoming_components(key.items()),
            can_view_incoming: true,
            can_view_outgoing: false,
            can_spend: false,
        });
    }

    Err(ViewingKeyError::InvalidViewingKey)
}

pub fn verify_viewing_key(encoded: &str, expected_network: ZcashNetwork) -> VerificationReport {
    match inspect_viewing_key(encoded, expected_network) {
        Ok(_) => VerificationReport::passed(
            VIEWING_KEY_VERIFIER_ID,
            VIEWING_KEY_VERIFIER_VERSION,
            vec![
                RuleResult::passed(
                    "zip316.viewing-key.parse",
                    vec!["zip-0316", "zcash-address-0.12.0"],
                    "The Unified Viewing Key encoding is valid.",
                ),
                RuleResult::passed(
                    "zip316.viewing-key.network",
                    vec!["zip-0316", "zcash-protocol-0.9.0"],
                    "The viewing key matches the configured network.",
                ),
                RuleResult::passed(
                    "zip316.viewing-key.non-custodial",
                    vec!["zip-0316"],
                    "The input grants viewing authority and cannot spend funds.",
                ),
            ],
        ),
        Err(error) => VerificationReport::failed(
            VIEWING_KEY_VERIFIER_ID,
            VIEWING_KEY_VERIFIER_VERSION,
            error.rule_result(),
        ),
    }
}

fn ensure_network(actual: NetworkType, expected: ZcashNetwork) -> Result<(), ViewingKeyError> {
    if actual == NetworkType::from(expected) {
        Ok(())
    } else {
        Err(ViewingKeyError::WrongNetwork)
    }
}

fn inspect_full_components(components: Vec<Fvk>) -> ViewingCapabilities {
    let mut capabilities = ViewingCapabilities::default();
    for component in components {
        match component {
            Fvk::Orchard(_) => capabilities.orchard = true,
            Fvk::Sapling(_) => capabilities.sapling = true,
            Fvk::P2pkh(_) => capabilities.transparent = true,
            Fvk::Unknown { .. } => capabilities.unknown_component_count += 1,
        }
    }
    capabilities
}

fn inspect_incoming_components(components: Vec<Ivk>) -> ViewingCapabilities {
    let mut capabilities = ViewingCapabilities::default();
    for component in components {
        match component {
            Ivk::Orchard(_) => capabilities.orchard = true,
            Ivk::Sapling(_) => capabilities.sapling = true,
            Ivk::P2pkh(_) => capabilities.transparent = true,
            Ivk::Unknown { .. } => capabilities.unknown_component_count += 1,
        }
    }
    capabilities
}

fn spending_prefixes() -> [&'static str; 3] {
    [
        mainnet::HRP_SAPLING_EXTENDED_SPENDING_KEY,
        testnet::HRP_SAPLING_EXTENDED_SPENDING_KEY,
        regtest::HRP_SAPLING_EXTENDED_SPENDING_KEY,
    ]
}
