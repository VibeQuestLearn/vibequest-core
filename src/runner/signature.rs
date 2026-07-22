use base64::Engine;
use ring::{
    rand::SystemRandom,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use std::collections::BTreeMap;
use thiserror::Error;

use super::{AuthenticatedRunnerResult, RunnerEvidence, RunnerProtocolError};

pub struct RunnerResultSigner {
    key_id: String,
    key_pair: Ed25519KeyPair,
}

impl RunnerResultSigner {
    pub fn from_config(config: &str) -> Result<Self, SignatureConfigError> {
        let (key_id, encoded_key) = split_config(config)?;
        let bytes = decode_base64url(encoded_key)?;
        let key_pair =
            Ed25519KeyPair::from_pkcs8(&bytes).map_err(|_| SignatureConfigError::InvalidKey)?;
        Ok(Self {
            key_id: key_id.to_string(),
            key_pair,
        })
    }

    pub fn generate_for_tests() -> Result<(Self, RunnerResultVerifier), SignatureConfigError> {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| SignatureConfigError::InvalidKey)?;
        let key_pair = Ed25519KeyPair::from_pkcs8(document.as_ref())
            .map_err(|_| SignatureConfigError::InvalidKey)?;
        let public_key = key_pair.public_key().as_ref().to_vec();
        let signer = Self {
            key_id: "runner-test".to_string(),
            key_pair,
        };
        let verifier = RunnerResultVerifier {
            keys: BTreeMap::from([("runner-test".to_string(), public_key)]),
        };
        Ok((signer, verifier))
    }

    pub fn public_key_config(&self) -> String {
        format!(
            "{}:{}",
            self.key_id,
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(self.key_pair.public_key().as_ref())
        )
    }

    pub fn sign(
        &self,
        mut evidence: RunnerEvidence,
    ) -> Result<AuthenticatedRunnerResult, SignatureConfigError> {
        evidence
            .finalize_digest()
            .map_err(SignatureConfigError::Protocol)?;
        let payload =
            serde_json::to_vec(&evidence).map_err(|_| SignatureConfigError::Serialization)?;
        let signature = self.key_pair.sign(&payload);
        Ok(AuthenticatedRunnerResult {
            evidence,
            key_id: self.key_id.clone(),
            signature: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.as_ref()),
        })
    }
}

#[derive(Clone)]
pub struct RunnerResultVerifier {
    keys: BTreeMap<String, Vec<u8>>,
}

impl RunnerResultVerifier {
    pub fn from_config(config: &str) -> Result<Self, SignatureConfigError> {
        let mut keys = BTreeMap::new();
        for entry in config
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let (key_id, encoded_key) = split_config(entry)?;
            if keys
                .insert(key_id.to_string(), decode_base64url(encoded_key)?)
                .is_some()
            {
                return Err(SignatureConfigError::DuplicateKey);
            }
        }
        if keys.is_empty() {
            return Err(SignatureConfigError::MissingKey);
        }
        Ok(Self { keys })
    }

    pub fn verify(&self, result: &AuthenticatedRunnerResult) -> Result<(), SignatureConfigError> {
        result
            .evidence
            .verify_digest()
            .map_err(SignatureConfigError::Protocol)?;
        let public_key = self
            .keys
            .get(&result.key_id)
            .ok_or(SignatureConfigError::UnknownKey)?;
        let payload = serde_json::to_vec(&result.evidence)
            .map_err(|_| SignatureConfigError::Serialization)?;
        let signature_bytes = decode_base64url(&result.signature)?;
        signature::UnparsedPublicKey::new(&signature::ED25519, public_key)
            .verify(&payload, &signature_bytes)
            .map_err(|_| SignatureConfigError::InvalidSignature)
    }
}

#[derive(Debug, Error)]
pub enum SignatureConfigError {
    #[error("runner result key configuration is missing")]
    MissingKey,
    #[error("runner result key identifier is invalid")]
    InvalidKeyId,
    #[error("runner result key is invalid")]
    InvalidKey,
    #[error("runner result key is duplicated")]
    DuplicateKey,
    #[error("runner result key identifier is unknown")]
    UnknownKey,
    #[error("runner result signature is invalid")]
    InvalidSignature,
    #[error("runner result encoding is invalid")]
    InvalidEncoding,
    #[error("runner result serialization failed")]
    Serialization,
    #[error(transparent)]
    Protocol(#[from] RunnerProtocolError),
}

fn split_config(config: &str) -> Result<(&str, &str), SignatureConfigError> {
    let (key_id, key) = config
        .split_once(':')
        .ok_or(SignatureConfigError::InvalidKey)?;
    if key_id.is_empty()
        || key_id.len() > 32
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SignatureConfigError::InvalidKeyId);
    }
    if key.is_empty() {
        return Err(SignatureConfigError::InvalidKey);
    }
    Ok((key_id, key))
}

fn decode_base64url(value: &str) -> Result<Vec<u8>, SignatureConfigError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| SignatureConfigError::InvalidEncoding)
}
