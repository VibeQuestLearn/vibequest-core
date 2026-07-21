use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleStatus {
    Passed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RuleResult {
    pub rule_id: &'static str,
    pub status: RuleStatus,
    pub source_references: Vec<&'static str>,
    pub message: &'static str,
}

impl RuleResult {
    pub(crate) fn passed(
        rule_id: &'static str,
        source_references: Vec<&'static str>,
        message: &'static str,
    ) -> Self {
        Self {
            rule_id,
            status: RuleStatus::Passed,
            source_references,
            message,
        }
    }

    pub(crate) fn failed(
        rule_id: &'static str,
        source_references: Vec<&'static str>,
        message: &'static str,
    ) -> Self {
        Self {
            rule_id,
            status: RuleStatus::Failed,
            source_references,
            message,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerificationReport {
    pub verifier_id: &'static str,
    pub verifier_version: &'static str,
    pub valid: bool,
    pub rules: Vec<RuleResult>,
}

impl VerificationReport {
    pub(crate) fn passed(
        verifier_id: &'static str,
        verifier_version: &'static str,
        rules: Vec<RuleResult>,
    ) -> Self {
        Self {
            verifier_id,
            verifier_version,
            valid: true,
            rules,
        }
    }

    pub(crate) fn failed(
        verifier_id: &'static str,
        verifier_version: &'static str,
        rule: RuleResult,
    ) -> Self {
        Self {
            verifier_id,
            verifier_version,
            valid: false,
            rules: vec![rule],
        }
    }
}
