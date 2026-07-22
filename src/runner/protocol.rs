use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    RUNNER_PROTOCOL_VERSION, RUNNER_VERSION, runner_manifest, sha256_hex, validate_runner_manifest,
};

pub const RUNNER_SCENARIO_ID: &str = "shielded-checkout";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSubmissionRequest {
    pub scenario_id: String,
    pub scenario_manifest_version: String,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerJob {
    pub protocol_version: String,
    pub runner_version: String,
    pub job_id: String,
    pub submission_id: String,
    pub user_id: String,
    pub ecosystem_id: String,
    pub track_id: String,
    pub track_version: String,
    pub content_version: String,
    pub scenario_id: String,
    pub scenario_manifest_version: String,
    pub source_digest: String,
    pub test_bundle_digest: String,
    pub source: String,
}

impl RunnerJob {
    pub fn reviewed(
        job_id: String,
        submission_id: String,
        user_id: String,
        source: String,
    ) -> Result<Self, RunnerProtocolError> {
        validate_runner_manifest().map_err(|_| RunnerProtocolError::ArtifactBinding)?;
        let manifest = runner_manifest();
        let job = Self {
            protocol_version: RUNNER_PROTOCOL_VERSION.to_string(),
            runner_version: RUNNER_VERSION.to_string(),
            job_id,
            submission_id,
            user_id,
            ecosystem_id: crate::platform::ZCASH_ECOSYSTEM_ID.to_string(),
            track_id: crate::platform::SHIELDED_PAYMENTS_TRACK_ID.to_string(),
            track_version: "1.0.0".to_string(),
            content_version: "2026-07-21.1".to_string(),
            scenario_id: RUNNER_SCENARIO_ID.to_string(),
            scenario_manifest_version: manifest.scenario_manifest_version.clone(),
            source_digest: sha256_hex(source.as_bytes()),
            test_bundle_digest: manifest.tests.bundle_sha256.clone(),
            source,
        };
        job.validate()?;
        Ok(job)
    }

    pub fn validate(&self) -> Result<(), RunnerProtocolError> {
        let manifest = runner_manifest();
        if self.protocol_version != RUNNER_PROTOCOL_VERSION
            || self.runner_version != RUNNER_VERSION
            || self.ecosystem_id != crate::platform::ZCASH_ECOSYSTEM_ID
            || self.track_id != crate::platform::SHIELDED_PAYMENTS_TRACK_ID
            || self.track_version != "1.0.0"
            || self.content_version != "2026-07-21.1"
            || self.scenario_id != RUNNER_SCENARIO_ID
            || self.scenario_manifest_version != manifest.scenario_manifest_version
            || self.test_bundle_digest != manifest.tests.bundle_sha256
        {
            return Err(RunnerProtocolError::ArtifactBinding);
        }
        if !bounded_id(&self.job_id, "job_", 96)
            || !bounded_id(&self.submission_id, "sub_", 96)
            || !bounded_id(&self.user_id, "usr_", 96)
        {
            return Err(RunnerProtocolError::InvalidIdentifier);
        }
        if self.source.is_empty() || self.source.len() > manifest.source.max_bytes {
            return Err(RunnerProtocolError::SourceLimit);
        }
        if self.source_digest != sha256_hex(self.source.as_bytes()) {
            return Err(RunnerProtocolError::SourceDigest);
        }
        Ok(())
    }

    pub fn binding(&self) -> RunnerBinding {
        RunnerBinding {
            protocol_version: self.protocol_version.clone(),
            runner_version: self.runner_version.clone(),
            job_id: self.job_id.clone(),
            submission_id: self.submission_id.clone(),
            user_id: self.user_id.clone(),
            scenario_id: self.scenario_id.clone(),
            scenario_manifest_version: self.scenario_manifest_version.clone(),
            source_digest: self.source_digest.clone(),
            test_bundle_digest: self.test_bundle_digest.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RunnerBinding {
    pub protocol_version: String,
    pub runner_version: String,
    pub job_id: String,
    pub submission_id: String,
    pub user_id: String,
    pub scenario_id: String,
    pub scenario_manifest_version: String,
    pub source_digest: String,
    pub test_bundle_digest: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerClassification {
    Passed,
    Failed,
    CompileError,
    Timeout,
    OutputLimit,
    ResourceLimit,
    Cancelled,
    IsolationError,
    WorkerLost,
}

impl RunnerClassification {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::WorkerLost)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaseStatus {
    Passed,
    Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PublicCaseResult {
    pub case_id: String,
    pub status: CaseStatus,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct HiddenCaseSummary {
    pub passed: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RunnerEvidence {
    pub binding: RunnerBinding,
    pub classification: RunnerClassification,
    pub public_cases: Vec<PublicCaseResult>,
    pub hidden_summary: HiddenCaseSummary,
    pub worker_attempts: u8,
    pub output_bytes: usize,
    pub output_truncated: bool,
    pub diagnostic_code: String,
    pub result_digest: String,
}

impl RunnerEvidence {
    pub fn finalize_digest(&mut self) -> Result<(), RunnerProtocolError> {
        self.result_digest = deterministic_result_digest(self)?;
        Ok(())
    }

    pub fn verify_digest(&self) -> Result<(), RunnerProtocolError> {
        if self.result_digest != deterministic_result_digest(self)? {
            return Err(RunnerProtocolError::ResultDigest);
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct DeterministicEvidence<'a> {
    binding: &'a RunnerBinding,
    classification: RunnerClassification,
    public_cases: &'a [PublicCaseResult],
    hidden_summary: &'a HiddenCaseSummary,
    diagnostic_code: &'a str,
}

fn deterministic_result_digest(evidence: &RunnerEvidence) -> Result<String, RunnerProtocolError> {
    let deterministic = DeterministicEvidence {
        binding: &evidence.binding,
        classification: evidence.classification,
        public_cases: &evidence.public_cases,
        hidden_summary: &evidence.hidden_summary,
        diagnostic_code: &evidence.diagnostic_code,
    };
    serde_json::to_vec(&deterministic)
        .map(|encoded| sha256_hex(&encoded))
        .map_err(|_| RunnerProtocolError::Serialization)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AuthenticatedRunnerResult {
    pub evidence: RunnerEvidence,
    pub key_id: String,
    pub signature: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubmissionState {
    Queued,
    Running,
    Passed,
    Failed,
    Cancelled,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunnerSubmissionView {
    pub submission_id: String,
    pub scenario_id: String,
    pub scenario_manifest_version: String,
    pub runner_version: String,
    pub source_digest: String,
    pub test_bundle_digest: String,
    pub state: SubmissionState,
    pub classification: Option<RunnerClassification>,
    pub public_cases: Vec<PublicCaseResult>,
    pub hidden_passed: usize,
    pub hidden_failed: usize,
    pub result_digest: Option<String>,
    pub diagnostic_code: Option<String>,
    pub output_truncated: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RunnerProtocolError {
    #[error("runner artifact binding is invalid")]
    ArtifactBinding,
    #[error("runner identifier is invalid")]
    InvalidIdentifier,
    #[error("runner source exceeds the reviewed boundary")]
    SourceLimit,
    #[error("runner source digest does not match")]
    SourceDigest,
    #[error("runner result digest does not match")]
    ResultDigest,
    #[error("runner protocol serialization failed")]
    Serialization,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct HarnessResult {
    pub protocol_version: String,
    pub classification: HarnessClassification,
    pub public_cases: Vec<PublicCaseResult>,
    pub hidden_summary: HiddenCaseSummary,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HarnessClassification {
    Passed,
    Failed,
    CompileError,
    Timeout,
    SourceLimit,
    ImportDenied,
    DynamicImportDenied,
}

fn bounded_id(value: &str, prefix: &str, max_len: usize) -> bool {
    value.starts_with(prefix)
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
