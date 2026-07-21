use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use thiserror::Error;
use zcash_address::{
    ConversionError, TryFromAddress, ZcashAddress,
    unified::{self, Container},
};
use zcash_protocol::consensus::NetworkType;

use super::report::{RuleResult, VerificationReport};

pub const ADDRESS_VERIFIER_ID: &str = "zcash-address-policy";
pub const ADDRESS_VERIFIER_VERSION: &str = "1.0.0";
const MAX_ADDRESS_INPUT_BYTES: usize = 2_048;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ZcashNetwork {
    Mainnet,
    Testnet,
}

impl From<ZcashNetwork> for NetworkType {
    fn from(value: ZcashNetwork) -> Self {
        match value {
            ZcashNetwork::Mainnet => NetworkType::Main,
            ZcashNetwork::Testnet => NetworkType::Test,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ParsedAddressKind {
    Unified,
    Sapling,
    TransparentP2pkh,
    TransparentP2sh,
    TransparentSourceRestricted,
    Sprout,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReceiverKind {
    Orchard,
    Sapling,
    Transparent,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ReceiverCapabilities {
    pub orchard: bool,
    pub sapling: bool,
    pub transparent: bool,
    pub unknown_receiver_count: usize,
}

impl ReceiverCapabilities {
    pub fn has_shielded(&self) -> bool {
        self.orchard || self.sapling
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ReceiverPolicy {
    pub require_unified: bool,
    pub require_shielded: bool,
    pub support_orchard: bool,
    pub support_sapling: bool,
    pub forbid_transparent_receiver: bool,
    pub allow_unknown_receivers: bool,
}

impl ReceiverPolicy {
    pub const fn shielded_checkout() -> Self {
        Self {
            require_unified: true,
            require_shielded: true,
            support_orchard: true,
            support_sapling: true,
            forbid_transparent_receiver: false,
            allow_unknown_receivers: false,
        }
    }

    pub const fn shielded_recipient() -> Self {
        Self {
            require_unified: false,
            ..Self::shielded_checkout()
        }
    }

    pub const fn protocol_compatible() -> Self {
        Self {
            require_unified: false,
            require_shielded: false,
            support_orchard: true,
            support_sapling: true,
            forbid_transparent_receiver: false,
            allow_unknown_receivers: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AddressInspection {
    pub network: ZcashNetwork,
    pub kind: ParsedAddressKind,
    pub receivers: ReceiverCapabilities,
    pub preferred_supported_receiver: Option<ReceiverKind>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AddressError {
    #[error("address input exceeds the verifier limit")]
    InputTooLong,
    #[error("address is malformed or unsupported")]
    InvalidAddress,
    #[error("address is for a different network")]
    WrongNetwork,
    #[error("this lab requires a Unified Address")]
    UnifiedRequired,
    #[error("Sprout addresses are outside this lab boundary")]
    SproutUnsupported,
    #[error("the address has no supported shielded receiver")]
    ShieldedReceiverRequired,
    #[error("the receiver policy forbids transparent receivers")]
    TransparentReceiverForbidden,
    #[error("the receiver policy does not allow unknown receivers")]
    UnknownReceiverUnsupported,
}

impl AddressError {
    fn rule_result(&self) -> RuleResult {
        let (rule_id, sources) = match self {
            AddressError::InputTooLong | AddressError::InvalidAddress => (
                "zip316.address.parse",
                vec!["zcash-address-0.12.0", "zip-0316"],
            ),
            AddressError::WrongNetwork => (
                "zip316.address.network",
                vec!["zcash-address-0.12.0", "zcash-protocol-0.9.0"],
            ),
            AddressError::UnifiedRequired
            | AddressError::SproutUnsupported
            | AddressError::ShieldedReceiverRequired
            | AddressError::TransparentReceiverForbidden
            | AddressError::UnknownReceiverUnsupported => {
                ("zip316.receiver.policy", vec!["zip-0316"])
            }
        };

        RuleResult::failed(rule_id, sources, self.safe_message())
    }

    fn safe_message(&self) -> &'static str {
        match self {
            AddressError::InputTooLong => "Address input exceeds the lab limit.",
            AddressError::InvalidAddress => "Provide a valid supported Zcash address.",
            AddressError::WrongNetwork => "Use an address for the configured network.",
            AddressError::UnifiedRequired => "Use a Unified Address for this scenario.",
            AddressError::SproutUnsupported => "Sprout addresses are not supported.",
            AddressError::ShieldedReceiverRequired => {
                "The address must contain a supported shielded receiver."
            }
            AddressError::TransparentReceiverForbidden => {
                "This scenario does not allow a transparent receiver."
            }
            AddressError::UnknownReceiverUnsupported => {
                "This scenario does not allow unknown receiver types."
            }
        }
    }
}

pub fn inspect_address(
    encoded: &str,
    expected_network: ZcashNetwork,
    policy: ReceiverPolicy,
) -> Result<AddressInspection, AddressError> {
    if encoded.is_empty() || encoded.len() > MAX_ADDRESS_INPUT_BYTES {
        return Err(AddressError::InputTooLong);
    }
    let address =
        ZcashAddress::try_from_encoded(encoded).map_err(|_| AddressError::InvalidAddress)?;

    inspect_parsed_address(address, expected_network, policy)
}

pub(crate) fn inspect_parsed_address(
    address: ZcashAddress,
    expected_network: ZcashNetwork,
    policy: ReceiverPolicy,
) -> Result<AddressInspection, AddressError> {
    let capabilities = match address
        .convert_if_network::<ParsedCapabilities>(expected_network.into())
    {
        Ok(value) => value,
        Err(ConversionError::IncorrectNetwork { .. }) => return Err(AddressError::WrongNetwork),
        Err(ConversionError::Unsupported(_)) => return Err(AddressError::InvalidAddress),
        Err(ConversionError::User(value)) => match value {},
    };

    apply_policy(capabilities, expected_network, policy)
}

pub fn verify_address(
    encoded: &str,
    expected_network: ZcashNetwork,
    policy: ReceiverPolicy,
) -> VerificationReport {
    match inspect_address(encoded, expected_network, policy) {
        Ok(_) => VerificationReport::passed(
            ADDRESS_VERIFIER_ID,
            ADDRESS_VERIFIER_VERSION,
            vec![
                RuleResult::passed(
                    "zip316.address.parse",
                    vec!["zcash-address-0.12.0", "zip-0316"],
                    "The address uses a supported canonical Zcash encoding.",
                ),
                RuleResult::passed(
                    "zip316.address.network",
                    vec!["zcash-address-0.12.0", "zcash-protocol-0.9.0"],
                    "The address matches the configured network.",
                ),
                RuleResult::passed(
                    "zip316.receiver.policy",
                    vec!["zip-0316"],
                    "The receiver set satisfies the scenario policy.",
                ),
            ],
        ),
        Err(error) => VerificationReport::failed(
            ADDRESS_VERIFIER_ID,
            ADDRESS_VERIFIER_VERSION,
            error.rule_result(),
        ),
    }
}

#[derive(Clone, Debug)]
struct ParsedCapabilities {
    kind: ParsedAddressKind,
    receivers: ReceiverCapabilities,
}

impl TryFromAddress for ParsedCapabilities {
    type Error = Infallible;

    fn try_from_sprout(
        _network: NetworkType,
        _data: [u8; 64],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self {
            kind: ParsedAddressKind::Sprout,
            receivers: ReceiverCapabilities::default(),
        })
    }

    fn try_from_sapling(
        _network: NetworkType,
        _data: [u8; 43],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self {
            kind: ParsedAddressKind::Sapling,
            receivers: ReceiverCapabilities {
                sapling: true,
                ..ReceiverCapabilities::default()
            },
        })
    }

    fn try_from_unified(
        _network: NetworkType,
        address: unified::Address,
    ) -> Result<Self, ConversionError<Self::Error>> {
        let mut receivers = ReceiverCapabilities::default();
        for receiver in address.items() {
            match receiver {
                unified::Receiver::Orchard(_) => receivers.orchard = true,
                unified::Receiver::Sapling(_) => receivers.sapling = true,
                unified::Receiver::P2pkh(_) | unified::Receiver::P2sh(_) => {
                    receivers.transparent = true;
                }
                unified::Receiver::Unknown { .. } => receivers.unknown_receiver_count += 1,
            }
        }

        Ok(Self {
            kind: ParsedAddressKind::Unified,
            receivers,
        })
    }

    fn try_from_transparent_p2pkh(
        _network: NetworkType,
        _data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(transparent_capabilities(
            ParsedAddressKind::TransparentP2pkh,
        ))
    }

    fn try_from_transparent_p2sh(
        _network: NetworkType,
        _data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(transparent_capabilities(ParsedAddressKind::TransparentP2sh))
    }

    fn try_from_tex(
        _network: NetworkType,
        _data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(transparent_capabilities(
            ParsedAddressKind::TransparentSourceRestricted,
        ))
    }
}

fn transparent_capabilities(kind: ParsedAddressKind) -> ParsedCapabilities {
    ParsedCapabilities {
        kind,
        receivers: ReceiverCapabilities {
            transparent: true,
            ..ReceiverCapabilities::default()
        },
    }
}

fn apply_policy(
    parsed: ParsedCapabilities,
    network: ZcashNetwork,
    policy: ReceiverPolicy,
) -> Result<AddressInspection, AddressError> {
    if policy.require_unified && parsed.kind != ParsedAddressKind::Unified {
        return Err(AddressError::UnifiedRequired);
    }
    if parsed.kind == ParsedAddressKind::Sprout {
        return Err(AddressError::SproutUnsupported);
    }
    if policy.forbid_transparent_receiver && parsed.receivers.transparent {
        return Err(AddressError::TransparentReceiverForbidden);
    }
    if !policy.allow_unknown_receivers && parsed.receivers.unknown_receiver_count > 0 {
        return Err(AddressError::UnknownReceiverUnsupported);
    }

    let preferred_supported_receiver = if policy.support_orchard && parsed.receivers.orchard {
        Some(ReceiverKind::Orchard)
    } else if policy.support_sapling && parsed.receivers.sapling {
        Some(ReceiverKind::Sapling)
    } else if !policy.require_shielded && parsed.receivers.transparent {
        Some(ReceiverKind::Transparent)
    } else {
        None
    };

    if policy.require_shielded
        && !matches!(
            preferred_supported_receiver,
            Some(ReceiverKind::Orchard | ReceiverKind::Sapling)
        )
    {
        return Err(AddressError::ShieldedReceiverRequired);
    }

    Ok(AddressInspection {
        network,
        kind: parsed.kind,
        receivers: parsed.receivers,
        preferred_supported_receiver,
    })
}
