use crate::{
    platform::{SHIELDED_PAYMENTS_TRACK_ID, ZCASH_ECOSYSTEM_ID},
    zcash::{SOURCE_MANIFEST_VERSION, source_manifest},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, OnceLock, RwLock},
};
use thiserror::Error;

pub const CURRICULUM_VERSION: &str = "zcash-shielded-payments-1.0.0";
pub const SCENARIO_MANIFEST_VERSION: &str = "shielded-checkout-scenarios-1.0.0";
pub const TUTOR_CONTRACT_VERSION: &str = "zcash-reviewed-tutor-1.0.0";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewerStatus {
    Reviewed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CurriculumManifest {
    pub curriculum_version: String,
    pub ecosystem_id: String,
    pub track_id: String,
    pub track_version: String,
    pub content_version: String,
    pub source_manifest_version: String,
    pub scenario_manifest_version: String,
    pub tutor_contract_version: String,
    pub reviewer_status: ReviewerStatus,
    pub lessons: Vec<Lesson>,
    pub capstone: Capstone,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Lesson {
    pub lesson_id: String,
    pub sequence: u8,
    pub title: String,
    pub learner_outcome: String,
    pub explainer: Vec<String>,
    pub code_lens: CodeLens,
    pub source_references: Vec<String>,
    pub misconception: String,
    pub denial_case_ids: Vec<String>,
    pub checkpoint: Checkpoint,
    pub lab_bridge: String,
    pub scenario_step_id: String,
    pub content_version: String,
    pub reviewer_status: ReviewerStatus,
    pub tutor_anchors: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CodeLens {
    pub path: String,
    pub symbol: String,
    pub language: String,
    pub snippet: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Checkpoint {
    pub checkpoint_id: String,
    pub prompt: String,
    pub options: Vec<CheckpointOption>,
    pub correct_option_id: String,
    pub rationale: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CheckpointOption {
    pub option_id: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Capstone {
    pub capstone_id: String,
    pub title: String,
    pub objective: String,
    pub required_repairs: Vec<String>,
    pub required_explanations: Vec<String>,
    pub completion_evidence: Vec<String>,
    pub multiple_choice_sufficient: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaseVisibility {
    Public,
    Hidden,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExpectedOutcome {
    Accepted,
    Denied,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ScenarioManifest {
    pub scenario_manifest_version: String,
    pub curriculum_version: String,
    pub source_manifest_version: String,
    pub codebase_id: String,
    pub language: String,
    pub root_path: String,
    pub allowlisted_edit_locations: Vec<EditLocation>,
    pub seeded_defects: Vec<SeededDefect>,
    pub steps: Vec<ScenarioStep>,
    pub cases: Vec<ScenarioCase>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct EditLocation {
    pub path: String,
    pub symbol: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SeededDefect {
    pub defect_id: String,
    pub lesson_id: String,
    pub location: EditLocation,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ScenarioStep {
    pub step_id: String,
    pub lesson_id: String,
    pub entrypoint: String,
    pub verifier_ids: Vec<String>,
    pub valid_case_ids: Vec<String>,
    pub denial_case_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ScenarioCase {
    pub case_id: String,
    pub lesson_id: String,
    pub visibility: CaseVisibility,
    pub expected_outcome: ExpectedOutcome,
    pub operation: String,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TutorMode {
    Explanation,
    Hint,
    Remediation,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct TutorContract {
    pub tutor_contract_version: String,
    pub curriculum_version: String,
    pub scenario_manifest_version: String,
    pub source_manifest_version: String,
    pub allowed_modes: Vec<TutorMode>,
    pub max_prompt_chars: usize,
    pub max_test_output_chars: usize,
    pub max_answer_chars: usize,
    pub forbidden_generic_phrases: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublicCurriculum {
    pub curriculum_version: String,
    pub ecosystem_id: String,
    pub track_id: String,
    pub track_version: String,
    pub content_version: String,
    pub source_manifest_version: String,
    pub scenario_manifest_version: String,
    pub tutor_contract_version: String,
    pub reviewer_status: ReviewerStatus,
    pub lessons: Vec<PublicLesson>,
    pub sources: Vec<PublicSource>,
    pub capstone: PublicCapstone,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublicLesson {
    pub lesson_id: String,
    pub sequence: u8,
    pub title: String,
    pub learner_outcome: String,
    pub explainer: Vec<String>,
    pub code_lens: CodeLens,
    pub source_references: Vec<String>,
    pub misconception: String,
    pub checkpoint: PublicCheckpoint,
    pub lab_bridge: String,
    pub content_version: String,
    pub reviewer_status: ReviewerStatus,
    pub valid_case_count: usize,
    pub denial_case_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublicCheckpoint {
    pub checkpoint_id: String,
    pub prompt: String,
    pub options: Vec<CheckpointOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublicSource {
    pub source_id: String,
    pub title: String,
    pub version: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublicCapstone {
    pub capstone_id: String,
    pub title: String,
    pub objective: String,
    pub repair_count: usize,
    pub explanation_count: usize,
    pub completion_evidence: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TutorContext {
    pub lesson_id: String,
    pub scenario_manifest_version: String,
    pub mode: TutorMode,
    pub prompt: String,
    pub test_output: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TutorArtifactOrigin {
    Provider,
    ReviewedFallback,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct TutorArtifact {
    pub lesson_id: String,
    pub scenario_manifest_version: String,
    pub mode: TutorMode,
    pub answer: String,
    pub source_references: Vec<String>,
    pub next_question: String,
    pub origin: TutorArtifactOrigin,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("reviewed curriculum artifact is invalid: {0}")]
pub struct CurriculumValidationError(String);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TutorValidationError {
    #[error("tutor context is outside the reviewed lesson boundary")]
    InvalidContext,
    #[error("tutor response cites a source outside the reviewed lesson")]
    UnsupportedCitation,
    #[error("tutor response is generic or ungrounded")]
    GenericResponse,
    #[error("tutor response exceeds the bounded contract")]
    ResponseTooLong,
    #[error("tutor artifact cache is unavailable")]
    CacheUnavailable,
}

pub fn curriculum_manifest() -> &'static CurriculumManifest {
    static MANIFEST: OnceLock<CurriculumManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../fixtures/zcash/v1/curriculum-manifest.json"
        ))
        .expect("the reviewed curriculum manifest must be valid JSON")
    })
}

pub fn scenario_manifest() -> &'static ScenarioManifest {
    static MANIFEST: OnceLock<ScenarioManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(include_str!("../fixtures/zcash/v1/scenario-manifest.json"))
            .expect("the reviewed scenario manifest must be valid JSON")
    })
}

pub fn tutor_contract() -> &'static TutorContract {
    static CONTRACT: OnceLock<TutorContract> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        serde_json::from_str(include_str!("../fixtures/zcash/v1/tutor-contract.json"))
            .expect("the reviewed tutor contract must be valid JSON")
    })
}

pub fn validate_reviewed_artifacts() -> Result<(), CurriculumValidationError> {
    let curriculum = curriculum_manifest();
    let scenarios = scenario_manifest();
    let tutor = tutor_contract();
    let sources = source_manifest();

    ensure(
        curriculum.curriculum_version == CURRICULUM_VERSION
            && scenarios.curriculum_version == CURRICULUM_VERSION
            && tutor.curriculum_version == CURRICULUM_VERSION,
        "curriculum versions do not match",
    )?;
    ensure(
        curriculum.scenario_manifest_version == SCENARIO_MANIFEST_VERSION
            && scenarios.scenario_manifest_version == SCENARIO_MANIFEST_VERSION
            && tutor.scenario_manifest_version == SCENARIO_MANIFEST_VERSION,
        "scenario manifest versions do not match",
    )?;
    ensure(
        curriculum.source_manifest_version == SOURCE_MANIFEST_VERSION
            && scenarios.source_manifest_version == SOURCE_MANIFEST_VERSION
            && tutor.source_manifest_version == SOURCE_MANIFEST_VERSION,
        "source manifest versions do not match",
    )?;
    ensure(
        curriculum.tutor_contract_version == TUTOR_CONTRACT_VERSION
            && tutor.tutor_contract_version == TUTOR_CONTRACT_VERSION,
        "tutor contract versions do not match",
    )?;
    ensure(
        curriculum.ecosystem_id == ZCASH_ECOSYSTEM_ID
            && curriculum.track_id == SHIELDED_PAYMENTS_TRACK_ID,
        "curriculum namespace is not the registered Zcash track",
    )?;
    ensure(
        curriculum.lessons.len() == 5,
        "curriculum must have five lessons",
    )?;
    ensure(scenarios.steps.len() == 5, "scenario must have five steps")?;

    let source_ids = sources
        .sources
        .iter()
        .map(|source| source.source_id.as_str())
        .collect::<BTreeSet<_>>();
    let case_by_id = unique_map(
        scenarios
            .cases
            .iter()
            .map(|case| (case.case_id.as_str(), case)),
        "scenario case IDs are duplicated",
    )?;
    let step_by_id = unique_map(
        scenarios
            .steps
            .iter()
            .map(|step| (step.step_id.as_str(), step)),
        "scenario step IDs are duplicated",
    )?;
    let lesson_ids = curriculum
        .lessons
        .iter()
        .map(|lesson| lesson.lesson_id.as_str())
        .collect::<BTreeSet<_>>();
    ensure(lesson_ids.len() == 5, "lesson IDs are duplicated")?;

    let solution = include_str!("../scenarios/zcash/shielded-checkout/v1/solution/src/checkout.ts");
    for (index, lesson) in curriculum.lessons.iter().enumerate() {
        ensure(
            lesson.sequence as usize == index + 1,
            "lesson sequence is not contiguous",
        )?;
        ensure(
            lesson.reviewer_status == ReviewerStatus::Reviewed
                && lesson.content_version == curriculum.content_version,
            "lesson version or review status is inconsistent",
        )?;
        ensure(
            !lesson.source_references.is_empty()
                && lesson
                    .source_references
                    .iter()
                    .all(|source| source_ids.contains(source.as_str())),
            "lesson contains an unsupported source reference",
        )?;
        ensure(
            lesson.checkpoint.options.len() == 4
                && lesson
                    .checkpoint
                    .options
                    .iter()
                    .any(|option| option.option_id == lesson.checkpoint.correct_option_id),
            "checkpoint answer key is invalid",
        )?;
        ensure(
            lesson
                .checkpoint
                .options
                .iter()
                .map(|option| option.option_id.as_str())
                .collect::<BTreeSet<_>>()
                .len()
                == 4,
            "checkpoint option IDs are duplicated",
        )?;
        ensure(
            solution.contains(&lesson.code_lens.snippet),
            "code lens is not present in the reviewed scenario solution",
        )?;

        let step = step_by_id
            .get(lesson.scenario_step_id.as_str())
            .ok_or_else(|| invalid("lesson scenario step is missing"))?;
        ensure(
            step.lesson_id == lesson.lesson_id
                && !step.valid_case_ids.is_empty()
                && step.denial_case_ids.len() >= 2,
            "scenario step does not meet lesson case coverage",
        )?;
        for case_id in step.valid_case_ids.iter().chain(&step.denial_case_ids) {
            let case = case_by_id
                .get(case_id.as_str())
                .ok_or_else(|| invalid("scenario step references an unknown case"))?;
            ensure(
                case.lesson_id == lesson.lesson_id,
                "scenario case is mapped to the wrong lesson",
            )?;
        }
        ensure(
            lesson
                .denial_case_ids
                .iter()
                .all(|case_id| step.denial_case_ids.contains(case_id)),
            "lesson denial trail is not mapped to its scenario step",
        )?;
    }

    let allowlist = scenarios
        .allowlisted_edit_locations
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    ensure(
        allowlist.len() == scenarios.allowlisted_edit_locations.len(),
        "allowlisted edit locations are duplicated",
    )?;
    ensure(
        scenarios.seeded_defects.len() == 5
            && scenarios.seeded_defects.iter().all(|defect| {
                allowlist.contains(&defect.location)
                    && lesson_ids.contains(defect.lesson_id.as_str())
            }),
        "a seeded defect is outside the reviewed allowlist",
    )?;
    ensure(
        !curriculum.capstone.multiple_choice_sufficient
            && scenarios.seeded_defects.iter().all(|defect| {
                curriculum
                    .capstone
                    .required_repairs
                    .contains(&defect.defect_id)
            })
            && curriculum.capstone.required_explanations.len() >= 5,
        "capstone does not require code repair and boundary explanations",
    )?;
    ensure(
        tutor.allowed_modes
            == vec![
                TutorMode::Explanation,
                TutorMode::Hint,
                TutorMode::Remediation,
            ],
        "tutor modes are not the reviewed set",
    )?;

    let serialized = serde_json::to_string(curriculum)
        .map_err(|_| invalid("curriculum cannot be inspected for prohibited claims"))?
        .to_ascii_lowercase();
    for prohibited in [
        "google authentication proves a zcash payment",
        "paste your spending key",
        "provide your spending key",
        "enter your seed phrase",
    ] {
        ensure(
            !serialized.contains(prohibited),
            "curriculum contains a prohibited custody or identity claim",
        )?;
    }

    Ok(())
}

pub fn public_curriculum() -> Result<PublicCurriculum, CurriculumValidationError> {
    validate_reviewed_artifacts()?;
    let curriculum = curriculum_manifest();
    let scenarios = scenario_manifest();
    let referenced_sources = curriculum
        .lessons
        .iter()
        .flat_map(|lesson| lesson.source_references.iter().cloned())
        .collect::<BTreeSet<_>>();
    let sources = source_manifest()
        .sources
        .iter()
        .filter(|source| referenced_sources.contains(&source.source_id))
        .map(|source| PublicSource {
            source_id: source.source_id.clone(),
            title: source.title.clone(),
            version: source.version.clone(),
            url: source.url.clone(),
        })
        .collect();

    let lessons = curriculum
        .lessons
        .iter()
        .map(|lesson| {
            let step = scenarios
                .steps
                .iter()
                .find(|step| step.step_id == lesson.scenario_step_id)
                .expect("validated scenario step");
            PublicLesson {
                lesson_id: lesson.lesson_id.clone(),
                sequence: lesson.sequence,
                title: lesson.title.clone(),
                learner_outcome: lesson.learner_outcome.clone(),
                explainer: lesson.explainer.clone(),
                code_lens: lesson.code_lens.clone(),
                source_references: lesson.source_references.clone(),
                misconception: lesson.misconception.clone(),
                checkpoint: PublicCheckpoint {
                    checkpoint_id: lesson.checkpoint.checkpoint_id.clone(),
                    prompt: lesson.checkpoint.prompt.clone(),
                    options: lesson.checkpoint.options.clone(),
                },
                lab_bridge: lesson.lab_bridge.clone(),
                content_version: lesson.content_version.clone(),
                reviewer_status: lesson.reviewer_status,
                valid_case_count: step.valid_case_ids.len(),
                denial_case_count: step.denial_case_ids.len(),
            }
        })
        .collect();

    Ok(PublicCurriculum {
        curriculum_version: curriculum.curriculum_version.clone(),
        ecosystem_id: curriculum.ecosystem_id.clone(),
        track_id: curriculum.track_id.clone(),
        track_version: curriculum.track_version.clone(),
        content_version: curriculum.content_version.clone(),
        source_manifest_version: curriculum.source_manifest_version.clone(),
        scenario_manifest_version: curriculum.scenario_manifest_version.clone(),
        tutor_contract_version: curriculum.tutor_contract_version.clone(),
        reviewer_status: curriculum.reviewer_status,
        lessons,
        sources,
        capstone: PublicCapstone {
            capstone_id: curriculum.capstone.capstone_id.clone(),
            title: curriculum.capstone.title.clone(),
            objective: curriculum.capstone.objective.clone(),
            repair_count: curriculum.capstone.required_repairs.len(),
            explanation_count: curriculum.capstone.required_explanations.len(),
            completion_evidence: curriculum.capstone.completion_evidence.clone(),
        },
    })
}

#[derive(Clone)]
pub struct TutorArtifactCache {
    artifacts: Arc<RwLock<BTreeMap<String, TutorArtifact>>>,
    max_entries: usize,
}

impl TutorArtifactCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            artifacts: Arc::new(RwLock::new(BTreeMap::new())),
            max_entries: max_entries.max(1),
        }
    }

    pub fn resolve(
        &self,
        context: &TutorContext,
        candidate: Option<TutorArtifact>,
    ) -> Result<TutorArtifact, TutorValidationError> {
        let lesson = validate_tutor_context(context)?;
        let key = tutor_cache_key(context);

        if let Some(artifact) = candidate {
            validate_tutor_artifact(lesson, context, &artifact)?;
            self.insert(key, artifact.clone())?;
            return Ok(artifact);
        }

        if let Some(artifact) = self
            .artifacts
            .read()
            .map_err(|_| TutorValidationError::CacheUnavailable)?
            .get(&key)
            .cloned()
        {
            return Ok(artifact);
        }

        let fallback = reviewed_fallback(lesson, context);
        validate_tutor_artifact(lesson, context, &fallback)?;
        self.insert(key, fallback.clone())?;
        Ok(fallback)
    }

    pub fn len(&self) -> Result<usize, TutorValidationError> {
        self.artifacts
            .read()
            .map(|artifacts| artifacts.len())
            .map_err(|_| TutorValidationError::CacheUnavailable)
    }

    pub fn is_empty(&self) -> Result<bool, TutorValidationError> {
        self.artifacts
            .read()
            .map(|artifacts| artifacts.is_empty())
            .map_err(|_| TutorValidationError::CacheUnavailable)
    }

    fn insert(&self, key: String, artifact: TutorArtifact) -> Result<(), TutorValidationError> {
        let mut artifacts = self
            .artifacts
            .write()
            .map_err(|_| TutorValidationError::CacheUnavailable)?;
        while artifacts.len() >= self.max_entries && !artifacts.contains_key(&key) {
            let Some(oldest) = artifacts.keys().next().cloned() else {
                break;
            };
            artifacts.remove(&oldest);
        }
        artifacts.insert(key, artifact);
        Ok(())
    }
}

fn validate_tutor_context(context: &TutorContext) -> Result<&'static Lesson, TutorValidationError> {
    let contract = tutor_contract();
    if context.scenario_manifest_version != SCENARIO_MANIFEST_VERSION
        || !contract.allowed_modes.contains(&context.mode)
        || context.prompt.trim().is_empty()
        || context.prompt.chars().count() > contract.max_prompt_chars
        || context.test_output.chars().count() > contract.max_test_output_chars
    {
        return Err(TutorValidationError::InvalidContext);
    }

    curriculum_manifest()
        .lessons
        .iter()
        .find(|lesson| lesson.lesson_id == context.lesson_id)
        .ok_or(TutorValidationError::InvalidContext)
}

fn validate_tutor_artifact(
    lesson: &Lesson,
    context: &TutorContext,
    artifact: &TutorArtifact,
) -> Result<(), TutorValidationError> {
    if artifact.lesson_id != context.lesson_id
        || artifact.scenario_manifest_version != context.scenario_manifest_version
        || artifact.mode != context.mode
        || artifact.answer.trim().is_empty()
        || artifact.next_question.trim().is_empty()
    {
        return Err(TutorValidationError::InvalidContext);
    }
    if artifact.answer.chars().count() > tutor_contract().max_answer_chars {
        return Err(TutorValidationError::ResponseTooLong);
    }
    if artifact.source_references.is_empty()
        || artifact
            .source_references
            .iter()
            .any(|source| !lesson.source_references.contains(source))
    {
        return Err(TutorValidationError::UnsupportedCitation);
    }

    let answer = artifact.answer.to_ascii_lowercase();
    if tutor_contract()
        .forbidden_generic_phrases
        .iter()
        .any(|phrase| answer.contains(&phrase.to_ascii_lowercase()))
        || lesson
            .tutor_anchors
            .iter()
            .filter(|anchor| answer.contains(&anchor.to_ascii_lowercase()))
            .count()
            < 2
    {
        return Err(TutorValidationError::GenericResponse);
    }
    Ok(())
}

fn reviewed_fallback(lesson: &Lesson, context: &TutorContext) -> TutorArtifact {
    let anchors = lesson.tutor_anchors.join(", ");
    TutorArtifact {
        lesson_id: lesson.lesson_id.clone(),
        scenario_manifest_version: context.scenario_manifest_version.clone(),
        mode: context.mode,
        answer: format!(
            "{} {} Review {} in `{}`. Key terms: {}.",
            lesson.learner_outcome,
            lesson
                .explainer
                .first()
                .map(String::as_str)
                .unwrap_or_default(),
            lesson.code_lens.symbol,
            lesson.code_lens.path,
            anchors
        ),
        source_references: lesson.source_references.clone(),
        next_question: lesson.checkpoint.prompt.clone(),
        origin: TutorArtifactOrigin::ReviewedFallback,
    }
}

fn tutor_cache_key(context: &TutorContext) -> String {
    let mut digest = Sha256::new();
    digest.update(context.lesson_id.as_bytes());
    digest.update([0]);
    digest.update(context.scenario_manifest_version.as_bytes());
    digest.update([0]);
    digest.update(format!("{:?}", context.mode).as_bytes());
    digest.update([0]);
    digest.update(context.prompt.as_bytes());
    digest.update([0]);
    digest.update(context.test_output.as_bytes());
    format!("{:x}", digest.finalize())
}

fn ensure(condition: bool, message: &str) -> Result<(), CurriculumValidationError> {
    condition.then_some(()).ok_or_else(|| invalid(message))
}

fn invalid(message: &str) -> CurriculumValidationError {
    CurriculumValidationError(message.to_string())
}

fn unique_map<'a, T>(
    entries: impl Iterator<Item = (&'a str, &'a T)>,
    duplicate_message: &str,
) -> Result<BTreeMap<&'a str, &'a T>, CurriculumValidationError> {
    let mut values = BTreeMap::new();
    for (key, value) in entries {
        if values.insert(key, value).is_some() {
            return Err(invalid(duplicate_message));
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zcash::{
        PaymentLifecycleFixture, PaymentLifecycleState, PaymentRequestPolicy, ReceiverPolicy,
        ZcashNetwork, evaluate_lifecycle, inspect_address, inspect_viewing_key,
        validate_payment_request,
    };
    use serde_json::Value;
    use zcash_address::unified::{Encoding, Ivk, Uivk};
    use zcash_protocol::consensus::NetworkType;

    #[test]
    fn reviewed_artifact_graph_is_complete_and_public_projection_is_safe() {
        validate_reviewed_artifacts().expect("reviewed artifact graph");
        let public = public_curriculum().expect("public curriculum");
        assert_eq!(public.lessons.len(), 5);
        assert_eq!(public.curriculum_version, CURRICULUM_VERSION);
        assert_eq!(public.scenario_manifest_version, SCENARIO_MANIFEST_VERSION);
        assert_eq!(public.tutor_contract_version, TUTOR_CONTRACT_VERSION);
        assert!(public.lessons.iter().all(|lesson| {
            lesson.valid_case_count >= 1
                && lesson.denial_case_count >= 2
                && !lesson.source_references.is_empty()
        }));

        let encoded = serde_json::to_string(&public).expect("public curriculum JSON");
        for private_key in [
            "correct_option_id",
            "rationale",
            "denial_case_ids",
            "seeded_defects",
            "required_repairs",
        ] {
            assert!(!encoded.contains(&format!("\"{private_key}\":")));
        }
        for hidden_case_id in ["ua-non-unified-shielded", "privacy-google-wallet-linkage"] {
            assert!(!encoded.contains(hidden_case_id), "{hidden_case_id}");
        }
    }

    #[test]
    fn defect_markers_and_test_specs_match_the_scenario_manifest() {
        let scenarios = scenario_manifest();
        let starter =
            include_str!("../scenarios/zcash/shielded-checkout/v1/starter/src/checkout.ts");
        let solution =
            include_str!("../scenarios/zcash/shielded-checkout/v1/solution/src/checkout.ts");
        let public_tests =
            include_str!("../scenarios/zcash/shielded-checkout/v1/tests/public.spec.ts");
        let hidden_spec: Value = serde_json::from_str(include_str!(
            "../scenarios/zcash/shielded-checkout/v1/specs/hidden-tests.json"
        ))
        .expect("hidden test specification");

        assert_eq!(starter.matches("VQ_DEFECT:").count(), 5);
        assert!(!solution.contains("VQ_DEFECT:"));
        for defect in &scenarios.seeded_defects {
            assert!(starter.contains(&format!("VQ_DEFECT:{}", defect.defect_id)));
            assert!(starter.contains(&format!("function {}", defect.location.symbol)));
            assert!(solution.contains(&format!("function {}", defect.location.symbol)));
        }

        let hidden_ids = hidden_spec["cases"]
            .as_array()
            .expect("hidden cases")
            .iter()
            .map(|case| case["case_id"].as_str().expect("hidden case ID"))
            .collect::<BTreeSet<_>>();
        let manifest_hidden_ids = scenarios
            .cases
            .iter()
            .filter(|case| case.visibility == CaseVisibility::Hidden)
            .map(|case| case.case_id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(hidden_ids, manifest_hidden_ids);

        for case in scenarios
            .cases
            .iter()
            .filter(|case| case.visibility == CaseVisibility::Public)
        {
            assert!(public_tests.contains(&case.case_id), "{}", case.case_id);
        }
    }

    #[test]
    fn every_declared_scenario_case_executes_with_its_reviewed_outcome() {
        for case in &scenario_manifest().cases {
            let accepted = execute_reviewed_case(&case.case_id);
            assert_eq!(
                accepted,
                case.expected_outcome == ExpectedOutcome::Accepted,
                "{}",
                case.case_id
            );
        }
    }

    #[test]
    fn tutor_contract_rejects_unsupported_and_generic_artifacts_and_works_offline() {
        let context = TutorContext {
            lesson_id: "unified-address-policy".to_string(),
            scenario_manifest_version: SCENARIO_MANIFEST_VERSION.to_string(),
            mode: TutorMode::Hint,
            prompt: "Why did the wrong-network case fail?".to_string(),
            test_output: "ua-wrong-network: denied".to_string(),
        };
        let cache = TutorArtifactCache::new(8);
        let fallback = cache
            .resolve(&context, None)
            .expect("reviewed offline fallback");
        assert_eq!(fallback.origin, TutorArtifactOrigin::ReviewedFallback);
        assert_eq!(cache.len().expect("cache length"), 1);
        assert_eq!(
            cache.resolve(&context, None).expect("cached fallback"),
            fallback
        );

        let accepted = TutorArtifact {
            lesson_id: context.lesson_id.clone(),
            scenario_manifest_version: context.scenario_manifest_version.clone(),
            mode: context.mode,
            answer: "Decode the Unified Address, verify the network, and only then select a supported shielded receiver. The failed test shows that string validity does not satisfy receiver policy.".to_string(),
            source_references: vec!["zip-0316".to_string()],
            next_question: "Which field should be checked before receiver selection?".to_string(),
            origin: TutorArtifactOrigin::Provider,
        };
        assert_eq!(
            cache
                .resolve(&context, Some(accepted.clone()))
                .expect("grounded provider artifact"),
            accepted
        );

        let mut unsupported = accepted.clone();
        unsupported.source_references = vec!["unreviewed-blog".to_string()];
        assert_eq!(
            cache.resolve(&context, Some(unsupported)),
            Err(TutorValidationError::UnsupportedCitation)
        );

        let mut generic = accepted;
        generic.answer = "Here is a general overview. Cryptocurrency can be complex.".to_string();
        assert_eq!(
            cache.resolve(&context, Some(generic)),
            Err(TutorValidationError::GenericResponse)
        );
    }

    fn execute_reviewed_case(case_id: &str) -> bool {
        match case_id {
            "ua-valid-shielded" => inspect_address(
                &protocol_case("/mainnet_unified_address"),
                ZcashNetwork::Mainnet,
                ReceiverPolicy::shielded_checkout(),
            )
            .is_ok(),
            "ua-wrong-network" => inspect_address(
                &protocol_case("/mainnet_unified_address"),
                ZcashNetwork::Testnet,
                ReceiverPolicy::shielded_checkout(),
            )
            .is_ok(),
            "ua-transparent-only" => inspect_address(
                &protocol_case("/testnet_transparent_address"),
                ZcashNetwork::Testnet,
                ReceiverPolicy::shielded_checkout(),
            )
            .is_ok(),
            "ua-non-unified-shielded" => inspect_address(
                &protocol_case("/testnet_sapling_address"),
                ZcashNetwork::Testnet,
                ReceiverPolicy::shielded_checkout(),
            )
            .is_ok(),
            "zip321-valid-exact" => validate_payment_request(
                &protocol_case("/zip321/valid_single"),
                PaymentRequestPolicy::shielded_checkout_testnet(),
            )
            .is_ok(),
            "zip321-unknown-required" => validate_payment_request(
                &protocol_case("/zip321/unknown_required"),
                PaymentRequestPolicy::shielded_checkout_testnet(),
            )
            .is_ok(),
            "zip321-lab-amount-limit" => validate_payment_request(
                &protocol_case("/zip321/amount_at_protocol_max"),
                PaymentRequestPolicy::shielded_checkout_testnet(),
            )
            .is_ok(),
            "zip321-transparent-memo" => validate_payment_request(
                &protocol_case("/zip321/transparent_memo"),
                PaymentRequestPolicy::protocol_compatible(ZcashNetwork::Testnet),
            )
            .is_ok(),
            "viewing-valid-incoming" => {
                inspect_viewing_key(&synthetic_uivk(), ZcashNetwork::Testnet).is_ok()
            }
            "viewing-spending-material" => {
                inspect_viewing_key("secret-extended-key-main1forbidden", ZcashNetwork::Mainnet)
                    .is_ok()
            }
            "viewing-wrong-network" => {
                inspect_viewing_key(&synthetic_uivk(), ZcashNetwork::Mainnet).is_ok()
            }
            "viewing-malformed" => {
                inspect_viewing_key("uview-malformed", ZcashNetwork::Testnet).is_ok()
            }
            "lifecycle-confirmed" => lifecycle_case("confirmed-threshold"),
            "lifecycle-reorg" => lifecycle_case("reorg-after-confirmation"),
            "lifecycle-duplicate" => lifecycle_case("duplicate-observation"),
            "lifecycle-mismatch" => lifecycle_case("recipient-mismatch"),
            "privacy-safe-event" => {
                privacy_case_safe(&["request_id", "scenario_id", "state", "rule_ids"])
            }
            "privacy-raw-address-log" => privacy_case_safe(&[
                "request_id",
                "scenario_id",
                "state",
                "rule_ids",
                "raw_address",
            ]),
            "privacy-memo-log" => privacy_case_safe(&[
                "request_id",
                "scenario_id",
                "state",
                "rule_ids",
                "memo",
                "payment_request",
            ]),
            "privacy-google-wallet-linkage" => privacy_case_safe(&[
                "request_id",
                "scenario_id",
                "state",
                "rule_ids",
                "google_email",
                "raw_address",
            ]),
            unknown => panic!("scenario manifest contains an unimplemented case: {unknown}"),
        }
    }

    fn protocol_case(pointer: &str) -> String {
        let fixtures: Value =
            serde_json::from_str(include_str!("../fixtures/zcash/v1/protocol-cases.json"))
                .expect("protocol fixture JSON");
        fixtures
            .pointer(pointer)
            .and_then(Value::as_str)
            .expect("protocol fixture value")
            .to_string()
    }

    fn synthetic_uivk() -> String {
        Uivk::try_from_items(vec![Ivk::Orchard([3; 64])])
            .expect("synthetic UIVK")
            .encode(&NetworkType::Test)
    }

    fn lifecycle_case(case_id: &str) -> bool {
        let fixtures: Vec<PaymentLifecycleFixture> =
            serde_json::from_str(include_str!("../fixtures/zcash/v1/payment-lifecycle.json"))
                .expect("lifecycle fixtures");
        let fixture = fixtures
            .iter()
            .find(|fixture| fixture.case_id == case_id)
            .expect("lifecycle case");
        evaluate_lifecycle(fixture).expect("valid lifecycle fixture")
            == PaymentLifecycleState::Confirmed
    }

    fn privacy_case_safe(fields: &[&str]) -> bool {
        let allowed = BTreeSet::from(["request_id", "scenario_id", "state", "rule_ids"]);
        fields.iter().all(|field| allowed.contains(field))
    }
}
