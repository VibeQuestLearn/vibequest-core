#![recursion_limit = "256"]
#![allow(
    dead_code,
    reason = "legacy CKB implementation is intentionally unrouted during the v3 migration"
)]

pub mod auth;
pub mod curriculum;

pub mod platform;
pub mod runner;
pub mod zcash;

use auth::{AuthVerifier, AuthenticatedPrincipal};
use axum::{
    Json, Router,
    extract::{Extension, Path, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use base64::Engine;
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use mongodb::{
    Client as MongoClient, Collection, Database,
    bson::{DateTime as BsonDateTime, Document, doc},
    options::ClientOptions,
};
use p256::ecdsa::{Signature as P256Signature, VerifyingKey, signature::Verifier as _};
use platform::{
    AccountDeletion, AccountExport, CatalogResponse, EcosystemRegistry, PlatformStore,
    RegistryError, TrackRegistration,
};
use reqwest::{Client, StatusCode as ReqwestStatusCode, Url};
use ring::signature::{self, RsaPublicKeyComponents};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::sync::OnceCell;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing::warn;
use uuid::Uuid;

const DEFAULT_OPENAI_MODEL: &str = "gpt-5.5";
const DEFAULT_OPENAI_BASE_URL: &str = "https://share-ai.ckbdev.com";
const DEFAULT_OPENAI_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::Minimal;
const DEFAULT_OPENAI_TIMEOUT_SECONDS: u64 = 90;
const MAX_OPENAI_TIMEOUT_SECONDS: u64 = 240;
const QUICK_QUEST_OUTPUT_TOKENS: u16 = 5200;
const LEARNING_LESSON_OUTPUT_TOKENS: u16 = 4200;
const TUTOR_OUTPUT_TOKENS: u16 = 780;

#[derive(Clone)]
pub struct AppState {
    auth: AuthVerifier,
    runner: runner::RunnerService,
    config: AppConfig,
    openai: OpenAiClient,
    registry: EcosystemRegistry,
    platform_store: PlatformStore,
    fiber: FiberPayoutClient,
    store: MongoStore,
}

#[derive(Clone)]
struct AppConfig {
    port: u16,
    app_env: String,
    cors_origins: Vec<String>,
    ckb_rpc_url: Option<String>,
    fiber_rpc_url: Option<String>,
    fiber_payout_rpc_url: Option<String>,
    fiber_payout_enabled: bool,
    reward_amount_shannons: u128,
    reward_currency: String,
    mongodb_uri: Option<String>,
    mongodb_database: String,
    mongodb_v3_database: String,
}

#[derive(Clone)]
struct OpenAiClient {
    http: Client,
    api_key: Option<String>,
    model: String,
    base_url: String,
    reasoning_effort: ReasoningEffort,
    disable_response_storage: bool,
    timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct AiProviderMetadata {
    provider_kind: String,
    model: String,
    endpoint_origin: String,
    reasoning_effort: ReasoningEffort,
    response_storage_disabled: bool,
    timeout_seconds: u64,
    configured: bool,
}

#[derive(Clone)]
struct FiberPayoutClient {
    http: Client,
    rpc_url: Option<String>,
    enabled: bool,
    timeout: Duration,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    service: &'static str,
    status: &'static str,
    environment: String,
    ai_layer: AiLayer,
    integrations: IntegrationStatus,
    missing: Vec<&'static str>,
    diagnostics: HealthDiagnostics,
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct HealthDiagnostics {
    mongodb: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReadyResponse {
    ready: bool,
    missing: Vec<&'static str>,
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum AiLayer {
    OpenAi,
}

#[derive(Debug, Serialize)]
struct IntegrationStatus {
    openai: bool,
    ckb_rpc: bool,
    fiber_rpc: bool,
    fiber_payout: bool,
    mongodb: bool,
}

#[derive(Debug, Serialize)]
struct SeasonResponse {
    season: String,
    thesis: String,
    tracks: Vec<Track>,
    gates: Vec<Gate>,
}

#[derive(Debug, Serialize)]
struct Track {
    name: String,
    description: String,
    sample_quests: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Gate {
    name: String,
    unlocks: String,
}

#[derive(Debug, Deserialize)]
struct GenerateQuestRequest {
    build_prompt: String,
    skill_track: Option<String>,
    difficulty: Option<Difficulty>,
    wallet: WalletProof,
    learning_context: Option<LearningQuestLink>,
}

#[derive(Debug, Deserialize)]
struct BindWalletRequest {
    wallet: WalletProof,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningQuestLink {
    module_id: String,
    lesson_id: String,
    module_title: String,
    lesson_title: String,
    checkpoint_question: String,
    #[serde(default)]
    quest_bridge: String,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    concepts: Vec<String>,
    #[serde(default)]
    correct_answer: String,
    #[serde(default)]
    misunderstanding: String,
    #[serde(default)]
    lesson_summary: String,
}

#[derive(Clone, Debug, Deserialize)]
struct GenerateLearningModuleRequest {
    #[serde(default)]
    path_id: Option<String>,
    #[serde(default)]
    ecosystem_id: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    learning_profile: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    learning_intents: Vec<String>,
    interests: Vec<String>,
    learner_goal: String,
    background: String,
    pace: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct PriorLearningLesson {
    #[serde(default)]
    title: String,
    #[serde(default)]
    checkpoint_question: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    code_lens: String,
}

#[derive(Debug, Deserialize)]
struct GenerateLearningLessonRequest {
    #[serde(default)]
    path_id: Option<String>,
    #[serde(default)]
    ecosystem_id: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    learning_profile: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    learning_intents: Vec<String>,
    interests: Vec<String>,
    learner_goal: String,
    background: String,
    pace: String,
    lesson_index: usize,
    #[serde(default)]
    prior_lessons: Vec<PriorLearningLesson>,
}

impl GenerateLearningLessonRequest {
    fn module_request(&self) -> GenerateLearningModuleRequest {
        GenerateLearningModuleRequest {
            path_id: self.path_id.clone(),
            ecosystem_id: self.ecosystem_id.clone(),
            topic: self.topic.clone(),
            learning_profile: self.learning_profile.clone(),
            learning_intents: self.learning_intents.clone(),
            interests: self.interests.clone(),
            learner_goal: self.learner_goal.clone(),
            background: self.background.clone(),
            pace: self.pace.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct GenerateLearningModuleResponse {
    module_id: String,
    source: QuestSource,
    provider: AiProviderMetadata,
    module: LearningModule,
    eval_artifact: LearningEvalArtifact,
    warning: Option<String>,
    persistence: PersistenceStatus,
}

#[derive(Debug, Serialize)]
struct GenerateLearningLessonResponse {
    source: QuestSource,
    provider: AiProviderMetadata,
    module_title: String,
    learner_profile: String,
    outcome: String,
    capstone_quest_prompt: String,
    resources: Vec<LearningResource>,
    lesson: LearningLesson,
    lesson_index: usize,
    module_status: LearningModuleGenerationState,
    eval_artifact: LearningEvalArtifact,
    warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningModule {
    title: String,
    learner_profile: String,
    outcome: String,
    lessons: Vec<LearningLesson>,
    capstone_quest_prompt: String,
    resources: Vec<LearningResource>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningLesson {
    id: String,
    title: String,
    why_it_matters: String,
    explanation: String,
    concepts: Vec<String>,
    #[serde(default)]
    submodules: Vec<LearningSubmodule>,
    #[serde(default)]
    resources: Vec<LearningResource>,
    #[serde(default)]
    evidence_map: Vec<LearningEvidence>,
    #[serde(default)]
    quality_score: LearningQualityScore,
    checkpoint: LearningCheckpoint,
    quest_bridge: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningSubmodule {
    id: String,
    title: String,
    summary: String,
    #[serde(default)]
    children: Vec<LearningSubmodule>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningCheckpoint {
    question: String,
    options: Vec<LearningOption>,
    correct_index: usize,
    explanation: String,
    follow_up_question: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningOption {
    label: String,
    feedback: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningResource {
    title: String,
    url: String,
    #[serde(alias = "description")]
    reason: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct LearningEvidence {
    claim: String,
    source_title: String,
    source_url: String,
    lesson_section: String,
    confidence: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct LearningQualityScore {
    source_coverage: u8,
    technical_depth: u8,
    checkpoint_quality: u8,
    placeholder_free: bool,
    ecosystem_alignment: bool,
    passed: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
struct LearningModuleValidationState {
    source_grounding: bool,
    technical_depth: bool,
    placeholder_free: bool,
    repetition_check: bool,
    checkpoint_quality: bool,
    ecosystem_alignment: bool,
    passed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningModuleGenerationState {
    lesson_index: usize,
    #[serde(default)]
    lesson_id: Option<String>,
    status: String,
    #[serde(default)]
    validation: LearningModuleValidationState,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningEvalArtifact {
    artifact_version: String,
    ecosystem_id: String,
    topic: Option<String>,
    learning_profile: Option<String>,
    learning_intents: Vec<String>,
    request_hash: String,
    provider: AiProviderMetadata,
    module_title: String,
    lesson_count: usize,
    validation: LearningModuleValidationState,
    lesson_reports: Vec<LearningLessonEvalReport>,
    warnings: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    integration_tags: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    source_ids: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    source_categories: Vec<String>,
    #[serde(default)]
    code_mode_enabled: bool,
    #[serde(default)]
    final_lab_ready: bool,
    #[serde(default)]
    denial_tests_count: usize,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    unsupported_claim_warnings: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    compute_model_coverage: Vec<String>,
    #[serde(default)]
    execution_path: Option<String>,
    #[serde(default)]
    task_lifecycle_covered: bool,
    #[serde(default)]
    failure_cases_count: usize,
    #[serde(default)]
    final_compute_lab_ready: bool,
    generated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningLessonEvalReport {
    lesson_id: String,
    title: String,
    validation: LearningModuleValidationState,
    quality_score: LearningQualityScore,
    source_titles: Vec<String>,
    source_urls: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    source_ids: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    source_categories: Vec<String>,
    warning_count: usize,
}

#[derive(Clone, Debug, Deserialize)]
struct AiLearningModuleCompact {
    t: String,
    l: Vec<AiLearningLessonCompact>,
}

#[derive(Clone, Debug, Deserialize)]
struct AiLearningLessonCompact {
    t: String,
    e: String,
    s: String,
    w: String,
    j: String,
    f: String,
    q: String,
    a: String,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    b: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    bf: Vec<String>,
    #[serde(default)]
    ci: usize,
}

#[derive(Debug, Deserialize)]
struct LearningTutorRequest {
    module_title: String,
    lesson_title: String,
    lesson_context: String,
    question: String,
}

#[derive(Debug, Serialize)]
struct LearningTutorResponse {
    source: QuestSource,
    answer: String,
    why_it_matters: String,
    follow_up_question: String,
    references: Vec<LearningResource>,
}

#[derive(Debug, Deserialize)]
struct LearningTutorAiResponse {
    answer: String,
    why_it_matters: String,
    follow_up_question: String,
    references: Vec<LearningResource>,
}

#[derive(Debug, Deserialize)]
struct CodeTutorRequest {
    quest_title: String,
    quest_objective: String,
    question: String,
    files: Vec<WorkbenchFile>,
    challenge: Option<QuestChallengeBrief>,
    run_id: Option<String>,
    wallet: Option<WalletProof>,
}

#[derive(Debug, Serialize)]
struct CodeTutorResponse {
    source: QuestSource,
    answer: String,
    code_walkthrough: Vec<String>,
    common_misunderstanding: String,
    follow_up_question: String,
    references: Vec<LearningResource>,
    persistence: PersistenceStatus,
}

#[derive(Debug, Deserialize)]
struct CodeTutorAiResponse {
    answer: String,
    code_walkthrough: Vec<String>,
    common_misunderstanding: String,
    follow_up_question: String,
    references: Vec<LearningResource>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningTutorMessage {
    id: String,
    role: String,
    text: String,
    why: Option<String>,
    follow_up: Option<String>,
    #[serde(default)]
    module_id: Option<String>,
    #[serde(default)]
    module_title: Option<String>,
    #[serde(default)]
    lesson_id: Option<String>,
    #[serde(default)]
    lesson_title: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningSessionDocument {
    #[serde(rename = "_id")]
    id: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    provider_subject: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    user_address: String,
    #[serde(default)]
    wallet: Option<WalletBinding>,
    source: QuestSource,
    module: LearningModule,
    #[serde(default)]
    module_statuses: Vec<LearningModuleGenerationState>,
    #[serde(default)]
    eval_artifacts: Vec<LearningEvalArtifact>,
    #[serde(default)]
    ecosystem_id: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    learning_profile: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    learning_intents: Vec<String>,
    selected_interests: Vec<String>,
    learner_goal: String,
    background: String,
    pace: String,
    active_lesson_index: i64,
    checkpoint_answers: Document,
    tutor_messages: Vec<LearningTutorMessage>,
    #[serde(default = "active_learning_session_status")]
    status: String,
    created_at: BsonDateTime,
    updated_at: BsonDateTime,
}

#[derive(Debug, Deserialize)]
struct SaveLearningSessionRequest {
    module_id: Option<String>,
    source: Option<QuestSource>,
    module: LearningModule,
    #[serde(default)]
    module_statuses: Vec<LearningModuleGenerationState>,
    #[serde(default)]
    eval_artifacts: Vec<LearningEvalArtifact>,
    #[serde(default)]
    ecosystem_id: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    learning_profile: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    learning_intents: Vec<String>,
    selected_interests: Vec<String>,
    learner_goal: String,
    background: String,
    pace: String,
    active_lesson_index: usize,
    checkpoint_answers: std::collections::BTreeMap<String, i64>,
    tutor_messages: Vec<LearningTutorMessage>,
}

#[derive(Debug, Serialize)]
struct LearningSessionResponse {
    session: Option<LearningSessionRecord>,
    persistence: PersistenceStatus,
}

#[derive(Debug, Serialize)]
struct LearningSessionsResponse {
    sessions: Vec<LearningSessionRecord>,
    persistence: PersistenceStatus,
}

#[derive(Debug, Serialize)]
struct SaveLearningSessionResponse {
    session: Option<LearningSessionRecord>,
    persistence: PersistenceStatus,
}

#[derive(Clone, Debug, Serialize)]
struct LearningSessionMutationResponse {
    module_id: String,
    archived: bool,
    deleted: bool,
    persistence: PersistenceStatus,
}

#[derive(Clone, Debug, Serialize)]
struct LearningSessionRecord {
    module_id: String,
    user_id: String,
    status: String,
    provider: String,
    email: Option<String>,
    name: Option<String>,
    source: QuestSource,
    module: LearningModule,
    module_statuses: Vec<LearningModuleGenerationState>,
    eval_artifacts: Vec<LearningEvalArtifact>,
    ecosystem_id: Option<String>,
    topic: Option<String>,
    learning_profile: Option<String>,
    learning_intents: Vec<String>,
    selected_interests: Vec<String>,
    learner_goal: String,
    background: String,
    pace: String,
    active_lesson_index: usize,
    checkpoint_answers: std::collections::BTreeMap<String, i64>,
    tutor_messages: Vec<LearningTutorMessage>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct SaveTutorExchangeRequest {
    #[serde(default)]
    module_id: Option<String>,
    module_title: String,
    lesson_title: String,
    lesson_context: String,
    question: String,
}

#[derive(Debug, Serialize)]
struct SavedTutorExchangeResponse {
    answer: LearningTutorResponse,
    session: Option<LearningSessionRecord>,
    persistence: PersistenceStatus,
}

#[derive(Debug, Deserialize)]
struct GenerateLearningQuestRequest {
    module_id: String,
    #[serde(default)]
    ecosystem_id: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    module_title: String,
    learner_profile: String,
    outcome: String,
    lesson: LearningLesson,
}

#[derive(Debug, Serialize)]
struct GenerateLearningQuestResponse {
    run_id: String,
    source: QuestSource,
    learning_context: LearningQuestLink,
    quest: QuestBlueprint,
    runner: LearningQuestRunnerState,
    persistence: PersistenceStatus,
    warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct LearningEventRequest {
    event_type: String,
    #[serde(default)]
    module_id: Option<String>,
    #[serde(default)]
    lesson_id: Option<String>,
    #[serde(default)]
    ecosystem_id: Option<String>,
    #[serde(default)]
    course_title: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LearningEventDocument {
    #[serde(rename = "_id")]
    id: String,
    user_id: String,
    provider: String,
    event_type: String,
    #[serde(default)]
    module_id: Option<String>,
    #[serde(default)]
    lesson_id: Option<String>,
    #[serde(default)]
    ecosystem_id: Option<String>,
    #[serde(default)]
    course_title: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    created_at: BsonDateTime,
}

#[derive(Clone, Debug, Serialize)]
struct LearningEventRecord {
    event_id: String,
    event_type: String,
    module_id: Option<String>,
    lesson_id: Option<String>,
    ecosystem_id: Option<String>,
    course_title: Option<String>,
    metadata: BTreeMap<String, String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct LearningEventResponse {
    saved: bool,
    persistence: PersistenceStatus,
}

#[derive(Clone, Debug, Default, Serialize)]
struct LearningMetricsSummary {
    total_events: usize,
    courses_generated: usize,
    modules_opened: usize,
    checkpoints_attempted: usize,
    checkpoints_passed: usize,
    courses_completed: usize,
    tutor_used: usize,
    generation_failures: usize,
    by_event: BTreeMap<String, usize>,
    by_ecosystem: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
struct LearningMetricsResponse {
    summary: LearningMetricsSummary,
    recent_events: Vec<LearningEventRecord>,
    persistence: PersistenceStatus,
}

#[derive(Debug, Serialize)]
struct LearningSessionExportResponse {
    session: Option<LearningSessionRecord>,
    markdown: String,
    json: Value,
    persistence: PersistenceStatus,
}

#[derive(Debug, Serialize)]
struct LearningAdminReviewResponse {
    sessions: Vec<LearningSessionRecord>,
    metrics: LearningMetricsSummary,
    recent_events: Vec<LearningEventRecord>,
    persistence: PersistenceStatus,
}

#[derive(Debug, Serialize)]
struct LearningQuestRunnerState {
    enabled: bool,
    ecosystem_supported: bool,
    scenario_id: String,
    scenario_manifest_version: String,
    runner_version: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct WalletProof {
    address: String,
    message: String,
    signature: WalletSignature,
}

#[derive(Debug, Deserialize, Serialize)]
struct WalletSignature {
    signature: String,
    identity: String,
    sign_type: String,
    pubkey: Option<String>,
    key_type: Option<String>,
    challenge: Option<String>,
    alg: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Difficulty {
    Novice,
    Builder,
    Boss,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Debug, Serialize)]
struct GenerateQuestResponse {
    run_id: Uuid,
    source: QuestSource,
    learning_context: Option<LearningQuestLink>,
    wallet: WalletBinding,
    quest: QuestBlueprint,
    ship_requirements: ShipRequirements,
    persistence: PersistenceStatus,
    warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistenceStatus {
    saved: bool,
    warning: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum QuestSource {
    OpenAi,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WalletBinding {
    address: String,
    identity: String,
    sign_type: String,
    message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ShipRequirements {
    ckb_rpc_ready: bool,
    fiber_rpc_ready: bool,
    can_claim_rewards: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct QuestBlueprint {
    title: String,
    premise: String,
    #[serde(alias = "buildObjective")]
    build_objective: String,
    #[serde(
        alias = "comprehensionGates",
        deserialize_with = "deserialize_string_vec"
    )]
    comprehension_gates: Vec<String>,
    #[serde(alias = "bossFight")]
    boss_fight: String,
    #[serde(alias = "challengeBrief", default)]
    challenge_brief: Option<QuestChallengeBrief>,
    #[serde(alias = "codeExplainer")]
    code_explainer: QuestCodeExplainer,
    #[serde(alias = "rewardLogic")]
    reward_logic: String,
    #[serde(
        alias = "ckbFiberHooks",
        default,
        deserialize_with = "deserialize_string_vec"
    )]
    ckb_fiber_hooks: Vec<String>,
    #[serde(alias = "workbenchFiles")]
    workbench_files: Vec<WorkbenchFile>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct QuestChallengeBrief {
    question: String,
    #[serde(alias = "correctAnswer")]
    correct_answer: String,
    #[serde(alias = "wrongAnswers")]
    wrong_answers: Vec<ChallengeWrongAnswer>,
    invariant: String,
    #[serde(alias = "attackScenario")]
    attack_scenario: String,
    #[serde(alias = "codeFocus")]
    code_focus: String,
    #[serde(alias = "testFocus")]
    test_focus: String,
    hint: String,
    #[serde(alias = "followUpQuestion")]
    follow_up_question: String,
    resources: Vec<LearningResource>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ChallengeWrongAnswer {
    label: String,
    feedback: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct QuestCodeExplainer {
    #[serde(alias = "primaryInvariant")]
    primary_invariant: String,
    #[serde(alias = "denialPath")]
    denial_path: String,
    #[serde(alias = "proofLabel")]
    proof_label: String,
    #[serde(alias = "proofArtifact")]
    proof_artifact: String,
    #[serde(alias = "networkLabel")]
    network_label: String,
    #[serde(alias = "networkBoundary")]
    network_boundary: String,
    #[serde(alias = "riskFocus")]
    risk_focus: String,
    #[serde(
        alias = "inspectSteps",
        default,
        deserialize_with = "deserialize_string_vec"
    )]
    inspect_steps: Vec<String>,
    #[serde(
        alias = "mentorPrompts",
        default,
        deserialize_with = "deserialize_string_vec"
    )]
    mentor_prompts: Vec<String>,
    #[serde(default)]
    resources: Vec<LearningResource>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WorkbenchFile {
    path: String,
    #[serde(default)]
    language: String,
    #[serde(deserialize_with = "deserialize_workbench_file_content")]
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    output_text: Option<String>,
    output: Option<Vec<OpenAiOutputItem>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiOutputItem {
    content: Option<Vec<OpenAiContentItem>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiContentItem {
    #[serde(rename = "type")]
    content_type: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Error)]
enum ApiError {
    #[error("build_prompt must be at least 12 characters")]
    InvalidPrompt,
    #[error(
        "This looks like a learning request, not a coding quest. Open Learning Mode to generate a lesson path, tutor chat, checkpoints, and follow-up quests."
    )]
    LearningRequestNeedsModule,
    #[error("wallet address is required")]
    MissingWalletAddress,
    #[error("wallet signature is required")]
    MissingWalletSignature,
    #[error("wallet proof message must include VibeQuest")]
    InvalidWalletProofMessage,
    #[error("wallet proof must use JoyID")]
    UnsupportedWalletSignature,
    #[error("wallet signature could not be verified against the signer identity")]
    InvalidWalletSignature,
    #[error("OpenAI is not configured. Add OPENAI_API_KEY before generating live quests.")]
    MissingOpenAiKey,
    #[error("AI generation is temporarily unavailable. Please regenerate in a moment.")]
    OpenAiTransport(String),
    #[error("AI generation is temporarily unavailable. Please regenerate in a moment.")]
    OpenAiStatus {
        status: ReqwestStatusCode,
        body: String,
    },
    #[error("The AI response was incomplete. Please regenerate.")]
    InvalidAiResponse,
    #[error("Quest history is temporarily unavailable because MongoDB is not configured.")]
    DatabaseUnavailable,
    #[error("Quest history is temporarily unavailable. Please refresh in a moment.")]
    Database(String),
    #[error("quest run was not found")]
    QuestNotFound,
    #[error("wallet proof does not own this quest run")]
    WalletMismatch,
    #[error("quest completion evidence is not payout eligible")]
    CompletionNotVerified,
    #[error("Fiber invoice is required before locking a reward claim")]
    MissingFiberInvoice,
    #[error("Fiber invoice is not valid enough to lock a reward claim")]
    InvalidFiberInvoice,
    #[error("Fiber payout is not configured on vibequest-core")]
    FiberPayoutUnavailable,
    #[error("Fiber payout failed: {0}")]
    FiberPayout(String),
    #[error("reward claim is already paid or currently paying")]
    RewardAlreadyProcessed,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

impl From<mongodb::error::Error> for ApiError {
    fn from(error: mongodb::error::Error) -> Self {
        Self::Database(error.to_string())
    }
}

#[derive(Clone)]
struct MongoStore {
    uri: Option<String>,
    database_name: String,
    client: Arc<OnceCell<MongoClient>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct UserDocument {
    #[serde(rename = "_id")]
    id: String,
    address: String,
    wallet: WalletBinding,
    quest_counts: UserQuestCounts,
    created_at: BsonDateTime,
    updated_at: BsonDateTime,
    last_seen_at: BsonDateTime,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct UserQuestCounts {
    created: i64,
    completed: i64,
    uncompleted: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct QuestRunDocument {
    #[serde(rename = "_id")]
    run_id: String,
    user_address: String,
    build_prompt: String,
    skill_track: String,
    difficulty: String,
    learning_context: Option<LearningQuestLink>,
    source: QuestSource,
    wallet: WalletBinding,
    quest: QuestBlueprint,
    ship_requirements: ShipRequirements,
    progress: QuestProgress,
    #[serde(default)]
    boss_attempts: Vec<BossAttempt>,
    #[serde(default)]
    code_tutor_messages: Vec<CodeTutorMessage>,
    status: QuestRunStatus,
    created_at: BsonDateTime,
    updated_at: BsonDateTime,
    completed_at: Option<BsonDateTime>,
    #[serde(default = "default_reward_snapshot")]
    reward: RewardSnapshot,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RewardSnapshot {
    amount_shannons: String,
    currency: String,
    sponsor: String,
}

fn default_reward_snapshot() -> RewardSnapshot {
    RewardSnapshot {
        amount_shannons: "0".to_string(),
        currency: "Fibd".to_string(),
        sponsor: "vibequest-core".to_string(),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RewardClaimDocument {
    #[serde(rename = "_id")]
    claim_id: String,
    run_id: String,
    user_address: String,
    fiber_invoice: String,
    amount_shannons: String,
    currency: String,
    status: RewardClaimStatus,
    verification: ServerCompletionProof,
    fiber_payment: Option<FiberPaymentReceipt>,
    error: Option<String>,
    created_at: BsonDateTime,
    updated_at: BsonDateTime,
    paid_at: Option<BsonDateTime>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RewardClaimStatus {
    Pending,
    Verified,
    Paying,
    Paid,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ServerCompletionProof {
    identity_gate: bool,
    infrastructure_gate: bool,
    verification_gate: bool,
    boss_fight_solved: bool,
    generated_files_verified: bool,
    tests_present: bool,
    proof_present: bool,
    denial_path_present: bool,
    completed_at: BsonDateTime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct FiberPaymentReceipt {
    payment_hash: Option<String>,
    status: Option<String>,
    fee: Option<String>,
    raw: Value,
}

#[derive(Debug, Deserialize)]
struct CompleteQuestRequest {
    wallet: WalletProof,
    gates: Vec<StoredGateProgress>,
    boss_fight_solved: bool,
    fiber_invoice: String,
}

#[derive(Debug, Serialize)]
struct CompleteQuestResponse {
    run: QuestRunRecord,
    claim: RewardClaimRecord,
}

#[derive(Clone, Debug, Serialize)]
struct RewardClaimRecord {
    claim_id: String,
    run_id: String,
    user_address: String,
    amount_shannons: String,
    currency: String,
    status: RewardClaimStatus,
    fiber_payment: Option<FiberPaymentReceipt>,
    error: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    paid_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct FiberRpcResponse {
    result: Option<Value>,
    error: Option<FiberRpcError>,
}

#[derive(Debug, Deserialize)]
struct FiberRpcError {
    code: Option<i64>,
    message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct QuestProgress {
    gates: Vec<StoredGateProgress>,
    boss_fight_solved: bool,
    shipped: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredGateProgress {
    id: String,
    name: String,
    description: String,
    #[serde(alias = "isCompleted")]
    is_completed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum QuestRunStatus {
    InProgress,
    Completed,
}

#[derive(Debug, Serialize)]
struct UserQuestHistoryResponse {
    user: Option<UserProfileResponse>,
    stats: UserQuestCounts,
    active_run: Option<QuestRunRecord>,
    runs: Vec<QuestRunRecord>,
    reward_claims: Vec<RewardClaimRecord>,
    persistence: HistoryPersistenceStatus,
}

#[derive(Clone, Debug, Serialize)]
struct HistoryPersistenceStatus {
    available: bool,
    message: Option<String>,
}

#[derive(Debug, Serialize)]
struct UserProfileResponse {
    address: String,
    quest_counts: UserQuestCounts,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
struct QuestRunRecord {
    run_id: String,
    user_address: String,
    build_prompt: String,
    skill_track: String,
    difficulty: String,
    learning_context: Option<LearningQuestLink>,
    source: QuestSource,
    quest: QuestBlueprint,
    ship_requirements: ShipRequirements,
    progress: QuestProgress,
    boss_attempts: Vec<BossAttempt>,
    code_tutor_messages: Vec<CodeTutorMessage>,
    status: QuestRunStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    reward: RewardSnapshot,
}

#[derive(Debug, Deserialize)]
struct UpdateQuestProgressRequest {
    wallet: WalletProof,
    gates: Option<Vec<StoredGateProgress>>,
    boss_fight_solved: Option<bool>,
    boss_attempt: Option<BossAttemptRequest>,
    shipped: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BossAttempt {
    selected_index: i64,
    selected_label: String,
    correct: bool,
    feedback: String,
    follow_up_question: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct BossAttemptRequest {
    selected_index: i64,
    selected_label: String,
    correct: bool,
    feedback: String,
    follow_up_question: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CodeTutorMessage {
    id: String,
    role: String,
    text: String,
    code_walkthrough: Vec<String>,
    common_misunderstanding: Option<String>,
    follow_up_question: Option<String>,
    references: Vec<LearningResource>,
    created_at: DateTime<Utc>,
}

impl MongoStore {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            uri: config.mongodb_uri.clone(),
            database_name: config.mongodb_database.clone(),
            client: Arc::new(OnceCell::new()),
        }
    }

    #[cfg(test)]
    fn disabled() -> Self {
        Self {
            uri: None,
            database_name: "vibequest".to_string(),
            client: Arc::new(OnceCell::new()),
        }
    }

    fn is_configured(&self) -> bool {
        self.uri.is_some()
    }

    async fn is_available(&self) -> bool {
        self.availability_diagnostic().await.is_ok()
    }

    async fn availability_diagnostic(&self) -> Result<(), String> {
        if !self.is_configured() {
            return Err("MONGODB_URI is not configured".to_string());
        }

        tokio::time::timeout(Duration::from_secs(4), async {
            let database = self.database().await.map_err(|error| error.to_string())?;
            let mut command = Document::new();
            command.insert("ping", 1);

            database
                .run_command(command)
                .await
                .map(|_| ())
                .map_err(|error| sanitize_mongodb_error(&error.to_string()))
        })
        .await
        .unwrap_or_else(|_| Err("MongoDB ping timed out after 4 seconds".to_string()))
    }

    async fn database(&self) -> Result<Database, ApiError> {
        let uri = self.uri.clone().ok_or(ApiError::DatabaseUnavailable)?;
        let client = self
            .client
            .get_or_try_init(move || async move {
                let mut options = ClientOptions::parse(&uri)
                    .await
                    .map_err(|error| ApiError::Database(error.to_string()))?;
                options.server_selection_timeout = Some(Duration::from_secs(3));
                options.connect_timeout = Some(Duration::from_secs(3));

                MongoClient::with_options(options)
                    .map_err(|error| ApiError::Database(error.to_string()))
            })
            .await?;

        Ok(client.database(&self.database_name))
    }

    async fn users(&self) -> Result<Collection<UserDocument>, ApiError> {
        Ok(self.database().await?.collection("users"))
    }

    async fn quest_runs(&self) -> Result<Collection<QuestRunDocument>, ApiError> {
        Ok(self.database().await?.collection("quest_runs"))
    }

    async fn reward_claims(&self) -> Result<Collection<RewardClaimDocument>, ApiError> {
        Ok(self.database().await?.collection("reward_claims"))
    }

    async fn learning_sessions(&self) -> Result<Collection<LearningSessionDocument>, ApiError> {
        Ok(self.database().await?.collection("learning_sessions"))
    }

    async fn learning_events(&self) -> Result<Collection<LearningEventDocument>, ApiError> {
        Ok(self.database().await?.collection("learning_events"))
    }

    async fn record_generated_quest(
        &self,
        request: &GenerateQuestRequest,
        response: &GenerateQuestResponse,
        reward_amount_shannons: u128,
        reward_currency: &str,
    ) -> Result<(), ApiError> {
        if !self.is_configured() {
            return Ok(());
        }

        self.upsert_user(&response.wallet).await?;

        let now = BsonDateTime::now();
        let run = QuestRunDocument {
            run_id: response.run_id.to_string(),
            user_address: response.wallet.address.clone(),
            build_prompt: request.build_prompt.trim().to_string(),
            skill_track: request
                .skill_track
                .as_deref()
                .unwrap_or("CKB + Fiber Builder")
                .trim()
                .to_string(),
            difficulty: difficulty_label(request.difficulty.as_ref()).to_string(),
            learning_context: request
                .learning_context
                .clone()
                .map(compact_learning_quest_link),
            source: response.source,
            wallet: response.wallet.clone(),
            quest: response.quest.clone(),
            ship_requirements: response.ship_requirements.clone(),
            progress: initial_quest_progress(
                response.ship_requirements.ckb_rpc_ready
                    && response.ship_requirements.fiber_rpc_ready,
            ),
            boss_attempts: Vec::new(),
            code_tutor_messages: Vec::new(),
            status: QuestRunStatus::InProgress,
            created_at: now,
            updated_at: now,
            completed_at: None,
            reward: RewardSnapshot {
                amount_shannons: reward_amount_shannons.to_string(),
                currency: reward_currency.to_string(),
                sponsor: "vibequest-core".to_string(),
            },
        };

        self.quest_runs().await?.insert_one(&run).await?;
        self.refresh_user_counts(&response.wallet.address).await?;
        Ok(())
    }

    async fn upsert_user(&self, wallet: &WalletBinding) -> Result<(), ApiError> {
        let users = self.users().await?;
        let address = wallet.address.trim().to_string();
        let now = BsonDateTime::now();

        if users.find_one(doc! { "_id": &address }).await?.is_some() {
            users
                .update_one(
                    doc! { "_id": &address },
                    doc! {
                        "$set": {
                            "address": &address,
                            "wallet": wallet_document(wallet),
                            "updated_at": now,
                            "last_seen_at": now,
                        }
                    },
                )
                .await?;
            return Ok(());
        }

        users
            .insert_one(UserDocument {
                id: address.clone(),
                address,
                wallet: wallet.clone(),
                quest_counts: UserQuestCounts::default(),
                created_at: now,
                updated_at: now,
                last_seen_at: now,
            })
            .await?;
        Ok(())
    }

    async fn bind_wallet_user(&self, wallet: WalletProof) -> Result<UserProfileResponse, ApiError> {
        validate_wallet_proof(&wallet)?;
        let binding = wallet_binding_from_proof(&wallet);
        let address = binding.address.clone();
        self.upsert_user(&binding).await?;

        let user = self
            .users()
            .await?
            .find_one(doc! { "_id": &address })
            .await?
            .ok_or_else(|| {
                ApiError::Database(
                    "MongoDB user profile was not found after wallet bind".to_string(),
                )
            })?;

        Ok(user.into())
    }

    async fn user_history(&self, address: &str) -> Result<UserQuestHistoryResponse, ApiError> {
        let address = address.trim();
        if address.is_empty() {
            return Err(ApiError::MissingWalletAddress);
        }

        let users = self.users().await?;
        let runs = self.quest_runs().await?;
        let user = users.find_one(doc! { "_id": address }).await?;
        let stats = self.counts_for_user(address).await?;
        let claims_cursor = self
            .reward_claims()
            .await?
            .find(doc! { "user_address": address })
            .sort(doc! { "updated_at": -1 })
            .limit(40)
            .await?;
        let reward_claims = claims_cursor
            .try_collect::<Vec<_>>()
            .await?
            .into_iter()
            .map(RewardClaimRecord::from)
            .collect::<Vec<_>>();
        let cursor = runs
            .find(doc! { "user_address": address })
            .sort(doc! { "updated_at": -1 })
            .limit(40)
            .await?;
        let documents = cursor.try_collect::<Vec<_>>().await?;
        let records = documents
            .into_iter()
            .map(QuestRunRecord::from)
            .collect::<Vec<_>>();
        let active_run = records
            .iter()
            .find(|run| run.status != QuestRunStatus::Completed)
            .cloned()
            .or_else(|| records.first().cloned());

        Ok(UserQuestHistoryResponse {
            user: user.map(UserProfileResponse::from),
            stats,
            active_run,
            runs: records,
            reward_claims,
            persistence: HistoryPersistenceStatus {
                available: true,
                message: None,
            },
        })
    }

    async fn get_learning_session(
        &self,
        user_id: &str,
    ) -> Result<Option<LearningSessionRecord>, ApiError> {
        let user_id = user_id.trim();
        if user_id.is_empty() {
            return Err(ApiError::InvalidPrompt);
        }
        if !self.is_configured() {
            return Ok(None);
        }

        let document = self
            .learning_sessions()
            .await?
            .find_one(doc! { "user_id": user_id, "status": { "$ne": "archived" } })
            .sort(doc! { "updated_at": -1 })
            .await?;

        Ok(document.map(LearningSessionRecord::from))
    }

    async fn list_learning_sessions(
        &self,
        user_id: &str,
    ) -> Result<Vec<LearningSessionRecord>, ApiError> {
        let user_id = user_id.trim();
        if user_id.is_empty() {
            return Err(ApiError::InvalidPrompt);
        }
        if !self.is_configured() {
            return Ok(Vec::new());
        }

        let mut cursor = self
            .learning_sessions()
            .await?
            .find(doc! { "user_id": user_id, "status": { "$ne": "archived" } })
            .sort(doc! { "updated_at": -1 })
            .limit(50)
            .await?;
        let mut sessions = Vec::new();
        while let Some(session) = cursor.try_next().await? {
            sessions.push(LearningSessionRecord::from(session));
        }

        Ok(sessions)
    }

    async fn save_learning_session(
        &self,
        principal: &AuthenticatedPrincipal,
        request: SaveLearningSessionRequest,
    ) -> Result<LearningSessionRecord, ApiError> {
        if !self.is_configured() {
            return Err(ApiError::DatabaseUnavailable);
        }

        let user_id = principal.user_id.trim();
        if user_id.is_empty() {
            return Err(ApiError::InvalidPrompt);
        }

        let module = compact_learning_module(request.module)?;
        let sessions = self.learning_sessions().await?;
        let now = BsonDateTime::now();
        let id = request
            .module_id
            .map(|module_id| clamp_text(module_id, 120))
            .filter(|module_id| !module_id.trim().is_empty())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let existing = sessions
            .find_one(doc! { "_id": &id, "user_id": user_id })
            .await?;
        let created_at = existing
            .as_ref()
            .map(|session| session.created_at)
            .unwrap_or(now);
        let document = LearningSessionDocument {
            id: id.clone(),
            user_id: user_id.to_string(),
            provider: principal.provider.clone(),
            provider_subject: principal.provider_subject.clone(),
            email: principal.email.clone(),
            name: principal.name.clone(),
            user_address: user_id.to_string(),
            wallet: None,
            source: request.source.unwrap_or(QuestSource::OpenAi),
            module_statuses: compact_module_generation_statuses(
                request.module_statuses,
                &module,
                5,
            ),
            eval_artifacts: compact_learning_eval_artifacts(request.eval_artifacts, &module, 5),
            module,
            ecosystem_id: request.ecosystem_id.map(|value| clamp_text(value, 48)),
            topic: request.topic.map(|value| clamp_text(value, 160)),
            learning_profile: request.learning_profile.map(|value| clamp_text(value, 80)),
            learning_intents: compact_string_list(request.learning_intents, 8, 120),
            selected_interests: compact_string_list(request.selected_interests, 8, 80),
            learner_goal: clamp_text(request.learner_goal, 360),
            background: clamp_text(request.background, 80),
            pace: clamp_text(request.pace, 80),
            active_lesson_index: request.active_lesson_index.min(20) as i64,
            checkpoint_answers: checkpoint_answers_document(request.checkpoint_answers),
            tutor_messages: compact_tutor_messages(request.tutor_messages),
            status: "active".to_string(),
            created_at,
            updated_at: now,
        };

        sessions
            .replace_one(doc! { "_id": &id, "user_id": user_id }, &document)
            .upsert(true)
            .await?;

        Ok(document.into())
    }

    async fn get_learning_session_by_id(
        &self,
        user_id: &str,
        module_id: &str,
    ) -> Result<Option<LearningSessionRecord>, ApiError> {
        let user_id = user_id.trim();
        let module_id = module_id.trim();
        if user_id.is_empty() || module_id.is_empty() {
            return Err(ApiError::InvalidPrompt);
        }
        if !self.is_configured() {
            return Ok(None);
        }
        let document = self
            .learning_sessions()
            .await?
            .find_one(
                doc! { "_id": module_id, "user_id": user_id, "status": { "$ne": "archived" } },
            )
            .await?;
        Ok(document.map(LearningSessionRecord::from))
    }

    async fn save_learning_event(
        &self,
        principal: &AuthenticatedPrincipal,
        request: LearningEventRequest,
    ) -> Result<(), ApiError> {
        if !self.is_configured() {
            return Err(ApiError::DatabaseUnavailable);
        }
        let event_type = normalized_learning_event_type(&request.event_type)?;
        let user_id = principal.user_id.trim();
        if user_id.is_empty() {
            return Err(ApiError::InvalidPrompt);
        }
        let document = LearningEventDocument {
            id: Uuid::new_v4().to_string(),
            user_id: user_id.to_string(),
            provider: principal.provider.clone(),
            event_type,
            module_id: request.module_id.map(|value| clamp_text(value, 120)),
            lesson_id: request.lesson_id.map(|value| clamp_text(value, 120)),
            ecosystem_id: request.ecosystem_id.map(|value| clamp_text(value, 48)),
            course_title: request.course_title.map(|value| clamp_text(value, 160)),
            metadata: compact_event_metadata(request.metadata),
            created_at: BsonDateTime::now(),
        };
        self.learning_events().await?.insert_one(document).await?;
        Ok(())
    }

    async fn list_learning_events(
        &self,
        user_id: &str,
        limit: i64,
    ) -> Result<Vec<LearningEventRecord>, ApiError> {
        let user_id = user_id.trim();
        if user_id.is_empty() {
            return Err(ApiError::InvalidPrompt);
        }
        if !self.is_configured() {
            return Ok(Vec::new());
        }
        let mut cursor = self
            .learning_events()
            .await?
            .find(doc! { "user_id": user_id })
            .sort(doc! { "created_at": -1 })
            .limit(limit.clamp(1, 500))
            .await?;
        let mut events = Vec::new();
        while let Some(event) = cursor.try_next().await? {
            events.push(LearningEventRecord::from(event));
        }
        Ok(events)
    }

    async fn archive_learning_session(
        &self,
        user_id: &str,
        module_id: &str,
    ) -> Result<bool, ApiError> {
        let user_id = user_id.trim();
        let module_id = module_id.trim();
        if user_id.is_empty() || module_id.is_empty() {
            return Err(ApiError::InvalidPrompt);
        }
        if !self.is_configured() {
            return Err(ApiError::DatabaseUnavailable);
        }

        let result = self
            .learning_sessions()
            .await?
            .update_one(
                doc! { "_id": module_id, "user_id": user_id },
                doc! { "$set": { "status": "archived", "updated_at": BsonDateTime::now() } },
            )
            .await?;
        Ok(result.matched_count > 0)
    }

    async fn delete_learning_session(
        &self,
        user_id: &str,
        module_id: &str,
    ) -> Result<bool, ApiError> {
        let user_id = user_id.trim();
        let module_id = module_id.trim();
        if user_id.is_empty() || module_id.is_empty() {
            return Err(ApiError::InvalidPrompt);
        }
        if !self.is_configured() {
            return Err(ApiError::DatabaseUnavailable);
        }

        let result = self
            .learning_sessions()
            .await?
            .delete_one(doc! { "_id": module_id, "user_id": user_id })
            .await?;
        Ok(result.deleted_count > 0)
    }

    async fn append_tutor_exchange(
        &self,
        principal: &AuthenticatedPrincipal,
        request: &SaveTutorExchangeRequest,
        answer: &LearningTutorResponse,
    ) -> Result<Option<LearningSessionRecord>, ApiError> {
        if !self.is_configured() {
            return Ok(None);
        }

        let user_id = principal.user_id.trim();
        if user_id.is_empty() {
            return Err(ApiError::InvalidPrompt);
        }
        let mut filter = doc! { "user_id": user_id };
        if let Some(module_id) = request
            .module_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            filter.insert("_id", module_id);
        }

        let mut session = match self.learning_sessions().await?.find_one(filter).await? {
            Some(session) => session,
            None => return Ok(None),
        };
        let now = Utc::now();
        let lesson_id = find_lesson_id_for_title(&session.module, &request.lesson_title);
        let module_title = Some(clamp_text(request.module_title.clone(), 140));
        let lesson_title = Some(clamp_text(request.lesson_title.clone(), 140));
        session.tutor_messages.push(LearningTutorMessage {
            id: format!("learner-{}", now.timestamp_millis()),
            role: "learner".to_string(),
            text: clamp_text(request.question.clone(), 900),
            why: None,
            follow_up: None,
            module_id: Some(session.id.clone()),
            module_title: module_title.clone(),
            lesson_id: lesson_id.clone(),
            lesson_title: lesson_title.clone(),
            created_at: now,
        });
        session.tutor_messages.push(LearningTutorMessage {
            id: format!("mentor-{}", now.timestamp_millis()),
            role: "mentor".to_string(),
            text: answer.answer.clone(),
            why: Some(answer.why_it_matters.clone()),
            follow_up: Some(answer.follow_up_question.clone()),
            module_id: Some(session.id.clone()),
            module_title,
            lesson_id,
            lesson_title,
            created_at: now,
        });
        session.tutor_messages = compact_tutor_messages(session.tutor_messages);
        session.updated_at = BsonDateTime::now();

        self.learning_sessions()
            .await?
            .replace_one(doc! { "_id": &session.id, "user_id": user_id }, &session)
            .await?;

        Ok(Some(session.into()))
    }

    async fn get_run(&self, run_id: &str) -> Result<QuestRunDocument, ApiError> {
        self.quest_runs()
            .await?
            .find_one(doc! { "_id": run_id })
            .await?
            .ok_or(ApiError::QuestNotFound)
    }

    async fn update_progress(
        &self,
        run_id: &str,
        request: UpdateQuestProgressRequest,
    ) -> Result<QuestRunRecord, ApiError> {
        validate_wallet_proof(&request.wallet)?;

        let mut run = self.get_run(run_id).await?;
        if run.user_address != request.wallet.address.trim() {
            return Err(ApiError::WalletMismatch);
        }

        if let Some(gates) = request.gates {
            run.progress.gates = gates;
        }
        if let Some(boss_fight_solved) = request.boss_fight_solved {
            run.progress.boss_fight_solved = boss_fight_solved;
        }
        if let Some(attempt) = request.boss_attempt {
            run.boss_attempts.push(compact_boss_attempt(attempt));
            if run.boss_attempts.len() > 20 {
                let drain_count = run.boss_attempts.len() - 20;
                run.boss_attempts.drain(0..drain_count);
            }
        }
        if let Some(shipped) = request.shipped {
            if shipped {
                return Err(ApiError::CompletionNotVerified);
            }
            run.progress.shipped = false;
        }

        run.status = status_for_progress(&run.progress);
        run.updated_at = BsonDateTime::now();
        if run.status == QuestRunStatus::Completed && run.completed_at.is_none() {
            run.completed_at = Some(run.updated_at);
        }
        if run.status != QuestRunStatus::Completed {
            run.completed_at = None;
        }

        self.quest_runs()
            .await?
            .replace_one(doc! { "_id": run_id }, &run)
            .await?;
        self.refresh_user_counts(&run.user_address).await?;

        Ok(run.into())
    }

    async fn append_code_tutor_exchange(
        &self,
        run_id: &str,
        wallet: &WalletProof,
        request: &CodeTutorRequest,
        answer: &CodeTutorResponse,
    ) -> Result<(), ApiError> {
        if !self.is_configured() {
            return Ok(());
        }

        validate_wallet_proof(wallet)?;
        let mut run = self.get_run(run_id).await?;
        if run.user_address != wallet.address.trim() {
            return Err(ApiError::WalletMismatch);
        }

        let now = Utc::now();
        run.code_tutor_messages.push(CodeTutorMessage {
            id: format!("learner-{}", now.timestamp_millis()),
            role: "learner".to_string(),
            text: clamp_text(request.question.clone(), 700),
            code_walkthrough: Vec::new(),
            common_misunderstanding: None,
            follow_up_question: None,
            references: Vec::new(),
            created_at: now,
        });
        run.code_tutor_messages.push(CodeTutorMessage {
            id: format!("mentor-{}", now.timestamp_millis()),
            role: "mentor".to_string(),
            text: answer.answer.clone(),
            code_walkthrough: answer.code_walkthrough.clone(),
            common_misunderstanding: Some(answer.common_misunderstanding.clone()),
            follow_up_question: Some(answer.follow_up_question.clone()),
            references: answer.references.clone(),
            created_at: now,
        });
        run.code_tutor_messages = compact_code_tutor_messages(run.code_tutor_messages);
        run.updated_at = BsonDateTime::now();

        self.quest_runs()
            .await?
            .replace_one(doc! { "_id": run_id }, &run)
            .await?;

        Ok(())
    }

    async fn complete_quest(
        &self,
        run_id: &str,
        request: CompleteQuestRequest,
        reward_amount_shannons: u128,
        reward_currency: &str,
        fiber: &FiberPayoutClient,
    ) -> Result<CompleteQuestResponse, ApiError> {
        validate_wallet_proof(&request.wallet)?;
        validate_reward_invoice(&request.fiber_invoice)?;

        let mut run = self.get_run(run_id).await?;
        if run.user_address != request.wallet.address.trim() {
            return Err(ApiError::WalletMismatch);
        }

        run.progress.gates = request.gates;
        run.progress.boss_fight_solved = request.boss_fight_solved;
        let proof = server_completion_proof(&run)?;
        run.progress.shipped = true;
        run.status = QuestRunStatus::Completed;
        run.updated_at = BsonDateTime::now();
        if run.completed_at.is_none() {
            run.completed_at = Some(run.updated_at);
        }
        run.reward = RewardSnapshot {
            amount_shannons: reward_amount_shannons.to_string(),
            currency: reward_currency.to_string(),
            sponsor: "vibequest-core".to_string(),
        };

        let claim_id = format!("{}:{}", run.run_id, run.user_address);
        let claims = self.reward_claims().await?;
        let existing_claim = claims.find_one(doc! { "_id": &claim_id }).await?;
        if existing_claim.is_some_and(|existing| {
            matches!(
                existing.status,
                RewardClaimStatus::Paid | RewardClaimStatus::Paying
            )
        }) {
            return Err(ApiError::RewardAlreadyProcessed);
        }

        let now = BsonDateTime::now();
        let mut claim = RewardClaimDocument {
            claim_id: claim_id.clone(),
            run_id: run.run_id.clone(),
            user_address: run.user_address.clone(),
            fiber_invoice: request.fiber_invoice.trim().to_string(),
            amount_shannons: reward_amount_shannons.to_string(),
            currency: reward_currency.to_string(),
            status: if fiber.enabled {
                RewardClaimStatus::Paying
            } else {
                RewardClaimStatus::Verified
            },
            verification: proof,
            fiber_payment: None,
            error: None,
            created_at: now,
            updated_at: now,
            paid_at: None,
        };

        self.quest_runs()
            .await?
            .replace_one(doc! { "_id": run_id }, &run)
            .await?;

        claims
            .replace_one(doc! { "_id": &claim_id }, &claim)
            .upsert(true)
            .await?;

        match fiber.pay_invoice(&claim.fiber_invoice).await {
            Ok(receipt) => {
                claim.status = if fiber.enabled {
                    RewardClaimStatus::Paid
                } else {
                    RewardClaimStatus::Verified
                };
                claim.fiber_payment = receipt;
                claim.error = None;
                claim.paid_at = if fiber.enabled {
                    Some(BsonDateTime::now())
                } else {
                    None
                };
            }
            Err(error) => {
                claim.status = RewardClaimStatus::Failed;
                claim.error = Some(error.to_string());
            }
        }
        claim.updated_at = BsonDateTime::now();

        claims
            .replace_one(doc! { "_id": &claim_id }, &claim)
            .await?;
        self.refresh_user_counts(&run.user_address).await?;

        Ok(CompleteQuestResponse {
            run: run.into(),
            claim: claim.into(),
        })
    }

    async fn counts_for_user(&self, address: &str) -> Result<UserQuestCounts, ApiError> {
        let runs = self.quest_runs().await?;
        let created = runs
            .count_documents(doc! { "user_address": address })
            .await? as i64;
        let completed = runs
            .count_documents(doc! { "user_address": address, "status": "completed" })
            .await? as i64;

        Ok(UserQuestCounts {
            created,
            completed,
            uncompleted: created.saturating_sub(completed),
        })
    }

    async fn refresh_user_counts(&self, address: &str) -> Result<(), ApiError> {
        let counts = self.counts_for_user(address).await?;
        self.users()
            .await?
            .update_one(
                doc! { "_id": address },
                doc! {
                    "$set": {
                        "quest_counts.created": counts.created,
                        "quest_counts.completed": counts.completed,
                        "quest_counts.uncompleted": counts.uncompleted,
                        "updated_at": BsonDateTime::now(),
                    }
                },
            )
            .await?;
        Ok(())
    }
}

impl From<UserDocument> for UserProfileResponse {
    fn from(user: UserDocument) -> Self {
        Self {
            address: user.address,
            quest_counts: user.quest_counts,
            created_at: bson_datetime_to_utc(user.created_at),
            updated_at: bson_datetime_to_utc(user.updated_at),
            last_seen_at: bson_datetime_to_utc(user.last_seen_at),
        }
    }
}

impl From<QuestRunDocument> for QuestRunRecord {
    fn from(run: QuestRunDocument) -> Self {
        Self {
            run_id: run.run_id,
            user_address: run.user_address,
            build_prompt: run.build_prompt,
            skill_track: run.skill_track,
            difficulty: run.difficulty,
            learning_context: run.learning_context,
            source: run.source,
            quest: run.quest,
            ship_requirements: run.ship_requirements,
            progress: run.progress,
            boss_attempts: run.boss_attempts,
            code_tutor_messages: run.code_tutor_messages,
            status: run.status,
            created_at: bson_datetime_to_utc(run.created_at),
            updated_at: bson_datetime_to_utc(run.updated_at),
            completed_at: run.completed_at.map(bson_datetime_to_utc),
            reward: run.reward,
        }
    }
}

impl From<LearningEventDocument> for LearningEventRecord {
    fn from(event: LearningEventDocument) -> Self {
        Self {
            event_id: event.id,
            event_type: event.event_type,
            module_id: event.module_id,
            lesson_id: event.lesson_id,
            ecosystem_id: event.ecosystem_id,
            course_title: event.course_title,
            metadata: event.metadata,
            created_at: bson_datetime_to_utc(event.created_at),
        }
    }
}

fn normalized_learning_event_type(value: &str) -> Result<String, ApiError> {
    let normalized = value.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    let allowed = [
        "course_generated",
        "module_opened",
        "checkpoint_attempted",
        "checkpoint_passed",
        "course_completed",
        "tutor_used",
        "ecosystem_selected",
        "generation_failed",
        "module_regenerated",
        "course_archived",
        "course_deleted",
    ];
    if allowed.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(ApiError::InvalidPrompt)
    }
}

fn compact_event_metadata(metadata: BTreeMap<String, String>) -> BTreeMap<String, String> {
    metadata
        .into_iter()
        .filter_map(|(key, value)| {
            let key = clamp_text(key.trim().to_ascii_lowercase().replace([' ', '-'], "_"), 48);
            if key.is_empty() {
                None
            } else {
                Some((key, clamp_text(value, 180)))
            }
        })
        .take(16)
        .collect()
}

fn summarize_learning_events(events: &[LearningEventRecord]) -> LearningMetricsSummary {
    let mut summary = LearningMetricsSummary {
        total_events: events.len(),
        ..Default::default()
    };
    for event in events {
        *summary
            .by_event
            .entry(event.event_type.clone())
            .or_insert(0) += 1;
        if let Some(ecosystem_id) = event
            .ecosystem_id
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            *summary
                .by_ecosystem
                .entry(ecosystem_id.clone())
                .or_insert(0) += 1;
        }
        match event.event_type.as_str() {
            "course_generated" => summary.courses_generated += 1,
            "module_opened" => summary.modules_opened += 1,
            "checkpoint_attempted" => summary.checkpoints_attempted += 1,
            "checkpoint_passed" => summary.checkpoints_passed += 1,
            "course_completed" => summary.courses_completed += 1,
            "tutor_used" => summary.tutor_used += 1,
            "generation_failed" => summary.generation_failures += 1,
            _ => {}
        }
    }
    summary
}

fn learning_session_markdown(session: &LearningSessionRecord) -> String {
    let mut markdown = String::new();
    markdown.push_str(&format!("# {}\n\n", session.module.title));
    markdown.push_str(&format!(
        "- Ecosystem: {}\n",
        session
            .ecosystem_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string())
    ));
    markdown.push_str(&format!(
        "- Topic: {}\n",
        session
            .topic
            .clone()
            .unwrap_or_else(|| "untitled".to_string())
    ));
    markdown.push_str(&format!("- Status: {}\n", session.status));
    markdown.push_str(&format!(
        "- Active module: {}\n\n",
        session.active_lesson_index + 1
    ));
    markdown.push_str(&format!("## Outcome\n\n{}\n\n", session.module.outcome));
    markdown.push_str("## Module Statuses\n\n");
    for status in &session.module_statuses {
        markdown.push_str(&format!(
            "- Module {}: {}{}\n",
            status.lesson_index + 1,
            status.status,
            status
                .error
                .as_ref()
                .map(|error| format!(" — {}", error))
                .unwrap_or_default()
        ));
    }
    markdown.push_str("\n## Lessons\n\n");
    for (index, lesson) in session.module.lessons.iter().enumerate() {
        markdown.push_str(&format!("### {}. {}\n\n", index + 1, lesson.title));
        markdown.push_str(&format!("{}\n\n", lesson.why_it_matters));
        markdown.push_str(&format!("{}\n\n", lesson.explanation));
        markdown.push_str(&format!("Checkpoint: {}\n\n", lesson.checkpoint.question));
    }
    markdown
}

fn active_learning_session_status() -> String {
    "active".to_string()
}

fn normalized_learning_session_status(status: &str) -> String {
    match status.trim().to_ascii_lowercase().as_str() {
        "archived" => "archived".to_string(),
        "completed" => "completed".to_string(),
        _ => "active".to_string(),
    }
}

fn normalized_module_generation_status(status: &str) -> String {
    match status.trim().to_ascii_lowercase().as_str() {
        "queued" => "queued".to_string(),
        "generating" => "generating".to_string(),
        "ready" => "ready".to_string(),
        "failed" => "failed".to_string(),
        "validated" => "validated".to_string(),
        _ => "queued".to_string(),
    }
}

fn validation_state_from_lesson(lesson: &LearningLesson) -> LearningModuleValidationState {
    let quality = &lesson.quality_score;
    let source_grounding = !lesson.evidence_map.is_empty() && quality.source_coverage >= 75;
    let technical_depth = quality.technical_depth >= 75;
    let placeholder_free = quality.placeholder_free;
    let checkpoint_quality = quality.checkpoint_quality >= 75;
    let ecosystem_alignment = quality.ecosystem_alignment;
    LearningModuleValidationState {
        source_grounding,
        technical_depth,
        placeholder_free,
        repetition_check: true,
        checkpoint_quality,
        ecosystem_alignment,
        passed: source_grounding
            && technical_depth
            && placeholder_free
            && checkpoint_quality
            && ecosystem_alignment
            && quality.passed,
    }
}

fn module_generation_status_for_lesson(
    lesson_index: usize,
    lesson: &LearningLesson,
    status: &str,
    error: Option<String>,
) -> LearningModuleGenerationState {
    let validation = validation_state_from_lesson(lesson);
    let resolved_status = match status {
        "failed" => "failed".to_string(),
        "generating" => "generating".to_string(),
        _ if validation.passed => "validated".to_string(),
        _ => "ready".to_string(),
    };
    LearningModuleGenerationState {
        lesson_index,
        lesson_id: Some(lesson.id.clone()),
        status: resolved_status,
        validation,
        error,
        updated_at: Utc::now().to_rfc3339(),
    }
}

fn learning_module_statuses_from_module(
    module: &LearningModule,
    total_lessons: usize,
) -> Vec<LearningModuleGenerationState> {
    let total = total_lessons.max(module.lessons.len()).max(1);
    (0..total)
        .map(|index| {
            if let Some(lesson) = module.lessons.get(index) {
                module_generation_status_for_lesson(index, lesson, "ready", None)
            } else {
                LearningModuleGenerationState {
                    lesson_index: index,
                    lesson_id: None,
                    status: "queued".to_string(),
                    validation: LearningModuleValidationState::default(),
                    error: None,
                    updated_at: Utc::now().to_rfc3339(),
                }
            }
        })
        .collect()
}

fn compact_module_generation_statuses(
    statuses: Vec<LearningModuleGenerationState>,
    module: &LearningModule,
    total_lessons: usize,
) -> Vec<LearningModuleGenerationState> {
    let total = total_lessons.max(module.lessons.len()).max(1);
    let mut merged = learning_module_statuses_from_module(module, total);

    for status in statuses {
        if status.lesson_index >= total {
            continue;
        }
        let lesson = module.lessons.get(status.lesson_index);
        let mut normalized = LearningModuleGenerationState {
            lesson_index: status.lesson_index,
            lesson_id: status
                .lesson_id
                .or_else(|| lesson.map(|item| item.id.clone())),
            status: normalized_module_generation_status(&status.status),
            validation: status.validation,
            error: status.error.map(|value| clamp_text(value, 240)),
            updated_at: if status.updated_at.trim().is_empty() {
                Utc::now().to_rfc3339()
            } else {
                clamp_text(status.updated_at, 64)
            },
        };

        if let Some(lesson) = lesson {
            let validation = validation_state_from_lesson(lesson);
            if normalized.validation == LearningModuleValidationState::default()
                || validation.passed
            {
                normalized.validation = validation;
            }
            if normalized.status != "failed" && normalized.status != "generating" {
                normalized.status = if normalized.validation.passed {
                    "validated".to_string()
                } else {
                    "ready".to_string()
                };
            }
        }
        merged[status.lesson_index] = normalized;
    }

    merged
}

fn compact_learning_eval_artifacts(
    artifacts: Vec<LearningEvalArtifact>,
    module: &LearningModule,
    total_lessons: usize,
) -> Vec<LearningEvalArtifact> {
    let lesson_ids = module
        .lessons
        .iter()
        .map(|lesson| lesson.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let limit = total_lessons.max(module.lessons.len()).max(1);
    let mut seen_lessons = std::collections::BTreeSet::new();
    let mut compacted = Vec::new();

    for artifact in artifacts.into_iter().rev() {
        let report_lesson_id = artifact
            .lesson_reports
            .iter()
            .find(|report| lesson_ids.contains(&report.lesson_id))
            .map(|report| report.lesson_id.clone());
        let Some(report_lesson_id) = report_lesson_id else {
            continue;
        };
        if !seen_lessons.insert(report_lesson_id) {
            continue;
        }
        compacted.push(compact_learning_eval_artifact(artifact, limit));
        if compacted.len() >= limit {
            break;
        }
    }

    compacted.reverse();
    compacted
}

fn compact_learning_eval_artifact(
    artifact: LearningEvalArtifact,
    lesson_report_limit: usize,
) -> LearningEvalArtifact {
    LearningEvalArtifact {
        artifact_version: clamp_text(artifact.artifact_version, 64),
        ecosystem_id: clamp_text(artifact.ecosystem_id, 48),
        topic: artifact.topic.map(|topic| clamp_text(topic, 160)),
        learning_profile: artifact
            .learning_profile
            .map(|profile| clamp_text(profile, 80)),
        learning_intents: compact_string_list(artifact.learning_intents, 10, 140),
        request_hash: clamp_text(artifact.request_hash, 96),
        provider: compact_ai_provider_metadata(artifact.provider),
        module_title: clamp_text(artifact.module_title, 160),
        lesson_count: artifact.lesson_count.min(20),
        validation: artifact.validation,
        lesson_reports: artifact
            .lesson_reports
            .into_iter()
            .map(compact_learning_lesson_eval_report)
            .take(lesson_report_limit)
            .collect(),
        warnings: compact_string_list(artifact.warnings, 16, 220),
        integration_tags: compact_string_list(artifact.integration_tags, 12, 80),
        source_ids: compact_string_list(artifact.source_ids, 16, 80),
        source_categories: compact_string_list(artifact.source_categories, 12, 80),
        code_mode_enabled: artifact.code_mode_enabled,
        final_lab_ready: artifact.final_lab_ready,
        denial_tests_count: artifact.denial_tests_count.min(99),
        unsupported_claim_warnings: compact_string_list(
            artifact.unsupported_claim_warnings,
            16,
            220,
        ),
        compute_model_coverage: compact_string_list(artifact.compute_model_coverage, 12, 80),
        execution_path: artifact.execution_path.map(|path| clamp_text(path, 80)),
        task_lifecycle_covered: artifact.task_lifecycle_covered,
        failure_cases_count: artifact.failure_cases_count.min(99),
        final_compute_lab_ready: artifact.final_compute_lab_ready,
        generated_at: artifact.generated_at,
    }
}

fn compact_learning_lesson_eval_report(
    report: LearningLessonEvalReport,
) -> LearningLessonEvalReport {
    LearningLessonEvalReport {
        lesson_id: clamp_text(report.lesson_id, 120),
        title: clamp_text(report.title, 160),
        validation: report.validation,
        quality_score: report.quality_score,
        source_titles: compact_string_list(report.source_titles, 8, 100),
        source_urls: compact_string_list(report.source_urls, 8, 220),
        source_ids: compact_string_list(report.source_ids, 12, 80),
        source_categories: compact_string_list(report.source_categories, 10, 80),
        warning_count: report.warning_count.min(99),
    }
}

fn compact_ai_provider_metadata(provider: AiProviderMetadata) -> AiProviderMetadata {
    AiProviderMetadata {
        provider_kind: clamp_text(provider.provider_kind, 64),
        model: clamp_text(provider.model, 80),
        endpoint_origin: clamp_text(provider.endpoint_origin, 160),
        reasoning_effort: provider.reasoning_effort,
        response_storage_disabled: provider.response_storage_disabled,
        timeout_seconds: provider.timeout_seconds.min(600),
        configured: provider.configured,
    }
}

fn wants_code_snippets_for_request(request: &GenerateLearningModuleRequest) -> bool {
    request
        .learning_intents
        .iter()
        .chain(request.interests.iter())
        .any(|item| {
            let lower = item.to_ascii_lowercase();
            lower.contains("code snippet")
                || lower.contains("interactive code")
                || lower.contains("code sample")
        })
}

fn learning_source_ids_for_module(module: &LearningModule) -> Vec<String> {
    let urls = module
        .resources
        .iter()
        .map(|resource| resource.url.clone())
        .chain(
            module
                .lessons
                .iter()
                .flat_map(|lesson| unique_learning_source_urls(lesson)),
        )
        .collect::<Vec<_>>();
    learning_source_ids_from_urls(&urls)
}

fn learning_source_ids_from_urls(urls: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    for url in urls {
        if let Some(id) = learning_source_id_for_url(url) {
            if seen.insert(id) {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

fn learning_source_id_for_url(url: &str) -> Option<&'static str> {
    let lower = url.to_ascii_lowercase();
    if lower == "https://golem.network/ecosystem" || lower.contains("golem.network/ecosystem") {
        Some("golem-ecosystem-fund")
    } else if lower == "https://docs.golem.network/" || lower == "https://docs.golem.network" {
        Some("golem-docs")
    } else if lower.contains("docs.golem.network/docs/quickstarts") {
        Some("golem-quickstarts")
    } else if lower.contains("docs.golem.network/docs/creators/javascript/quickstarts/quickstart") {
        Some("golem-js-quickstart")
    } else if lower.contains("docs.golem.network/docs/creators/javascript/guides/task-model") {
        Some("golem-js-task-model")
    } else if lower.contains("docs.golem.network/docs/creators/javascript/examples/executing-tasks")
    {
        Some("golem-js-executing-tasks")
    } else if lower.contains("docs.golem.network/docs/creators/javascript") {
        Some("golem-js-sdk")
    } else if lower
        .contains("docs.golem.network/docs/creators/common/requestor-provider-interaction")
    {
        Some("golem-requestor-provider")
    } else if lower
        .contains("docs.golem.network/docs/creators/python/quickstarts/run-first-task-on-golem")
    {
        Some("golem-python-quickstart")
    } else if lower
        .contains("docs.golem.network/docs/creators/python/guides/application-fundamentals")
    {
        Some("golem-python-fundamentals")
    } else if lower
        .contains("docs.golem.network/docs/creators/ray/supported-versions-and-other-limitations")
    {
        Some("golem-ray-limitations")
    } else if lower.contains("docs.golem.network/docs/creators/ray") {
        Some("golem-ray")
    } else if lower.contains("docs.golem.network/docs/creators/dapps/hello-world-dapp") {
        Some("golem-dapp-hello-world")
    } else if lower.contains("docs.golem.network/docs/creators/dapps/creating-golem-dapps") {
        Some("golem-dapp-creation")
    } else if lower.contains("docs.golem.network/docs/providers") {
        Some("golem-provider-overview")
    } else if lower.contains("docs.golem.network/docs/golem/overview/provider") {
        Some("golem-provider-architecture")
    } else if lower.contains("docs.ston.fi/developer-section/dex/overview") {
        Some("stonfi-dex-overview")
    } else if lower.contains("docs.ston.fi/developer-section/dex/sdk") {
        Some("stonfi-dex-sdk")
    } else if lower.contains("docs.ston.fi/developer-section/dex/smart-contracts") {
        Some("stonfi-dex-smart-contracts")
    } else if lower.contains("docs.ston.fi/developer-section/dex/api") {
        Some("stonfi-dex-rest-api")
    } else if lower.contains("docs.ston.fi/developer-section/widget/widget") {
        Some("stonfi-omniston-widget-guide")
    } else if lower.contains("docs.ston.fi/developer-section/widget") {
        Some("stonfi-omniston-widget")
    } else if lower.contains("docs.ston.fi/developer-section/omniston/sdk") {
        Some("stonfi-omniston-sdk")
    } else if lower.contains("docs.ton.org/applications/ton-connect/api-reference/ui") {
        Some("ton-connect-ui")
    } else if lower.contains("docs.ton.org/applications/ton-connect/overview") {
        Some("ton-connect-overview")
    } else if lower.contains("docs.ton.org/contracts/standard/tokens/overview") {
        Some("ton-token-overview")
    } else if lower.contains("docs.ton.org/applications/payments/jettons") {
        Some("ton-jetton-processing")
    } else if lower.contains("docs.ton.org/contracts/standard/tokens/jettons/api") {
        Some("ton-jetton-interface")
    } else if lower.contains("docs.ton.org/contracts/standard/tokens/jettons/how-it-works") {
        Some("ton-jetton-architecture")
    } else if lower.contains("docs.stacks.co") {
        Some("stacks-docs")
    } else if lower.contains("zcash.readthedocs") {
        Some("zcash-docs")
    } else if lower.contains("zips.z.cash/zip-0321") {
        Some("zcash-zip-321")
    } else if lower.contains("docs.nervos.org") {
        Some("ckb-docs")
    } else if lower.contains("github.com/nervosnetwork/fiber") {
        Some("fiber-repo")
    } else if lower.contains("ethereum.org/developers/docs") {
        Some("ethereum-developer-docs")
    } else {
        None
    }
}

fn learning_source_categories_from_ids(ids: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut categories = Vec::new();
    for id in ids {
        let category = learning_source_category_for_id(id);
        if seen.insert(category) {
            categories.push(category.to_string());
        }
    }
    categories
}

fn learning_source_category_for_id(id: &str) -> &'static str {
    match id {
        "golem-ecosystem-fund" => "golem-ecosystem",
        "golem-docs" | "golem-quickstarts" => "golem-foundations",
        "golem-js-sdk"
        | "golem-js-quickstart"
        | "golem-js-task-model"
        | "golem-js-executing-tasks" => "golem-js-sdk",
        "golem-requestor-provider" => "golem-requestor-provider",
        "golem-python-quickstart" | "golem-python-fundamentals" => "golem-python",
        "golem-ray" | "golem-ray-limitations" => "golem-ray",
        "golem-dapp-hello-world" | "golem-dapp-creation" => "golem-dapp",
        "golem-provider-overview" | "golem-provider-architecture" => "golem-provider",
        "stonfi-dex-overview" | "stonfi-dex-smart-contracts" => "stonfi-dex",
        "stonfi-dex-sdk" => "stonfi-sdk",
        "stonfi-dex-rest-api" => "rest-api",
        "stonfi-omniston-widget" | "stonfi-omniston-widget-guide" => "omniston-widget",
        "stonfi-omniston-sdk" => "omniston-sdk",
        "ton-connect-overview" | "ton-connect-ui" => "ton-connect",
        "ton-token-overview"
        | "ton-jetton-processing"
        | "ton-jetton-interface"
        | "ton-jetton-architecture" => "jetton-standard",
        "stacks-docs" => "stacks",
        "zcash-docs" | "zcash-zip-321" => "zcash",
        "ckb-docs" => "ckb",
        "fiber-repo" => "fiber",
        "ethereum-developer-docs" => "web3-basics",
        _ => "source-pack",
    }
}

fn learning_unsupported_claim_warnings(
    request: &GenerateLearningModuleRequest,
    module: &LearningModule,
) -> Vec<String> {
    let ecosystem_id = learning_ecosystem_id(request);
    if ecosystem_id == "golem" {
        return golem_unsupported_claim_warnings(module);
    }
    if ecosystem_id != "ton-stonfi" {
        return Vec::new();
    }
    let combined = learning_module_full_text(module).to_ascii_lowercase();
    let mut warnings = Vec::new();

    if risky_phrase_present(
        &combined,
        &[
            "widget proves",
            "widget confirms",
            "widget output proves",
            "sdk response proves",
            "sdk output proves",
        ],
    ) {
        warnings.push("SDK/widget output is being treated as settlement proof; require wallet and transaction-state evidence.".to_string());
    }
    if (combined.contains("token symbol")
        || combined.contains("token name")
        || combined.contains("token image"))
        && !(combined.contains("jetton master")
            || combined.contains("master address")
            || combined.contains("allowlist"))
    {
        warnings.push(
            "Token metadata appears without jetton master or allowlist verification.".to_string(),
        );
    }
    if combined.contains("ton connect") && !combined.contains("manifest") {
        warnings.push(
            "TON Connect is mentioned without manifest or domain-boundary constraints.".to_string(),
        );
    }
    if risky_phrase_present(
        &combined,
        &[
            "pending transaction is success",
            "pending state is success",
            "pending transaction proves",
            "pending proves",
        ],
    ) {
        warnings.push("Pending transaction state is being treated as success; require confirmed transaction evidence.".to_string());
    }
    if risky_phrase_present(
        &combined,
        &[
            "rest api proves",
            "api response proves",
            "quote response proves",
            "rest response confirms settlement",
        ],
    ) {
        warnings.push("REST API response is being treated as final on-chain proof.".to_string());
    }
    if combined.contains("referral") && combined.contains("fee") && !combined.contains("disclos") {
        warnings.push(
            "Referral fee is mentioned without clear disclosure to preserve user intent."
                .to_string(),
        );
    }
    if risky_phrase_present(
        &combined,
        &[
            "production safe",
            "safe for production",
            "ship to production",
        ],
    ) && learning_denial_test_count(request, module) < 2
    {
        warnings.push(
            "Production-safety claim appears without enough denial-test coverage.".to_string(),
        );
    }

    warnings
}

fn golem_unsupported_claim_warnings(module: &LearningModule) -> Vec<String> {
    let combined = learning_module_full_text(module).to_ascii_lowercase();
    let mut warnings = Vec::new();

    if risky_phrase_present(
        &combined,
        &[
            "smart contract executes compute",
            "on-chain compute executes",
            "blockchain executes the workload",
        ],
    ) {
        warnings.push("Golem workload execution is being described like smart-contract execution; keep requestor/provider/Yagna boundaries explicit.".to_string());
    }
    if risky_phrase_present(
        &combined,
        &[
            "provider result proves correctness",
            "provider output is automatically trusted",
            "provider output proves correctness",
            "result is automatically trusted",
        ],
    ) {
        warnings.push("Provider output is being treated as automatically correct; require result validation, retries, or verification strategy.".to_string());
    }
    if risky_phrase_present(
        &combined,
        &[
            "free compute",
            "zero-cost compute",
            "unlimited compute",
            "guaranteed cheapest compute",
        ],
    ) {
        warnings.push("Cost or availability is overclaimed; require budget, agreement, provider, and workload constraints.".to_string());
    }
    if risky_phrase_present(
        &combined,
        &[
            "guaranteed gpu",
            "unlimited gpu",
            "production gpu inference",
            "any ai workload",
        ],
    ) {
        warnings.push("AI/GPU capability is overclaimed; ground workload support in official Golem docs and current limitations.".to_string());
    }
    if combined.contains("ray")
        && !combined.contains("limitation")
        && !combined.contains("supported version")
    {
        warnings.push(
            "Ray is mentioned without limitations or supported-version constraints.".to_string(),
        );
    }
    if risky_phrase_present(
        &combined,
        &[
            "production certified",
            "production-ready certification",
            "certifies production",
        ],
    ) {
        warnings.push("The lesson implies production certification; VibeQuest can teach and validate learning artifacts, not certify deployments.".to_string());
    }

    warnings
}

fn risky_phrase_present(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| text.contains(*phrase))
}

fn learning_module_full_text(module: &LearningModule) -> String {
    format!(
        "{} {} {}",
        module.title,
        module.outcome,
        module
            .lessons
            .iter()
            .map(|lesson| format!(
                "{} {} {} {} {} {}",
                lesson.title,
                lesson.explanation,
                lesson.why_it_matters,
                lesson.quest_bridge,
                lesson.checkpoint.question,
                lesson.concepts.join(" ")
            ))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn learning_denial_test_count(
    request: &GenerateLearningModuleRequest,
    module: &LearningModule,
) -> usize {
    let ecosystem_id = learning_ecosystem_id(request);
    if ecosystem_id == "golem" {
        return golem_failure_case_count(module);
    }
    if ecosystem_id != "ton-stonfi" {
        return module
            .lessons
            .iter()
            .map(|lesson| {
                usize::from(
                    lesson
                        .explanation
                        .to_ascii_lowercase()
                        .contains("denial test"),
                )
            })
            .sum();
    }
    let combined = learning_module_full_text(module).to_ascii_lowercase();
    let denial_cases = [
        ["fake jetton", "fake-token", "fake token"].as_slice(),
        ["misleading token metadata", "token metadata"].as_slice(),
        ["changed token pair", "mutate token pair", "wrong pair"].as_slice(),
        ["stale quote", "quote timestamp"].as_slice(),
        ["missing min-out", "missing minout"].as_slice(),
        ["min-out set too low", "unsafe min-out", "unsafe minout"].as_slice(),
        ["wallet disconnected", "disconnected wallet"].as_slice(),
        ["rejected wallet", "wallet rejection", "approval rejected"].as_slice(),
        ["pending transaction", "pending state"].as_slice(),
        [
            "wrong ton connect manifest",
            "manifest domain",
            "wrong manifest",
        ]
        .as_slice(),
        [
            "duplicate ton connect",
            "duplicate connector",
            "duplicate instance",
        ]
        .as_slice(),
        ["referral fee", "referrer fee"].as_slice(),
        ["rest api response", "api response", "rest response"].as_slice(),
    ];
    denial_cases
        .iter()
        .filter(|terms| terms.iter().any(|term| combined.contains(*term)))
        .count()
}

fn learning_final_lab_ready(
    request: &GenerateLearningModuleRequest,
    module: &LearningModule,
) -> bool {
    if learning_ecosystem_id(request) == "golem" {
        return golem_final_compute_lab_ready(request, module);
    }
    if learning_ecosystem_id(request) != "ton-stonfi" {
        return false;
    }
    let Some(final_lesson) = module.lessons.last() else {
        return false;
    };
    let final_lesson_marker = module.lessons.len() >= 5
        || final_lesson.id.starts_with("module-5-")
        || final_lesson.title.to_ascii_lowercase().contains("final")
        || final_lesson.title.to_ascii_lowercase().contains("lab");
    if !final_lesson_marker {
        return false;
    }
    let text = format!(
        "{} {} {} {}",
        final_lesson.title,
        final_lesson.explanation,
        final_lesson.quest_bridge,
        final_lesson.checkpoint.question
    )
    .to_ascii_lowercase();

    (text.contains("final") || text.contains("lab") || text.contains("quest"))
        && text.contains("ston.fi")
        && text.contains("denial")
        && text.contains("transaction")
        && learning_denial_test_count(request, module) >= 8
}

fn learning_integration_tags(request: &GenerateLearningModuleRequest) -> Vec<String> {
    match learning_ecosystem_id(request).as_str() {
        "golem" => vec![
            "decentralized-compute".to_string(),
            "requestor-provider".to_string(),
            "yagna".to_string(),
            "js-sdk".to_string(),
            "python-sdk".to_string(),
            "ray-on-golem".to_string(),
            "dapp-deployment".to_string(),
            "task-lifecycle".to_string(),
            "failure-state".to_string(),
        ],
        "ton-stonfi" => vec![
            "sdk".to_string(),
            "widget".to_string(),
            "ton-connect".to_string(),
            "jetton".to_string(),
            "slippage".to_string(),
            "quote-freshness".to_string(),
            "transaction-state".to_string(),
        ],
        "zcash" => vec![
            "zip-321".to_string(),
            "shielded-address".to_string(),
            "viewing-key".to_string(),
            "memo".to_string(),
            "confirmation-safety".to_string(),
        ],
        "stacks" => vec![
            "clarity".to_string(),
            "wallet-authorization".to_string(),
            "post-condition".to_string(),
            "sbtc".to_string(),
            "bns".to_string(),
        ],
        "fiber" => vec![
            "invoice".to_string(),
            "ptlc".to_string(),
            "channel-state".to_string(),
            "replay-defense".to_string(),
        ],
        "ckb" => vec![
            "cell".to_string(),
            "outpoint".to_string(),
            "script".to_string(),
            "witness".to_string(),
        ],
        _ => vec!["source-grounded-learning".to_string()],
    }
}

fn learning_eval_artifact(
    request: &GenerateLearningModuleRequest,
    module: &LearningModule,
    provider: AiProviderMetadata,
) -> LearningEvalArtifact {
    let lesson_reports = module
        .lessons
        .iter()
        .map(learning_lesson_eval_report)
        .collect::<Vec<_>>();
    let validation = validation_state_from_module(module);
    let mut warnings = learning_eval_warnings(module, &validation, &lesson_reports);
    let unsupported_claim_warnings = learning_unsupported_claim_warnings(request, module);
    warnings.extend(unsupported_claim_warnings.clone());
    let source_ids = learning_source_ids_for_module(module);
    let denial_tests_count = learning_denial_test_count(request, module);
    let compute_model_coverage = golem_compute_model_coverage(request, module);
    let execution_path = golem_execution_path(request, module);
    let task_lifecycle_covered = golem_task_lifecycle_covered(request, module);
    let failure_cases_count = golem_failure_case_count_for_request(request, module);
    let final_compute_lab_ready = golem_final_compute_lab_ready(request, module);

    LearningEvalArtifact {
        artifact_version: "vibequest-learning-eval-v1".to_string(),
        ecosystem_id: learning_ecosystem_id(request),
        topic: learning_topic_label(request),
        learning_profile: request.learning_profile.clone(),
        learning_intents: request.learning_intents.clone(),
        request_hash: learning_request_hash(request),
        provider,
        module_title: module.title.clone(),
        lesson_count: module.lessons.len(),
        validation,
        lesson_reports,
        warnings,
        integration_tags: learning_integration_tags(request),
        source_categories: learning_source_categories_from_ids(&source_ids),
        source_ids,
        code_mode_enabled: wants_code_snippets_for_request(request),
        final_lab_ready: learning_final_lab_ready(request, module),
        denial_tests_count,
        unsupported_claim_warnings,
        compute_model_coverage,
        execution_path,
        task_lifecycle_covered,
        failure_cases_count,
        final_compute_lab_ready,
        generated_at: Utc::now(),
    }
}

fn learning_lesson_eval_report(lesson: &LearningLesson) -> LearningLessonEvalReport {
    let validation = validation_state_from_lesson(lesson);
    let source_urls = unique_learning_source_urls(lesson);
    let source_ids = learning_source_ids_from_urls(&source_urls);
    LearningLessonEvalReport {
        lesson_id: lesson.id.clone(),
        title: lesson.title.clone(),
        validation,
        quality_score: lesson.quality_score.clone(),
        source_titles: unique_learning_source_titles(lesson),
        source_urls,
        source_categories: learning_source_categories_from_ids(&source_ids),
        source_ids,
        warning_count: learning_lesson_eval_warnings(lesson).len(),
    }
}

fn validation_state_from_module(module: &LearningModule) -> LearningModuleValidationState {
    if module.lessons.is_empty() {
        return LearningModuleValidationState::default();
    }

    let lesson_states = module
        .lessons
        .iter()
        .map(validation_state_from_lesson)
        .collect::<Vec<_>>();
    let repetition_check = !module_has_learning_repetition(module);

    LearningModuleValidationState {
        source_grounding: lesson_states.iter().all(|state| state.source_grounding),
        technical_depth: lesson_states.iter().all(|state| state.technical_depth),
        placeholder_free: lesson_states.iter().all(|state| state.placeholder_free),
        repetition_check,
        checkpoint_quality: lesson_states.iter().all(|state| state.checkpoint_quality),
        ecosystem_alignment: lesson_states.iter().all(|state| state.ecosystem_alignment),
        passed: repetition_check && lesson_states.iter().all(|state| state.passed),
    }
}

fn module_has_learning_repetition(module: &LearningModule) -> bool {
    let mut titles = BTreeSet::new();
    let mut checkpoints = BTreeSet::new();

    for lesson in &module.lessons {
        let title = normalized_fingerprint(&lesson.title);
        if !title.is_empty() && !titles.insert(title) {
            return true;
        }

        let checkpoint = normalized_fingerprint(&lesson.checkpoint.question);
        if !checkpoint.is_empty() && !checkpoints.insert(checkpoint) {
            return true;
        }
    }

    false
}

fn learning_eval_warnings(
    module: &LearningModule,
    validation: &LearningModuleValidationState,
    lesson_reports: &[LearningLessonEvalReport],
) -> Vec<String> {
    let mut warnings = Vec::new();

    if !validation.source_grounding {
        warnings.push("one or more lessons are missing source grounding".to_string());
    }
    if !validation.technical_depth {
        warnings.push("one or more lessons do not meet depth requirements".to_string());
    }
    if !validation.placeholder_free {
        warnings.push("placeholder or generic AI text was detected".to_string());
    }
    if !validation.repetition_check {
        warnings.push("repeated lesson titles or checkpoints were detected".to_string());
    }
    if !validation.checkpoint_quality {
        warnings.push("one or more checkpoints are too generic".to_string());
    }
    if !validation.ecosystem_alignment {
        warnings
            .push("one or more lessons appear misaligned with the selected ecosystem".to_string());
    }

    for (index, report) in lesson_reports.iter().enumerate() {
        if report.warning_count > 0 {
            warnings.push(format!(
                "lesson {} '{}' has {} validation warning{}",
                index + 1,
                clamp_text(report.title.clone(), 80),
                report.warning_count,
                if report.warning_count == 1 { "" } else { "s" }
            ));
        }
    }

    if module.lessons.len() < 5 {
        warnings.push("module has fewer than five generated lessons".to_string());
    }

    warnings.extend(ton_stonfi_module_warnings(module));
    warnings.extend(golem_module_warnings(module));

    warnings
}

fn ton_stonfi_module_warnings(module: &LearningModule) -> Vec<String> {
    let combined = format!(
        "{} {} {}",
        module.title,
        module.outcome,
        module
            .lessons
            .iter()
            .map(|lesson| format!(
                "{} {} {} {} {}",
                lesson.title,
                lesson.explanation,
                lesson.checkpoint.question,
                lesson.quest_bridge,
                lesson.concepts.join(" ")
            ))
            .collect::<Vec<_>>()
            .join(" ")
    )
    .to_ascii_lowercase();

    let looks_ton_stonfi = ["ston.fi", "stonfi", "omniston", "ton connect", "jetton"]
        .iter()
        .any(|term| combined.contains(term));
    if !looks_ton_stonfi {
        return Vec::new();
    }

    let checks = [
        (
            "TON Connect wallet boundary is not explicit",
            combined.contains("ton connect")
                && (combined.contains("manifest") || combined.contains("wallet approval")),
        ),
        (
            "jetton verification does not name master or wallet contract evidence",
            combined.contains("jetton")
                && (combined.contains("master") || combined.contains("wallet contract")),
        ),
        (
            "slippage/min-out safety is not explicit",
            combined.contains("slippage")
                && (combined.contains("min-out") || combined.contains("minout")),
        ),
        (
            "quote freshness or stale quote denial is not explicit",
            combined.contains("quote") && combined.contains("stale"),
        ),
        (
            "transaction-state evidence is not explicit",
            combined.contains("transaction")
                && (combined.contains("pending")
                    || combined.contains("confirmed")
                    || combined.contains("final")),
        ),
    ];

    checks
        .into_iter()
        .filter_map(|(warning, passed)| (!passed).then(|| warning.to_string()))
        .collect()
}

fn golem_module_warnings(module: &LearningModule) -> Vec<String> {
    let combined = learning_module_full_text(module).to_ascii_lowercase();
    let looks_golem = [
        "golem",
        "yagna",
        "requestor",
        "provider",
        "ray",
        "golem sdk",
        "decentralized compute",
    ]
    .iter()
    .any(|term| combined.contains(term));
    if !looks_golem {
        return Vec::new();
    }

    let checks = [
        (
            "requestor/provider boundary is not explicit",
            combined.contains("requestor") && combined.contains("provider"),
        ),
        (
            "Yagna or app-key coordination is not explicit",
            combined.contains("yagna") || combined.contains("app key"),
        ),
        (
            "task lifecycle is not explicit",
            combined.contains("task")
                && (combined.contains("result") || combined.contains("output"))
                && (combined.contains("agreement")
                    || combined.contains("allocation")
                    || combined.contains("market")),
        ),
        (
            "provider failure handling is not explicit",
            combined.contains("failure")
                || combined.contains("timeout")
                || combined.contains("retry")
                || combined.contains("provider unavailable"),
        ),
        (
            "cost or payment boundary is not explicit",
            combined.contains("cost")
                || combined.contains("budget")
                || combined.contains("payment")
                || combined.contains("price"),
        ),
    ];

    checks
        .into_iter()
        .filter_map(|(warning, passed)| (!passed).then(|| warning.to_string()))
        .collect()
}

fn golem_compute_model_coverage(
    request: &GenerateLearningModuleRequest,
    module: &LearningModule,
) -> Vec<String> {
    if learning_ecosystem_id(request) != "golem" {
        return Vec::new();
    }
    let combined = learning_module_full_text(module).to_ascii_lowercase();
    let mut coverage = Vec::new();
    for (needle, label) in [
        ("requestor", "requestor"),
        ("provider", "provider"),
        ("yagna", "yagna"),
        ("app key", "app-key"),
        ("agreement", "agreement"),
        ("allocation", "allocation"),
        ("task", "task"),
        ("result", "result-handling"),
        ("output", "result-handling"),
        ("payment", "payment"),
        ("budget", "budget"),
        ("ray", "ray"),
        ("dapp", "dapp"),
        ("gvmi", "gvmi"),
        ("provider failure", "provider-failure"),
        ("timeout", "timeout"),
        ("retry", "retry"),
    ] {
        if combined.contains(needle) && !coverage.iter().any(|item| item == label) {
            coverage.push(label.to_string());
        }
    }
    coverage
}

fn golem_execution_path(
    request: &GenerateLearningModuleRequest,
    module: &LearningModule,
) -> Option<String> {
    if learning_ecosystem_id(request) != "golem" {
        return None;
    }
    let request_hint = format!(
        "{} {} {}",
        learning_focus_label(request),
        request.interests.join(" "),
        request.learner_goal
    )
    .to_ascii_lowercase();
    let combined = learning_module_full_text(module).to_ascii_lowercase();
    let path = if request_hint.contains("javascript")
        || request_hint.contains("js sdk")
        || request_hint.contains("golem js")
    {
        "js-sdk-task-execution"
    } else if request_hint.contains("dapp")
        || request_hint.contains("gvmi")
        || request_hint.contains("descriptor")
    {
        "golem-dapp-deployment"
    } else if request_hint.contains("ray") {
        "ray-on-golem"
    } else if request_hint.contains("python") {
        "python-sdk-task-execution"
    } else if combined.contains("dapp")
        || combined.contains("gvmi")
        || combined.contains("descriptor")
    {
        "golem-dapp-deployment"
    } else if combined.contains("javascript")
        || combined.contains("js sdk")
        || combined.contains("@golem-sdk")
    {
        "js-sdk-task-execution"
    } else if combined.contains("python") {
        "python-sdk-task-execution"
    } else if combined.contains("ray") {
        "ray-on-golem"
    } else {
        "requestor-provider-task-execution"
    };
    Some(path.to_string())
}

fn golem_task_lifecycle_covered(
    request: &GenerateLearningModuleRequest,
    module: &LearningModule,
) -> bool {
    if learning_ecosystem_id(request) != "golem" {
        return false;
    }
    let combined = learning_module_full_text(module).to_ascii_lowercase();
    let lifecycle_terms = [
        "requestor",
        "provider",
        "yagna",
        "agreement",
        "allocation",
        "task",
        "result",
        "payment",
    ];
    count_terms(&combined, &lifecycle_terms) >= 5
}

fn golem_failure_case_count_for_request(
    request: &GenerateLearningModuleRequest,
    module: &LearningModule,
) -> usize {
    if learning_ecosystem_id(request) == "golem" {
        golem_failure_case_count(module)
    } else {
        0
    }
}

fn golem_failure_case_count(module: &LearningModule) -> usize {
    let combined = learning_module_full_text(module).to_ascii_lowercase();
    let failure_cases = [
        ["provider unavailable", "no provider", "provider offline"].as_slice(),
        ["provider timeout", "task timeout", "timeout"].as_slice(),
        ["failed task", "task failed", "execution failure"].as_slice(),
        ["missing result", "empty result", "no output"].as_slice(),
        ["corrupted result", "wrong result", "invalid result"].as_slice(),
        ["wrong image", "wrong gvmi", "image mismatch"].as_slice(),
        ["wrong runtime", "unsupported version", "version mismatch"].as_slice(),
        ["budget exceeded", "price too high", "payment failure"].as_slice(),
        [
            "agreement rejected",
            "agreement mismatch",
            "market mismatch",
        ]
        .as_slice(),
        ["yagna disconnected", "lost yagna", "yagna failure"].as_slice(),
        ["network failure", "network partition", "connection failure"].as_slice(),
        ["ray limitation", "ray unsupported", "ray supported version"].as_slice(),
        [
            "provider result automatically trusted",
            "unverified provider output",
        ]
        .as_slice(),
    ];
    failure_cases
        .iter()
        .filter(|terms| terms.iter().any(|term| combined.contains(*term)))
        .count()
}

fn golem_final_compute_lab_ready(
    request: &GenerateLearningModuleRequest,
    module: &LearningModule,
) -> bool {
    if learning_ecosystem_id(request) != "golem" {
        return false;
    }
    let Some(final_lesson) = module.lessons.last() else {
        return false;
    };
    let final_title = final_lesson.title.to_ascii_lowercase();
    let final_lesson_marker = module.lessons.len() >= 5
        || final_lesson.id.starts_with("module-5-")
        || final_title.contains("final");
    if !final_lesson_marker {
        return false;
    }
    let text = format!(
        "{} {} {} {}",
        final_lesson.title,
        final_lesson.explanation,
        final_lesson.quest_bridge,
        final_lesson.checkpoint.question
    )
    .to_ascii_lowercase();

    text.contains("final")
        && text.contains("golem")
        && text.contains("requestor")
        && text.contains("provider")
        && text.contains("task")
        && (text.contains("result") || text.contains("output"))
        && golem_failure_case_count(module) >= 6
}

fn learning_lesson_eval_warnings(lesson: &LearningLesson) -> Vec<String> {
    let validation = validation_state_from_lesson(lesson);
    let mut warnings = Vec::new();

    if !validation.source_grounding {
        warnings.push("missing source grounding".to_string());
    }
    if !validation.technical_depth {
        warnings.push("insufficient technical depth".to_string());
    }
    if !validation.placeholder_free {
        warnings.push("placeholder text detected".to_string());
    }
    if !validation.checkpoint_quality {
        warnings.push("generic checkpoint".to_string());
    }
    if !validation.ecosystem_alignment {
        warnings.push("ecosystem alignment failed".to_string());
    }

    warnings
}

fn unique_learning_source_titles(lesson: &LearningLesson) -> Vec<String> {
    let mut seen = BTreeSet::new();
    lesson
        .evidence_map
        .iter()
        .map(|source| source.source_title.as_str())
        .chain(lesson.resources.iter().map(|source| source.title.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter_map(|value| {
            let normalized = value.to_ascii_lowercase();
            if seen.insert(normalized) {
                Some(clamp_text(value.to_string(), 120))
            } else {
                None
            }
        })
        .collect()
}

fn unique_learning_source_urls(lesson: &LearningLesson) -> Vec<String> {
    let mut seen = BTreeSet::new();
    lesson
        .evidence_map
        .iter()
        .map(|source| source.source_url.as_str())
        .chain(lesson.resources.iter().map(|source| source.url.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter_map(|value| {
            let normalized = value.to_ascii_lowercase();
            if seen.insert(normalized) {
                Some(clamp_text(value.to_string(), 180))
            } else {
                None
            }
        })
        .collect()
}

fn learning_request_hash(request: &GenerateLearningModuleRequest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(learning_ecosystem_id(request).as_bytes());
    hasher.update(b"\0");
    hasher.update(
        request
            .topic
            .as_deref()
            .unwrap_or_default()
            .trim()
            .as_bytes(),
    );
    hasher.update(b"\0");
    hasher.update(
        request
            .learning_profile
            .as_deref()
            .unwrap_or_default()
            .trim()
            .as_bytes(),
    );
    hasher.update(b"\0");
    for intent in &request.learning_intents {
        hasher.update(intent.trim().as_bytes());
        hasher.update(b"|");
    }
    hasher.update(b"\0");
    hasher.update(request.background.trim().as_bytes());
    hasher.update(b"\0");
    hasher.update(request.pace.trim().as_bytes());
    hasher.update(b"\0");
    hasher.update(request.learner_goal.trim().as_bytes());
    let digest = hasher.finalize();
    format!("{digest:x}")
}

fn learning_provider_kind(base_url: &str) -> String {
    let lower = base_url.to_ascii_lowercase();
    if lower.contains("localhost") || lower.contains("127.0.0.1") || lower.contains("0.0.0.0") {
        "local-openai-compatible".to_string()
    } else if lower.contains("api.openai.com") {
        "openai".to_string()
    } else {
        "openai-compatible".to_string()
    }
}

fn sanitized_endpoint_origin(base_url: &str) -> String {
    let Ok(url) = Url::parse(base_url) else {
        return "configured-openai-compatible-endpoint".to_string();
    };
    let Some(host) = url.host_str() else {
        return "configured-openai-compatible-endpoint".to_string();
    };

    let mut origin = format!("{}://{}", url.scheme(), host);
    if let Some(port) = url.port() {
        origin.push_str(&format!(":{port}"));
    }
    origin
}

impl From<LearningSessionDocument> for LearningSessionRecord {
    fn from(session: LearningSessionDocument) -> Self {
        let user_id = if session.user_id.trim().is_empty() {
            session.user_address
        } else {
            session.user_id
        };
        let module_statuses =
            compact_module_generation_statuses(session.module_statuses, &session.module, 5);
        let eval_artifacts =
            compact_learning_eval_artifacts(session.eval_artifacts, &session.module, 5);
        Self {
            module_id: session.id,
            user_id,
            status: normalized_learning_session_status(&session.status),
            provider: session.provider,
            email: session.email,
            name: session.name,
            source: session.source,
            module: session.module,
            module_statuses,
            eval_artifacts,
            ecosystem_id: session.ecosystem_id,
            topic: session.topic,
            learning_profile: session.learning_profile,
            learning_intents: session.learning_intents,
            selected_interests: session.selected_interests,
            learner_goal: session.learner_goal,
            background: session.background,
            pace: session.pace,
            active_lesson_index: session.active_lesson_index.max(0) as usize,
            checkpoint_answers: document_to_checkpoint_answers(session.checkpoint_answers),
            tutor_messages: session.tutor_messages,
            created_at: bson_datetime_to_utc(session.created_at),
            updated_at: bson_datetime_to_utc(session.updated_at),
        }
    }
}

impl From<RewardClaimDocument> for RewardClaimRecord {
    fn from(claim: RewardClaimDocument) -> Self {
        Self {
            claim_id: claim.claim_id,
            run_id: claim.run_id,
            user_address: claim.user_address,
            amount_shannons: claim.amount_shannons,
            currency: claim.currency,
            status: claim.status,
            fiber_payment: claim.fiber_payment,
            error: claim.error,
            created_at: bson_datetime_to_utc(claim.created_at),
            updated_at: bson_datetime_to_utc(claim.updated_at),
            paid_at: claim.paid_at.map(bson_datetime_to_utc),
        }
    }
}

fn initial_quest_progress(infrastructure_ready: bool) -> QuestProgress {
    QuestProgress {
        gates: vec![
            StoredGateProgress {
                id: "identity".to_string(),
                name: "Wallet Proof".to_string(),
                description: "A signed JoyID passkey proof is bound to this quest session."
                    .to_string(),
                is_completed: true,
            },
            StoredGateProgress {
                id: "infrastructure".to_string(),
                name: "Backend Readiness".to_string(),
                description:
                    "vibequest-core reports OpenAI, CKB RPC, Fiber RPC, and MongoDB ready."
                        .to_string(),
                is_completed: infrastructure_ready,
            },
            StoredGateProgress {
                id: "verification".to_string(),
                name: "Generated Workspace Checks".to_string(),
                description: "Generated files pass proof, test, and denial-path checks."
                    .to_string(),
                is_completed: false,
            },
        ],
        boss_fight_solved: false,
        shipped: false,
    }
}

fn validate_reward_invoice(invoice: &str) -> Result<(), ApiError> {
    let trimmed = invoice.trim();
    if trimmed.is_empty() {
        return Err(ApiError::MissingFiberInvoice);
    }

    let has_invoice_shape = trimmed.len() >= 12
        && !trimmed.chars().any(char::is_whitespace)
        && trimmed
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ":_-./=".contains(character));

    if !has_invoice_shape {
        return Err(ApiError::InvalidFiberInvoice);
    }

    Ok(())
}

fn server_completion_proof(run: &QuestRunDocument) -> Result<ServerCompletionProof, ApiError> {
    let identity_gate = run
        .progress
        .gates
        .iter()
        .any(|gate| gate.id == "identity" && gate.is_completed);
    let infrastructure_gate = run
        .progress
        .gates
        .iter()
        .any(|gate| gate.id == "infrastructure" && gate.is_completed);
    let verification_gate = run
        .progress
        .gates
        .iter()
        .any(|gate| gate.id == "verification" && gate.is_completed);
    let workspace = run
        .quest
        .workbench_files
        .iter()
        .map(|file| {
            format!(
                "{}
{}",
                file.path, file.content
            )
            .to_lowercase()
        })
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    let tests_present = workspace.contains("test(") || workspace.contains("#[test]");
    let proof_present = workspace.contains("fiber")
        && workspace.contains("ckb")
        && (workspace.contains("proof") || workspace.contains("receipt"));
    let denial_path_present = ["reject", "block", "false", "unpaid"]
        .iter()
        .any(|needle| workspace.contains(needle));
    let generated_files_verified = tests_present && proof_present && denial_path_present;

    if !(identity_gate
        && infrastructure_gate
        && verification_gate
        && run.progress.boss_fight_solved
        && generated_files_verified)
    {
        return Err(ApiError::CompletionNotVerified);
    }

    Ok(ServerCompletionProof {
        identity_gate,
        infrastructure_gate,
        verification_gate,
        boss_fight_solved: run.progress.boss_fight_solved,
        generated_files_verified,
        tests_present,
        proof_present,
        denial_path_present,
        completed_at: BsonDateTime::now(),
    })
}

fn status_for_progress(progress: &QuestProgress) -> QuestRunStatus {
    if progress.shipped
        && progress.boss_fight_solved
        && progress.gates.iter().all(|gate| gate.is_completed)
    {
        QuestRunStatus::Completed
    } else {
        QuestRunStatus::InProgress
    }
}

fn wallet_document(wallet: &WalletBinding) -> Document {
    doc! {
        "address": &wallet.address,
        "identity": &wallet.identity,
        "sign_type": &wallet.sign_type,
        "message": &wallet.message,
    }
}

fn bson_datetime_to_utc(value: BsonDateTime) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(value.timestamp_millis()).unwrap_or_else(Utc::now)
}

fn difficulty_label(difficulty: Option<&Difficulty>) -> &'static str {
    match difficulty {
        Some(Difficulty::Novice) => "novice",
        Some(Difficulty::Boss) => "boss",
        _ => "builder",
    }
}

pub fn app_state() -> Arc<AppState> {
    dotenvy::dotenv().ok();

    let config = AppConfig::from_env();
    let store = MongoStore::from_config(&config);
    let registry =
        EcosystemRegistry::built_in().expect("the built-in ecosystem registry must be valid");
    let platform_store = PlatformStore::new(
        config.mongodb_uri.clone(),
        config.mongodb_v3_database.clone(),
    );
    let auth = AuthVerifier::from_env().unwrap_or_else(|error| {
        warn!(error = %error, "Core assertion verification is disabled");
        AuthVerifier::disabled()
    });
    let state = Arc::new(AppState {
        auth,
        runner: runner::RunnerService::from_environment(),
        registry,
        platform_store,
        openai: OpenAiClient::from_env(),
        fiber: FiberPayoutClient::from_config(&config),
        store,
        config,
    });

    warn_missing_integrations(&state);

    state
}

pub async fn initialize_platform(state: &Arc<AppState>) -> Result<(), String> {
    match tokio::time::timeout(
        Duration::from_secs(5),
        state.platform_store.ensure_indexes(),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(format!("failed to initialize v3 indexes: {error}")),
        Err(_) => Err("v3 index initialization timed out after 5 seconds".to_string()),
    }
}

pub fn app_port() -> u16 {
    AppConfig::from_env().port
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route("/ai/learning/module", post(generate_learning_module))
        .route(
            "/ai/learning/lesson",
            post(generate_learning_lesson_endpoint),
        )
        .route("/ai/learning/tutor", post(answer_learning_question))
        .route(
            "/ai/learning/tutor/save",
            post(api_save_learning_tutor_exchange),
        )
        .route(
            "/ai/learning/session",
            get(api_get_learning_session).post(api_save_learning_session),
        )
        .route("/ai/learning/sessions", get(api_list_learning_sessions))
        .route(
            "/ai/learning/events",
            get(api_learning_metrics).post(api_save_learning_event),
        )
        .route("/ai/learning/admin/review", get(api_learning_admin_review))
        .route(
            "/ai/learning/sessions/{module_id}/export",
            get(api_export_learning_session),
        )
        .route(
            "/ai/learning/sessions/{module_id}",
            delete(api_delete_learning_session),
        )
        .route(
            "/ai/learning/sessions/{module_id}/archive",
            patch(api_archive_learning_session),
        )
        .route("/ai/learning/quest", post(generate_learning_quest))
        .route("/v3/me", get(v3_me).delete(v3_delete_account))
        .route("/v3/me/export", get(v3_export_account))
        .route("/v3/submissions", post(v3_create_submission))
        .route(
            "/v3/submissions/{submission_id}",
            get(v3_get_submission).delete(v3_cancel_submission),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_authentication,
        ));

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/v3/catalog", get(v3_catalog))
        .route(
            "/v3/catalog/{ecosystem_id}/tracks/{track_id}",
            get(v3_track),
        )
        .route(
            "/v3/catalog/{ecosystem_id}/tracks/{track_id}/curriculum",
            get(v3_curriculum),
        )
        .merge(protected)
        .layer(cors_layer(&state.config))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn cors_layer(config: &AppConfig) -> CorsLayer {
    let layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    if config.cors_origins.iter().any(|origin| origin == "*") {
        return layer.allow_origin(AllowOrigin::any());
    }

    let origins = config
        .cors_origins
        .iter()
        .filter_map(|origin| origin.parse::<HeaderValue>().ok())
        .collect::<Vec<_>>();

    layer.allow_origin(origins)
}

impl AppConfig {
    fn from_env() -> Self {
        Self {
            port: env::var("PORT")
                .ok()
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or(8080),
            app_env: env::var("APP_ENV").unwrap_or_else(|_| "development".to_string()),
            cors_origins: parse_csv_env("CORS_ORIGINS", vec!["http://localhost:3000".to_string()]),
            ckb_rpc_url: optional_env("CKB_RPC_URL"),
            fiber_rpc_url: optional_env("FIBER_RPC_URL"),
            fiber_payout_rpc_url: optional_env("FIBER_PAYOUT_RPC_URL"),
            fiber_payout_enabled: parse_bool_env("FIBER_PAYOUT_ENABLED", false),
            reward_amount_shannons: optional_env("VIBEQUEST_REWARD_SHANNONS")
                .and_then(|value| value.parse::<u128>().ok())
                .unwrap_or(400),
            reward_currency: optional_env("VIBEQUEST_REWARD_CURRENCY")
                .unwrap_or_else(|| "Fibd".to_string()),
            mongodb_uri: optional_env("MONGODB_URI"),
            mongodb_database: optional_env("MONGODB_DATABASE")
                .unwrap_or_else(|| "vibequest".to_string()),
            mongodb_v3_database: optional_env("MONGODB_DATABASE_V3")
                .unwrap_or_else(|| platform::DEFAULT_V3_DATABASE.to_string()),
        }
    }
}

impl OpenAiClient {
    fn from_env() -> Self {
        let timeout_seconds = env::var("OPENAI_TIMEOUT_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_OPENAI_TIMEOUT_SECONDS)
            .clamp(15, MAX_OPENAI_TIMEOUT_SECONDS);

        Self {
            http: Client::builder()
                .user_agent("VibeQuestCore/1.0 (+https://github.com/buidlLabs3/vibequest-core)")
                .build()
                .expect("OpenAI HTTP client should build"),
            api_key: optional_env("OPENAI_API_KEY"),
            model: optional_env("OPENAI_MODEL")
                .or_else(|| optional_env("MODEL"))
                .unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_string()),
            base_url: env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_OPENAI_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            reasoning_effort: optional_env("OPENAI_REASONING_EFFORT")
                .or_else(|| optional_env("MODEL_REASONING_EFFORT"))
                .and_then(|value| ReasoningEffort::parse(&value))
                .unwrap_or(DEFAULT_OPENAI_REASONING_EFFORT),
            disable_response_storage: parse_bool_env("OPENAI_DISABLE_RESPONSE_STORAGE", true),
            timeout: Duration::from_secs(timeout_seconds),
        }
    }

    fn provider_metadata(&self) -> AiProviderMetadata {
        AiProviderMetadata {
            provider_kind: learning_provider_kind(&self.base_url),
            model: clamp_text(self.model.clone(), 96),
            endpoint_origin: sanitized_endpoint_origin(&self.base_url),
            reasoning_effort: self.reasoning_effort,
            response_storage_disabled: self.disable_response_storage,
            timeout_seconds: self.timeout.as_secs(),
            configured: self.api_key.is_some(),
        }
    }

    async fn generate_quest(
        &self,
        request: &GenerateQuestRequest,
    ) -> Result<QuestBlueprint, ApiError> {
        let difficulty = request.difficulty.clone().unwrap_or(Difficulty::Builder);
        let track = request
            .skill_track
            .as_deref()
            .unwrap_or("CKB + Fiber Builder");
        let prompt = quest_prompt(
            request.build_prompt.trim(),
            track,
            &difficulty,
            request.learning_context.as_ref(),
            false,
        );
        let quest = self
            .post_openai_json::<QuestBlueprint>(
                prompt,
                QUICK_QUEST_OUTPUT_TOKENS,
                ReasoningEffort::None,
                self.timeout,
            )
            .await?;

        match compact_quest_blueprint(quest, request.learning_context.as_ref()) {
            Ok(quest) => Ok(quest),
            Err(ApiError::InvalidAiResponse) => {
                warn!("AI-authored quest failed validation; retrying once with stricter schema");
                let repair_prompt = quest_prompt(
                    request.build_prompt.trim(),
                    track,
                    &difficulty,
                    request.learning_context.as_ref(),
                    true,
                );
                let repaired_quest = self
                    .post_openai_json::<QuestBlueprint>(
                        repair_prompt,
                        QUICK_QUEST_OUTPUT_TOKENS,
                        ReasoningEffort::None,
                        self.timeout,
                    )
                    .await?;
                compact_quest_blueprint(repaired_quest, request.learning_context.as_ref())
            }
            Err(error) => Err(error),
        }
    }

    async fn generate_learning_module(
        &self,
        request: &GenerateLearningModuleRequest,
    ) -> Result<LearningModule, ApiError> {
        let mut lessons = Vec::with_capacity(5);
        let mut prior_lessons = Vec::with_capacity(5);
        for lesson_index in 0..5 {
            let lesson = self
                .generate_learning_lesson(request, lesson_index, &prior_lessons)
                .await?;
            prior_lessons.push(prior_learning_lesson_from_compact(&lesson));
            lessons.push(lesson);
        }
        let compact = AiLearningModuleCompact {
            t: learning_module_title(request),
            l: lessons,
        };

        build_learning_module_from_compact_ai(request, compact)
    }

    async fn generate_learning_lesson_item(
        &self,
        request: &GenerateLearningModuleRequest,
        lesson_index: usize,
        prior_lessons: &[PriorLearningLesson],
    ) -> Result<LearningLesson, ApiError> {
        let compact = self
            .generate_learning_lesson(request, lesson_index, prior_lessons)
            .await?;
        compact_ai_lesson_to_learning_lesson(
            lesson_index,
            &learning_background_label(request),
            &learning_focus_label(request),
            request,
            compact,
        )
    }

    async fn generate_learning_lesson(
        &self,
        request: &GenerateLearningModuleRequest,
        lesson_index: usize,
        prior_lessons: &[PriorLearningLesson],
    ) -> Result<AiLearningLessonCompact, ApiError> {
        match self
            .request_learning_lesson(request, lesson_index, false, prior_lessons)
            .await
        {
            Ok(lesson) => Ok(lesson),
            Err(error) => {
                match &error {
                    ApiError::OpenAiTransport(detail) => {
                        warn!(%detail, lesson_index, "AI lesson transport failed");
                    }
                    ApiError::OpenAiStatus { status, body } => {
                        warn!(%status, body = %clamp_text(body.clone(), 300), lesson_index, "AI lesson provider returned error status");
                    }
                    ApiError::InvalidAiResponse => {
                        warn!(
                            lesson_index,
                            "AI lesson response failed validation; retrying once with repair contract"
                        );
                        return self
                            .request_learning_lesson(request, lesson_index, true, prior_lessons)
                            .await;
                    }
                    _ => warn!(%error, lesson_index, "AI lesson generation failed"),
                }
                Err(error)
            }
        }
    }

    async fn request_learning_lesson(
        &self,
        request: &GenerateLearningModuleRequest,
        lesson_index: usize,
        repair: bool,
        prior_lessons: &[PriorLearningLesson],
    ) -> Result<AiLearningLessonCompact, ApiError> {
        let prompt = learning_lesson_prompt(request, lesson_index, repair, prior_lessons);
        let lesson_timeout = self.timeout;
        let mut lesson = self
            .post_openai_json::<AiLearningLessonCompact>(
                prompt,
                LEARNING_LESSON_OUTPUT_TOKENS,
                self.reasoning_effort.serverless_safe(),
                lesson_timeout,
            )
            .await?;
        normalize_ai_learning_lesson_for_request(request, lesson_index, &mut lesson);
        if let Err(error) = validate_ai_learning_lesson_compact_for_request_with_context(
            request,
            &lesson,
            lesson_index,
            prior_lessons,
        ) {
            let validation_failures = ai_learning_lesson_validation_failures(
                request,
                &lesson,
                lesson_index,
                prior_lessons,
            )
            .join(", ");
            warn!(
                lesson_index,
                title = %clamp_text(lesson.t.clone(), 120),
                validation_failures = %clamp_text(validation_failures, 320),
                explainer_words = lesson.e.split_whitespace().count(),
                why_words = lesson.w.split_whitespace().count(),
                bridge_words = lesson.j.split_whitespace().count(),
                wrong_answers = lesson.b.len(),
                wrong_feedback = lesson.bf.len(),
                question = %clamp_text(lesson.q.clone(), 180),
                "AI lesson failed quality gate"
            );
            return Err(error);
        }
        Ok(lesson)
    }

    async fn post_openai_json<T>(
        &self,
        prompt: String,
        max_output_tokens: u16,
        reasoning_effort: ReasoningEffort,
        timeout: Duration,
    ) -> Result<T, ApiError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let Some(api_key) = self.api_key.as_ref() else {
            return Err(ApiError::MissingOpenAiKey);
        };

        let body = serde_json::json!({
            "model": self.model,
            "input": prompt,
            "reasoning": {
                "effort": reasoning_effort
            },
            "max_output_tokens": max_output_tokens,
            "store": !self.disable_response_storage,
            "text": {
                "format": {
                    "type": "json_object"
                }
            }
        });

        let response = self
            .http
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(api_key)
            .timeout(timeout)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                let detail = if error.is_timeout() {
                    format!(
                        "{error}; source: {}",
                        error
                            .source()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "request timed out".to_string())
                    )
                } else {
                    error.to_string()
                };

                ApiError::OpenAiTransport(detail)
            })?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|error| ApiError::OpenAiTransport(error.to_string()))?;

        if !status.is_success() {
            return Err(ApiError::OpenAiStatus {
                status,
                body: truncate_error_body(&response_body),
            });
        }

        match parse_openai_json_response::<T>(&response_body) {
            Ok(value) => Ok(value),
            Err(error) => {
                warn!(
                    body = %truncate_error_body(&response_body),
                    "OpenAI response did not match expected JSON schema"
                );
                Err(error)
            }
        }
    }

    async fn answer_learning_question(
        &self,
        request: &LearningTutorRequest,
    ) -> Result<LearningTutorResponse, ApiError> {
        let Some(api_key) = self.api_key.as_ref() else {
            return Err(ApiError::MissingOpenAiKey);
        };

        let prompt = learning_tutor_prompt(request);
        let body = serde_json::json!({
            "model": self.model,
            "input": prompt,
            "reasoning": {
                "effort": self.reasoning_effort.serverless_safe()
            },
            "max_output_tokens": TUTOR_OUTPUT_TOKENS,
            "store": !self.disable_response_storage,
            "text": {
                "format": {
                    "type": "json_object"
                }
            }
        });

        let response = self
            .http
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(api_key)
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|error| ApiError::OpenAiTransport(error.to_string()))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|error| ApiError::OpenAiTransport(error.to_string()))?;

        if !status.is_success() {
            return Err(ApiError::OpenAiStatus {
                status,
                body: truncate_error_body(&response_body),
            });
        }

        let answer = parse_openai_json_response::<LearningTutorAiResponse>(&response_body)?;
        Ok(LearningTutorResponse {
            source: QuestSource::OpenAi,
            answer: clamp_text(answer.answer, 1200),
            why_it_matters: clamp_text(answer.why_it_matters, 650),
            follow_up_question: clamp_text(answer.follow_up_question, 220),
            references: compact_learning_resources(answer.references),
        })
    }

    async fn answer_code_question(
        &self,
        request: &CodeTutorRequest,
    ) -> Result<CodeTutorResponse, ApiError> {
        let Some(api_key) = self.api_key.as_ref() else {
            return Err(ApiError::MissingOpenAiKey);
        };

        let prompt = code_tutor_prompt(request);
        let body = serde_json::json!({
            "model": self.model,
            "input": prompt,
            "reasoning": {
                "effort": self.reasoning_effort.serverless_safe()
            },
            "max_output_tokens": TUTOR_OUTPUT_TOKENS,
            "store": !self.disable_response_storage,
            "text": {
                "format": {
                    "type": "json_object"
                }
            }
        });

        let response = self
            .http
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(api_key)
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|error| ApiError::OpenAiTransport(error.to_string()))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .map_err(|error| ApiError::OpenAiTransport(error.to_string()))?;

        if !status.is_success() {
            return Err(ApiError::OpenAiStatus {
                status,
                body: truncate_error_body(&response_body),
            });
        }

        let answer = parse_openai_json_response::<CodeTutorAiResponse>(&response_body)?;
        Ok(CodeTutorResponse {
            source: QuestSource::OpenAi,
            answer: clamp_text(answer.answer, 900),
            code_walkthrough: compact_string_list(answer.code_walkthrough, 5, 220),
            common_misunderstanding: clamp_text(answer.common_misunderstanding, 360),
            follow_up_question: clamp_text(answer.follow_up_question, 260),
            references: compact_learning_resources(answer.references),
            persistence: PersistenceStatus {
                saved: false,
                warning: None,
            },
        })
    }
}

impl FiberPayoutClient {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            http: Client::new(),
            rpc_url: config.fiber_payout_rpc_url.clone(),
            enabled: config.fiber_payout_enabled,
            timeout: Duration::from_secs(30),
        }
    }

    fn is_ready(&self) -> bool {
        self.enabled && self.rpc_url.is_some()
    }

    async fn pay_invoice(&self, invoice: &str) -> Result<Option<FiberPaymentReceipt>, ApiError> {
        if !self.enabled {
            return Ok(Some(FiberPaymentReceipt {
                payment_hash: None,
                status: Some("verified-no-payout".to_string()),
                fee: None,
                raw: serde_json::json!({
                    "mode": "payout-disabled",
                    "invoice_bound": !invoice.trim().is_empty()
                }),
            }));
        }

        let rpc_url = self
            .rpc_url
            .as_ref()
            .ok_or(ApiError::FiberPayoutUnavailable)?;
        let body = serde_json::json!({
            "id": "vibequest-payout",
            "jsonrpc": "2.0",
            "method": "send_payment",
            "params": [{
                "invoice": invoice.trim(),
            }]
        });
        let response = self
            .http
            .post(rpc_url)
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|error| ApiError::FiberPayout(error.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| ApiError::FiberPayout(error.to_string()))?;
        if !status.is_success() {
            return Err(ApiError::FiberPayout(truncate_error_body(&text)));
        }

        let decoded = serde_json::from_str::<FiberRpcResponse>(&text)
            .map_err(|_| ApiError::FiberPayout("invalid Fiber RPC response".to_string()))?;
        if let Some(error) = decoded.error {
            return Err(ApiError::FiberPayout(format!(
                "{}{}",
                error
                    .code
                    .map(|code| format!("{code}: "))
                    .unwrap_or_default(),
                error.message
            )));
        }
        let result = decoded
            .result
            .ok_or_else(|| ApiError::FiberPayout("missing Fiber RPC result".to_string()))?;

        Ok(Some(FiberPaymentReceipt {
            payment_hash: result
                .get("payment_hash")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            status: result
                .get("status")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            fee: result.get("fee").map(|value| match value {
                Value::String(value) => value.clone(),
                other => other.to_string(),
            }),
            raw: result,
        }))
    }
}

impl ReasoningEffort {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            _ => None,
        }
    }

    fn serverless_safe(self) -> Self {
        match self {
            Self::High | Self::Xhigh | Self::Medium | Self::Low => Self::Minimal,
            value => value,
        }
    }
}

#[derive(Debug, Serialize)]
struct AuthBoundaryErrorResponse {
    code: &'static str,
    error: &'static str,
}

#[derive(Debug, Serialize)]
struct IdentityResponse {
    user_id: String,
    provider: String,
    email: Option<String>,
    name: Option<String>,
    persistence_enabled: bool,
}

#[derive(Debug, Serialize)]
struct RunnerErrorResponse {
    code: &'static str,
    error: &'static str,
}

async fn require_authentication(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    if !state.auth.configured() {
        return auth_boundary_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "identity-unavailable",
            "Identity verification is not configured.",
        );
    }

    let assertion = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty() && !value.bytes().any(|byte| byte.is_ascii_whitespace()));
    let Some(assertion) = assertion else {
        return auth_boundary_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Authentication is required.",
        );
    };
    let principal = match state.auth.verify_now(assertion) {
        Ok(principal) => principal,
        Err(_) => {
            return auth_boundary_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Authentication is required.",
            );
        }
    };

    request.extensions_mut().insert(principal);
    next.run(request).await
}

fn auth_boundary_error(status: StatusCode, code: &'static str, error: &'static str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(AuthBoundaryErrorResponse { code, error }),
    )
        .into_response()
}

async fn v3_me(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
) -> Response {
    match state
        .platform_store
        .upsert_identity(
            &principal.user_id,
            &principal.provider,
            &principal.provider_subject,
            principal.email.as_deref(),
            principal.name.as_deref(),
        )
        .await
    {
        Ok(persistence_enabled) => Json(IdentityResponse {
            user_id: principal.user_id,
            provider: principal.provider,
            email: principal.email,
            name: principal.name,
            persistence_enabled,
        })
        .into_response(),
        Err(_) => auth_boundary_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "identity-store-unavailable",
            "The identity store is unavailable.",
        ),
    }
}

async fn v3_export_account(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AccountExport>, Response> {
    state
        .platform_store
        .export_account(&principal.user_id)
        .await
        .map(Json)
        .map_err(|_| {
            auth_boundary_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "identity-store-unavailable",
                "The identity store is unavailable.",
            )
        })
}

async fn v3_delete_account(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<AccountDeletion>, Response> {
    state
        .platform_store
        .delete_account(&principal.user_id)
        .await
        .map(Json)
        .map_err(|_| {
            auth_boundary_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "identity-store-unavailable",
                "The identity store is unavailable.",
            )
        })
}

async fn v3_create_submission(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<runner::CreateSubmissionRequest>,
) -> Result<(StatusCode, Json<runner::RunnerSubmissionView>), Response> {
    state
        .runner
        .submit(&principal.user_id, request)
        .await
        .map(|submission| (StatusCode::ACCEPTED, Json(submission)))
        .map_err(runner_error_response)
}

async fn v3_get_submission(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Path(submission_id): Path<String>,
) -> Result<Json<runner::RunnerSubmissionView>, Response> {
    state
        .runner
        .get(&principal.user_id, &submission_id)
        .await
        .map(Json)
        .map_err(runner_error_response)
}

async fn v3_cancel_submission(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Path(submission_id): Path<String>,
) -> Result<Json<runner::RunnerSubmissionView>, Response> {
    state
        .runner
        .cancel(&principal.user_id, &submission_id)
        .await
        .map(Json)
        .map_err(runner_error_response)
}

fn runner_error_response(error: runner::RunnerServiceError) -> Response {
    let (status, code, message) = match error {
        runner::RunnerServiceError::Disabled => (
            StatusCode::SERVICE_UNAVAILABLE,
            "runner-review-required",
            "The isolated runner is unavailable pending production review.",
        ),
        runner::RunnerServiceError::InvalidRequest => (
            StatusCode::BAD_REQUEST,
            "invalid-submission",
            "The submission does not match the reviewed scenario contract.",
        ),
        runner::RunnerServiceError::QueueFull => (
            StatusCode::TOO_MANY_REQUESTS,
            "runner-queue-full",
            "The isolated runner queue is full.",
        ),
        runner::RunnerServiceError::NotFound => (
            StatusCode::NOT_FOUND,
            "submission-not-found",
            "The submission was not found.",
        ),
    };
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(RunnerErrorResponse {
            code,
            error: message,
        }),
    )
        .into_response()
}

#[derive(Debug, Serialize)]
struct RegistryErrorResponse {
    code: &'static str,
    error: String,
}

async fn v3_catalog(State(state): State<Arc<AppState>>) -> Json<CatalogResponse> {
    Json(state.registry.catalog())
}

async fn v3_track(
    Path((ecosystem_id, track_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<TrackRegistration>, (StatusCode, Json<RegistryErrorResponse>)> {
    state
        .registry
        .resolve_track(&ecosystem_id, &track_id)
        .map(Json)
        .map_err(registry_error_response)
}

async fn v3_curriculum(
    Path((ecosystem_id, track_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<curriculum::PublicCurriculum>, (StatusCode, Json<RegistryErrorResponse>)> {
    let track = state
        .registry
        .registered_track(&ecosystem_id, &track_id)
        .map_err(registry_error_response)?;
    let curriculum = curriculum::public_curriculum().map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(RegistryErrorResponse {
                code: "invalid-curriculum",
                error: error.to_string(),
            }),
        )
    })?;

    if curriculum.track_version != track.track_version
        || curriculum.content_version != track.content_version
        || curriculum.source_manifest_version != track.source_manifest_version
    {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(RegistryErrorResponse {
                code: "invalid-curriculum",
                error: "Curriculum and catalog versions do not match.".to_string(),
            }),
        ));
    }

    Ok(Json(curriculum))
}

fn registry_error_response(error: RegistryError) -> (StatusCode, Json<RegistryErrorResponse>) {
    let (status, code) = match error {
        RegistryError::UnknownEcosystem(_) | RegistryError::UnknownTrack { .. } => {
            (StatusCode::NOT_FOUND, "catalog-entry-not-found")
        }
        RegistryError::EcosystemDisabled(_) | RegistryError::TrackDisabled { .. } => {
            (StatusCode::CONFLICT, "catalog-entry-disabled")
        }
        RegistryError::Empty
        | RegistryError::EmptyEcosystemId
        | RegistryError::DuplicateEcosystem(_)
        | RegistryError::EmptyTrackId(_)
        | RegistryError::DuplicateTrack { .. } => {
            (StatusCode::INTERNAL_SERVER_ERROR, "invalid-catalog")
        }
    };

    (
        status,
        Json(RegistryErrorResponse {
            code,
            error: error.to_string(),
        }),
    )
}
async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let mongodb_diagnostic = state.store.availability_diagnostic().await;
    let integrations = IntegrationStatus {
        openai: state.openai.api_key.is_some(),
        ckb_rpc: state.config.ckb_rpc_url.is_some(),
        fiber_rpc: state.config.fiber_rpc_url.is_some(),
        fiber_payout: state.fiber.is_ready(),
        mongodb: mongodb_diagnostic.is_ok(),
    };
    let missing = missing_integrations(&state, &integrations);

    Json(HealthResponse {
        service: "vibequest-core",
        status: "ok",
        environment: state.config.app_env.clone(),
        ai_layer: AiLayer::OpenAi,
        integrations,
        missing,
        diagnostics: HealthDiagnostics {
            mongodb: mongodb_diagnostic.err(),
        },
        timestamp: Utc::now(),
    })
}

async fn ready(State(state): State<Arc<AppState>>) -> (StatusCode, Json<ReadyResponse>) {
    let integrations = integration_status(&state).await;
    let missing = missing_integrations(&state, &integrations);

    let is_ready = missing.is_empty();
    let status = if is_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(ReadyResponse {
            ready: is_ready,
            missing,
            timestamp: Utc::now(),
        }),
    )
}

async fn season() -> Json<SeasonResponse> {
    Json(SeasonResponse {
        season: "Season 0: Escape Black Box Mode".to_string(),
        thesis: "Vibecode a real app, then unlock shipping by explaining, debugging, testing, and remixing the code.".to_string(),
        tracks: vec![
            Track {
                name: "CKB Fundamentals".to_string(),
                description: "Learn the Cell model, transactions, xUDT assets, and proof receipts through generated app missions.".to_string(),
                sample_quests: vec![
                    "Cell Lab Escape".to_string(),
                    "Forge an xUDT".to_string(),
                    "Proof Receipt Mint".to_string(),
                ],
            },
            Track {
                name: "Fiber Builder".to_string(),
                description: "Build paywalls, rewards, game loops, and creator payouts that use Fiber-style instant payments.".to_string(),
                sample_quests: vec![
                    "Receipt Scope Lab".to_string(),
                    "Channel Gate".to_string(),
                    "No-Prompt Checkout".to_string(),
                ],
            },
            Track {
                name: "AI Discipline".to_string(),
                description: "Use AI aggressively while proving you can reason about, defend, and extend generated code.".to_string(),
                sample_quests: vec![
                    "Prompt Budget Trial".to_string(),
                    "Explain Room".to_string(),
                    "Boss Diff Defense".to_string(),
                ],
            },
        ],
        gates: vec![
            Gate {
                name: "Explain".to_string(),
                unlocks: "User explains the generated subsystem in their own words.".to_string(),
            },
            Gate {
                name: "Debug".to_string(),
                unlocks: "User fixes a seeded bug and passes tests.".to_string(),
            },
            Gate {
                name: "Remix".to_string(),
                unlocks: "User extends the feature with limited AI help.".to_string(),
            },
            Gate {
                name: "Attack".to_string(),
                unlocks: "User finds or defends against a real failure mode.".to_string(),
            },
            Gate {
                name: "Ship".to_string(),
                unlocks: "CKB proof badge and Fiber reward become claimable.".to_string(),
            },
        ],
    })
}

async fn generate_quest(
    State(state): State<Arc<AppState>>,
    Json(mut request): Json<GenerateQuestRequest>,
) -> Result<Json<GenerateQuestResponse>, ApiError> {
    let trimmed_prompt = request.build_prompt.trim();
    if trimmed_prompt.chars().count() < 12 {
        return Err(ApiError::InvalidPrompt);
    }

    if is_learning_only_prompt(trimmed_prompt) {
        return Err(ApiError::LearningRequestNeedsModule);
    }

    validate_wallet_proof(&request.wallet)?;

    request.learning_context = request
        .learning_context
        .take()
        .map(compact_learning_quest_link);
    let run_id = Uuid::new_v4();
    let learning_context = request.learning_context.clone();
    let (quest, source, warning) = match state.openai.generate_quest(&request).await {
        Ok(quest) => (quest, QuestSource::OpenAi, None),
        Err(error) => return Err(error),
    };

    let mut response = GenerateQuestResponse {
        run_id,
        source,
        learning_context,
        wallet: WalletBinding {
            address: request.wallet.address.trim().to_string(),
            identity: request.wallet.signature.identity.trim().to_string(),
            sign_type: request.wallet.signature.sign_type.trim().to_string(),
            message: request.wallet.message.trim().to_string(),
        },
        quest,
        ship_requirements: ShipRequirements {
            ckb_rpc_ready: state.config.ckb_rpc_url.is_some(),
            fiber_rpc_ready: state.config.fiber_rpc_url.is_some(),
            can_claim_rewards: state.config.ckb_rpc_url.is_some()
                && state.config.fiber_rpc_url.is_some(),
        },
        persistence: PersistenceStatus {
            saved: false,
            warning: None,
        },
        warning,
    };

    match tokio::time::timeout(
        Duration::from_secs(3),
        state.store.record_generated_quest(
            &request,
            &response,
            state.config.reward_amount_shannons,
            &state.config.reward_currency,
        ),
    )
    .await
    {
        Ok(Ok(())) => {
            response.persistence.saved = true;
        }
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "quest generated but persistence is degraded");
            response.persistence.warning = Some(persistence_degraded_warning());
        }
        Err(_) => {
            warn!("quest generated but persistence timed out");
            response.persistence.warning = Some(persistence_degraded_warning());
        }
        Ok(Err(error)) => return Err(error),
    }

    Ok(Json(response))
}

fn persistence_degraded_warning() -> String {
    "AI quest generated, but cloud save is temporarily unavailable. You can practice now; reward claim unlocks after persistence recovers.".to_string()
}

fn learning_persistence_degraded_warning() -> String {
    "Learning state is active in this browser session, but cloud save is temporarily unavailable."
        .to_string()
}

async fn generate_learning_module(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<GenerateLearningModuleRequest>,
) -> Result<Json<GenerateLearningModuleResponse>, ApiError> {
    if request.learner_goal.trim().chars().count() < 8
        && request.interests.is_empty()
        && request
            .topic
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    {
        return Err(ApiError::InvalidPrompt);
    }

    let module = state.openai.generate_learning_module(&request).await?;
    let source = QuestSource::OpenAi;
    let provider = state.openai.provider_metadata();
    let eval_artifact = learning_eval_artifact(&request, &module, provider.clone());
    let mut module_id = Uuid::new_v4().to_string();
    let mut persistence = PersistenceStatus {
        saved: false,
        warning: None,
    };

    let save_request = SaveLearningSessionRequest {
        module_id: Some(module_id.clone()),
        source: Some(source),
        module: module.clone(),
        module_statuses: learning_module_statuses_from_module(&module, 5),
        eval_artifacts: vec![eval_artifact.clone()],
        ecosystem_id: Some(learning_ecosystem_id(&request)),
        topic: learning_topic_label(&request),
        learning_profile: request.learning_profile.clone(),
        learning_intents: request.learning_intents.clone(),
        selected_interests: request.interests.clone(),
        learner_goal: request.learner_goal.clone(),
        background: request.background.clone(),
        pace: request.pace.clone(),
        active_lesson_index: 0,
        checkpoint_answers: std::collections::BTreeMap::new(),
        tutor_messages: Vec::new(),
    };

    match tokio::time::timeout(
        Duration::from_secs(3),
        state.store.save_learning_session(&principal, save_request),
    )
    .await
    {
        Ok(Ok(session)) => {
            module_id = session.module_id;
            persistence.saved = true;
        }
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning module generated but persistence is degraded");
            persistence.warning = Some(learning_persistence_degraded_warning());
        }
        Err(_) => {
            warn!("learning module generated but persistence timed out");
            persistence.warning = Some(learning_persistence_degraded_warning());
        }
        Ok(Err(error)) => return Err(error),
    }

    Ok(Json(GenerateLearningModuleResponse {
        module_id,
        source,
        provider,
        module,
        eval_artifact,
        warning: persistence.warning.clone(),
        persistence,
    }))
}

async fn generate_learning_lesson_endpoint(
    State(state): State<Arc<AppState>>,
    Json(request): Json<GenerateLearningLessonRequest>,
) -> Result<Json<GenerateLearningLessonResponse>, ApiError> {
    let module_request = request.module_request();
    if module_request.learner_goal.trim().chars().count() < 8
        && module_request.interests.is_empty()
        && module_request
            .topic
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    {
        return Err(ApiError::InvalidPrompt);
    }
    if request.lesson_index >= 5 {
        return Err(ApiError::InvalidPrompt);
    }

    let lesson = state
        .openai
        .generate_learning_lesson_item(
            &module_request,
            request.lesson_index,
            &request.prior_lessons,
        )
        .await?;

    let module_status =
        module_generation_status_for_lesson(request.lesson_index, &lesson, "ready", None);
    let provider = state.openai.provider_metadata();
    let module_title = learning_module_title(&module_request);
    let learner_profile = learning_module_profile(&module_request);
    let outcome = learning_module_outcome(&module_request);
    let capstone_quest_prompt = learning_module_capstone_prompt(&module_request);
    let resources = default_learning_resources_for_focus(&learning_focus_label(&module_request));
    let eval_module = LearningModule {
        title: module_title.clone(),
        learner_profile: learner_profile.clone(),
        outcome: outcome.clone(),
        lessons: vec![lesson.clone()],
        capstone_quest_prompt: capstone_quest_prompt.clone(),
        resources: resources.clone(),
    };
    let eval_artifact = learning_eval_artifact(&module_request, &eval_module, provider.clone());

    Ok(Json(GenerateLearningLessonResponse {
        source: QuestSource::OpenAi,
        provider,
        module_title,
        learner_profile,
        outcome,
        capstone_quest_prompt,
        resources,
        lesson,
        lesson_index: request.lesson_index,
        module_status,
        eval_artifact,
        warning: None,
    }))
}

async fn answer_learning_question(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LearningTutorRequest>,
) -> Result<Json<LearningTutorResponse>, ApiError> {
    if request.question.trim().chars().count() < 4 {
        return Err(ApiError::InvalidPrompt);
    }

    Ok(Json(state.openai.answer_learning_question(&request).await?))
}

async fn answer_code_question(
    State(state): State<Arc<AppState>>,
    Json(mut request): Json<CodeTutorRequest>,
) -> Result<Json<CodeTutorResponse>, ApiError> {
    if request.question.trim().chars().count() < 4 || request.files.is_empty() {
        return Err(ApiError::InvalidPrompt);
    }

    request.quest_title = clamp_text(request.quest_title, 120);
    request.quest_objective = clamp_text(request.quest_objective, 500);
    request.question = clamp_text(request.question, 500);
    request.files = request
        .files
        .into_iter()
        .take(4)
        .map(|mut file| {
            file.path = clamp_text(file.path, 160);
            file.language = clamp_text(file.language, 40);
            file.content = compact_file_content(&file.content, 80);
            file
        })
        .filter(|file| !file.path.trim().is_empty() && !file.content.trim().is_empty())
        .collect();

    let mut answer = state.openai.answer_code_question(&request).await?;

    if let (Some(run_id), Some(wallet)) = (request.run_id.as_deref(), request.wallet.as_ref()) {
        match tokio::time::timeout(
            Duration::from_secs(3),
            state
                .store
                .append_code_tutor_exchange(run_id, wallet, &request, &answer),
        )
        .await
        {
            Ok(Ok(())) => {
                answer.persistence.saved = true;
            }
            Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
                warn!(%error, "code tutor answered but persistence is degraded");
                answer.persistence.warning = Some(persistence_degraded_warning());
            }
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                answer.persistence.warning = Some(persistence_degraded_warning());
            }
        }
    }

    Ok(Json(answer))
}

async fn api_get_learning_session(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearningSessionResponse>, ApiError> {
    match tokio::time::timeout(
        Duration::from_secs(3),
        state.store.get_learning_session(&principal.user_id),
    )
    .await
    {
        Ok(Ok(session)) => Ok(Json(LearningSessionResponse {
            persistence: PersistenceStatus {
                saved: session.is_some(),
                warning: if state.store.is_configured() {
                    None
                } else {
                    Some(learning_persistence_degraded_warning())
                },
            },
            session,
        })),
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning session resume is degraded");
            Ok(Json(LearningSessionResponse {
                session: None,
                persistence: PersistenceStatus {
                    saved: false,
                    warning: Some(learning_persistence_degraded_warning()),
                },
            }))
        }
        Err(_) => Ok(Json(LearningSessionResponse {
            session: None,
            persistence: PersistenceStatus {
                saved: false,
                warning: Some(learning_persistence_degraded_warning()),
            },
        })),
        Ok(Err(error)) => Err(error),
    }
}

async fn api_list_learning_sessions(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearningSessionsResponse>, ApiError> {
    match tokio::time::timeout(
        Duration::from_secs(3),
        state.store.list_learning_sessions(&principal.user_id),
    )
    .await
    {
        Ok(Ok(sessions)) => Ok(Json(LearningSessionsResponse {
            persistence: PersistenceStatus {
                saved: state.store.is_configured(),
                warning: if state.store.is_configured() {
                    None
                } else {
                    Some(learning_persistence_degraded_warning())
                },
            },
            sessions,
        })),
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning session list is degraded");
            Ok(Json(LearningSessionsResponse {
                sessions: Vec::new(),
                persistence: PersistenceStatus {
                    saved: false,
                    warning: Some(learning_persistence_degraded_warning()),
                },
            }))
        }
        Err(_) => Ok(Json(LearningSessionsResponse {
            sessions: Vec::new(),
            persistence: PersistenceStatus {
                saved: false,
                warning: Some(learning_persistence_degraded_warning()),
            },
        })),
        Ok(Err(error)) => Err(error),
    }
}

async fn api_save_learning_event(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<LearningEventRequest>,
) -> Result<Json<LearningEventResponse>, ApiError> {
    match tokio::time::timeout(
        Duration::from_secs(3),
        state.store.save_learning_event(&principal, request),
    )
    .await
    {
        Ok(Ok(())) => Ok(Json(LearningEventResponse {
            saved: true,
            persistence: PersistenceStatus {
                saved: true,
                warning: None,
            },
        })),
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning event persistence is degraded");
            Ok(Json(LearningEventResponse {
                saved: false,
                persistence: PersistenceStatus {
                    saved: false,
                    warning: Some(learning_persistence_degraded_warning()),
                },
            }))
        }
        Err(_) => Ok(Json(LearningEventResponse {
            saved: false,
            persistence: PersistenceStatus {
                saved: false,
                warning: Some(learning_persistence_degraded_warning()),
            },
        })),
        Ok(Err(error)) => Err(error),
    }
}

async fn api_learning_metrics(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearningMetricsResponse>, ApiError> {
    match tokio::time::timeout(
        Duration::from_secs(3),
        state.store.list_learning_events(&principal.user_id, 500),
    )
    .await
    {
        Ok(Ok(events)) => {
            let summary = summarize_learning_events(&events);
            let recent_events = events.into_iter().take(25).collect();
            Ok(Json(LearningMetricsResponse {
                summary,
                recent_events,
                persistence: PersistenceStatus {
                    saved: state.store.is_configured(),
                    warning: None,
                },
            }))
        }
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning metrics are degraded");
            Ok(Json(LearningMetricsResponse {
                summary: LearningMetricsSummary::default(),
                recent_events: Vec::new(),
                persistence: PersistenceStatus {
                    saved: false,
                    warning: Some(learning_persistence_degraded_warning()),
                },
            }))
        }
        Err(_) => Ok(Json(LearningMetricsResponse {
            summary: LearningMetricsSummary::default(),
            recent_events: Vec::new(),
            persistence: PersistenceStatus {
                saved: false,
                warning: Some(learning_persistence_degraded_warning()),
            },
        })),
        Ok(Err(error)) => Err(error),
    }
}

async fn api_export_learning_session(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Path(module_id): Path<String>,
) -> Result<Json<LearningSessionExportResponse>, ApiError> {
    match tokio::time::timeout(
        Duration::from_secs(3),
        state
            .store
            .get_learning_session_by_id(&principal.user_id, &module_id),
    )
    .await
    {
        Ok(Ok(session)) => {
            let markdown = session
                .as_ref()
                .map(learning_session_markdown)
                .unwrap_or_default();
            let json = session
                .as_ref()
                .and_then(|session| serde_json::to_value(session).ok())
                .unwrap_or(Value::Null);
            Ok(Json(LearningSessionExportResponse {
                session,
                markdown,
                json,
                persistence: PersistenceStatus {
                    saved: state.store.is_configured(),
                    warning: None,
                },
            }))
        }
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning session export is degraded");
            Ok(Json(LearningSessionExportResponse {
                session: None,
                markdown: String::new(),
                json: Value::Null,
                persistence: PersistenceStatus {
                    saved: false,
                    warning: Some(learning_persistence_degraded_warning()),
                },
            }))
        }
        Err(_) => Ok(Json(LearningSessionExportResponse {
            session: None,
            markdown: String::new(),
            json: Value::Null,
            persistence: PersistenceStatus {
                saved: false,
                warning: Some(learning_persistence_degraded_warning()),
            },
        })),
        Ok(Err(error)) => Err(error),
    }
}

async fn api_learning_admin_review(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<LearningAdminReviewResponse>, ApiError> {
    let sessions = match tokio::time::timeout(
        Duration::from_secs(3),
        state.store.list_learning_sessions(&principal.user_id),
    )
    .await
    {
        Ok(Ok(sessions)) => sessions,
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning admin sessions are degraded");
            Vec::new()
        }
        Err(_) => Vec::new(),
        Ok(Err(error)) => return Err(error),
    };

    let events = match tokio::time::timeout(
        Duration::from_secs(3),
        state.store.list_learning_events(&principal.user_id, 500),
    )
    .await
    {
        Ok(Ok(events)) => events,
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning admin events are degraded");
            Vec::new()
        }
        Err(_) => Vec::new(),
        Ok(Err(error)) => return Err(error),
    };

    Ok(Json(LearningAdminReviewResponse {
        metrics: summarize_learning_events(&events),
        recent_events: events.into_iter().take(25).collect(),
        sessions,
        persistence: PersistenceStatus {
            saved: state.store.is_configured(),
            warning: if state.store.is_configured() {
                None
            } else {
                Some(learning_persistence_degraded_warning())
            },
        },
    }))
}

async fn api_archive_learning_session(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Path(module_id): Path<String>,
) -> Result<Json<LearningSessionMutationResponse>, ApiError> {
    match tokio::time::timeout(
        Duration::from_secs(3),
        state
            .store
            .archive_learning_session(&principal.user_id, &module_id),
    )
    .await
    {
        Ok(Ok(archived)) => Ok(Json(LearningSessionMutationResponse {
            module_id,
            archived,
            deleted: false,
            persistence: PersistenceStatus {
                saved: archived,
                warning: None,
            },
        })),
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning session archive is degraded");
            Ok(Json(LearningSessionMutationResponse {
                module_id,
                archived: false,
                deleted: false,
                persistence: PersistenceStatus {
                    saved: false,
                    warning: Some(learning_persistence_degraded_warning()),
                },
            }))
        }
        Err(_) => Ok(Json(LearningSessionMutationResponse {
            module_id,
            archived: false,
            deleted: false,
            persistence: PersistenceStatus {
                saved: false,
                warning: Some(learning_persistence_degraded_warning()),
            },
        })),
        Ok(Err(error)) => Err(error),
    }
}

async fn api_delete_learning_session(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Path(module_id): Path<String>,
) -> Result<Json<LearningSessionMutationResponse>, ApiError> {
    match tokio::time::timeout(
        Duration::from_secs(3),
        state
            .store
            .delete_learning_session(&principal.user_id, &module_id),
    )
    .await
    {
        Ok(Ok(deleted)) => Ok(Json(LearningSessionMutationResponse {
            module_id,
            archived: false,
            deleted,
            persistence: PersistenceStatus {
                saved: deleted,
                warning: None,
            },
        })),
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning session delete is degraded");
            Ok(Json(LearningSessionMutationResponse {
                module_id,
                archived: false,
                deleted: false,
                persistence: PersistenceStatus {
                    saved: false,
                    warning: Some(learning_persistence_degraded_warning()),
                },
            }))
        }
        Err(_) => Ok(Json(LearningSessionMutationResponse {
            module_id,
            archived: false,
            deleted: false,
            persistence: PersistenceStatus {
                saved: false,
                warning: Some(learning_persistence_degraded_warning()),
            },
        })),
        Ok(Err(error)) => Err(error),
    }
}

async fn api_save_learning_session(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<SaveLearningSessionRequest>,
) -> Result<Json<SaveLearningSessionResponse>, ApiError> {
    match tokio::time::timeout(
        Duration::from_secs(3),
        state.store.save_learning_session(&principal, request),
    )
    .await
    {
        Ok(Ok(session)) => Ok(Json(SaveLearningSessionResponse {
            session: Some(session),
            persistence: PersistenceStatus {
                saved: true,
                warning: None,
            },
        })),
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning session save is degraded");
            Ok(Json(SaveLearningSessionResponse {
                session: None,
                persistence: PersistenceStatus {
                    saved: false,
                    warning: Some(learning_persistence_degraded_warning()),
                },
            }))
        }
        Err(_) => Ok(Json(SaveLearningSessionResponse {
            session: None,
            persistence: PersistenceStatus {
                saved: false,
                warning: Some(learning_persistence_degraded_warning()),
            },
        })),
        Ok(Err(error)) => Err(error),
    }
}

async fn api_save_learning_tutor_exchange(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<SaveTutorExchangeRequest>,
) -> Result<Json<SavedTutorExchangeResponse>, ApiError> {
    if request.question.trim().chars().count() < 4 {
        return Err(ApiError::InvalidPrompt);
    }

    let answer = state
        .openai
        .answer_learning_question(&LearningTutorRequest {
            module_title: request.module_title.clone(),
            lesson_title: request.lesson_title.clone(),
            lesson_context: request.lesson_context.clone(),
            question: request.question.clone(),
        })
        .await?;
    let mut persistence = PersistenceStatus {
        saved: false,
        warning: None,
    };
    let session = match tokio::time::timeout(
        Duration::from_secs(3),
        state
            .store
            .append_tutor_exchange(&principal, &request, &answer),
    )
    .await
    {
        Ok(Ok(session)) => {
            persistence.saved = session.is_some();
            session
        }
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning tutor answered but persistence is degraded");
            persistence.warning = Some(learning_persistence_degraded_warning());
            None
        }
        Err(_) => {
            persistence.warning = Some(learning_persistence_degraded_warning());
            None
        }
        Ok(Err(error)) => return Err(error),
    };

    Ok(Json(SavedTutorExchangeResponse {
        answer,
        session,
        persistence,
    }))
}

async fn generate_learning_quest(
    Extension(principal): Extension<AuthenticatedPrincipal>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<GenerateLearningQuestRequest>,
) -> Result<Json<GenerateLearningQuestResponse>, ApiError> {
    let learning_context = learning_quest_link_from_generated_lesson(&request)?;
    let ecosystem_id = request
        .ecosystem_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("zcash");
    let build_prompt = learning_quest_prompt_from_request(&request, ecosystem_id);
    let mut quest_request = GenerateQuestRequest {
        build_prompt,
        skill_track: Some(skill_track_for_learning_ecosystem(ecosystem_id).to_string()),
        difficulty: Some(Difficulty::Builder),
        wallet: wallet_proof_from_principal(&principal),
        learning_context: Some(learning_context.clone()),
    };

    let quest = state.openai.generate_quest(&quest_request).await?;
    let source = QuestSource::OpenAi;
    let run_id = Uuid::new_v4().to_string();
    let ecosystem_supported = ecosystem_id.eq_ignore_ascii_case("zcash");
    let runner_manifest = runner::runner_manifest();
    let mut persistence = PersistenceStatus {
        saved: false,
        warning: None,
    };

    let legacy_response = GenerateQuestResponse {
        run_id: Uuid::parse_str(&run_id).unwrap_or_else(|_| Uuid::new_v4()),
        source,
        learning_context: Some(learning_context.clone()),
        wallet: identity_binding_from_principal(&principal),
        quest: quest.clone(),
        ship_requirements: ShipRequirements {
            ckb_rpc_ready: state.config.ckb_rpc_url.is_some(),
            fiber_rpc_ready: state.config.fiber_rpc_url.is_some(),
            can_claim_rewards: state.config.ckb_rpc_url.is_some()
                && state.config.fiber_rpc_url.is_some(),
        },
        persistence: PersistenceStatus {
            saved: false,
            warning: None,
        },
        warning: None,
    };
    quest_request.learning_context = Some(learning_context.clone());

    match tokio::time::timeout(
        Duration::from_secs(3),
        state.store.record_generated_quest(
            &quest_request,
            &legacy_response,
            state.config.reward_amount_shannons,
            &state.config.reward_currency,
        ),
    )
    .await
    {
        Ok(Ok(())) => {
            persistence.saved = state.store.is_configured();
        }
        Ok(Err(error @ (ApiError::Database(_) | ApiError::DatabaseUnavailable))) => {
            warn!(%error, "learning quest generated but persistence is degraded");
            persistence.warning = Some(persistence_degraded_warning());
        }
        Err(_) => {
            warn!("learning quest generated but persistence timed out");
            persistence.warning = Some(persistence_degraded_warning());
        }
        Ok(Err(error)) => return Err(error),
    }

    Ok(Json(GenerateLearningQuestResponse {
        run_id,
        source,
        learning_context,
        quest,
        runner: LearningQuestRunnerState {
            enabled: state.runner.is_enabled(),
            ecosystem_supported,
            scenario_id: runner::RUNNER_SCENARIO_ID.to_string(),
            scenario_manifest_version: runner_manifest.scenario_manifest_version.clone(),
            runner_version: runner::RUNNER_VERSION.to_string(),
        },
        warning: persistence.warning.clone(),
        persistence,
    }))
}

async fn bind_wallet_user(
    State(state): State<Arc<AppState>>,
    Json(request): Json<BindWalletRequest>,
) -> Result<Json<UserProfileResponse>, ApiError> {
    Ok(Json(state.store.bind_wallet_user(request.wallet).await?))
}

async fn list_user_quests(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> Result<Json<UserQuestHistoryResponse>, ApiError> {
    let address = address.trim();
    if address.is_empty() {
        return Err(ApiError::MissingWalletAddress);
    }

    if let Err(message) = state.store.availability_diagnostic().await {
        return Ok(Json(degraded_user_history(message)));
    }

    match state.store.user_history(address).await {
        Ok(history) => Ok(Json(history)),
        Err(ApiError::Database(message)) => Ok(Json(degraded_user_history(message))),
        Err(ApiError::DatabaseUnavailable) => Ok(Json(degraded_user_history(
            "MONGODB_URI is not configured".to_string(),
        ))),
        Err(error) => Err(error),
    }
}

fn degraded_user_history(message: String) -> UserQuestHistoryResponse {
    UserQuestHistoryResponse {
        user: None,
        stats: UserQuestCounts::default(),
        active_run: None,
        runs: Vec::new(),
        reward_claims: Vec::new(),
        persistence: HistoryPersistenceStatus {
            available: false,
            message: Some(format!(
                "Quest history is syncing. Continue learning in this session; stored history will reconnect once MongoDB is reachable. Detail: {message}"
            )),
        },
    }
}

async fn get_quest_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> Result<Json<QuestRunRecord>, ApiError> {
    Ok(Json(state.store.get_run(&run_id).await?.into()))
}

async fn update_quest_progress(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Json(request): Json<UpdateQuestProgressRequest>,
) -> Result<Json<QuestRunRecord>, ApiError> {
    Ok(Json(state.store.update_progress(&run_id, request).await?))
}

async fn complete_quest(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
    Json(request): Json<CompleteQuestRequest>,
) -> Result<Json<CompleteQuestResponse>, ApiError> {
    Ok(Json(
        state
            .store
            .complete_quest(
                &run_id,
                request,
                state.config.reward_amount_shannons,
                &state.config.reward_currency,
                &state.fiber,
            )
            .await?,
    ))
}

async fn integration_status(state: &AppState) -> IntegrationStatus {
    IntegrationStatus {
        openai: state.openai.api_key.is_some(),
        ckb_rpc: state.config.ckb_rpc_url.is_some(),
        fiber_rpc: state.config.fiber_rpc_url.is_some(),
        fiber_payout: state.fiber.is_ready(),
        mongodb: state.store.is_available().await,
    }
}

fn missing_integrations(state: &AppState, integrations: &IntegrationStatus) -> Vec<&'static str> {
    let mut missing = Vec::new();

    if !integrations.openai {
        missing.push("OPENAI_API_KEY");
    }

    if !integrations.ckb_rpc {
        missing.push("CKB_RPC_URL");
    }

    if !integrations.fiber_rpc {
        missing.push("FIBER_RPC_URL");
    }

    if state.store.is_configured() {
        if !integrations.mongodb {
            missing.push("MONGODB_CONNECTION");
        }
    } else {
        missing.push("MONGODB_URI");
    }

    if state.config.fiber_payout_enabled && state.config.fiber_payout_rpc_url.is_none() {
        missing.push("FIBER_PAYOUT_RPC_URL");
    }

    missing
}

fn warn_missing_integrations(state: &AppState) {
    let integrations = IntegrationStatus {
        openai: state.openai.api_key.is_some(),
        ckb_rpc: state.config.ckb_rpc_url.is_some(),
        fiber_rpc: state.config.fiber_rpc_url.is_some(),
        fiber_payout: state.fiber.is_ready(),
        mongodb: state.store.is_configured(),
    };
    let missing = missing_integrations(state, &integrations);

    if !missing.is_empty() {
        warn!(
            missing = missing.join(", "),
            "vibequest-core is not fully configured"
        );
    }
}

fn sanitize_mongodb_error(error: &str) -> String {
    let lower = error.to_lowercase();

    if lower.contains("authentication") || lower.contains("auth") {
        "MongoDB authentication failed; verify username, password, and database user permissions"
            .to_string()
    } else if lower.contains("server selection") || lower.contains("no available servers") {
        "MongoDB server selection failed; verify Atlas network access, URI host, and cluster availability".to_string()
    } else if lower.contains("tls") || lower.contains("ssl") || lower.contains("alert") {
        "MongoDB TLS handshake failed; verify Atlas connectivity from Vercel and cluster TLS settings".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "MongoDB connection timed out; verify Atlas network access and cluster health".to_string()
    } else {
        "MongoDB ping failed; inspect Atlas network access, credentials, and cluster health"
            .to_string()
    }
}

fn validate_wallet_proof(wallet: &WalletProof) -> Result<(), ApiError> {
    if wallet.address.trim().is_empty() {
        return Err(ApiError::MissingWalletAddress);
    }

    if wallet.signature.signature.trim().is_empty()
        || wallet.signature.identity.trim().is_empty()
        || wallet.signature.sign_type.trim().is_empty()
    {
        return Err(ApiError::MissingWalletSignature);
    }

    if !wallet.message.contains("VibeQuest") || !wallet.message.contains(wallet.address.trim()) {
        return Err(ApiError::InvalidWalletProofMessage);
    }

    verify_joyid_wallet_proof(wallet)
}

#[derive(Debug, Deserialize)]
struct JoyIdSignaturePayload {
    signature: String,
    alg: Value,
    message: String,
    pubkey: Option<String>,
    challenge: Option<String>,
    #[serde(rename = "keyType")]
    key_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JoyIdIdentityPayload {
    #[serde(rename = "keyType")]
    key_type: String,
    #[serde(rename = "publicKey")]
    public_key: String,
}

fn is_joyid_sign_type(sign_type: &str) -> bool {
    let normalized: String = sign_type
        .trim()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();

    matches!(normalized.as_str(), "joyid" | "signersigntypejoyid")
}

fn verify_joyid_wallet_proof(wallet: &WalletProof) -> Result<(), ApiError> {
    if !is_joyid_sign_type(&wallet.signature.sign_type) {
        return Err(ApiError::UnsupportedWalletSignature);
    }

    let signature_payload =
        serde_json::from_str::<JoyIdSignaturePayload>(&wallet.signature.signature)
            .map_err(|_| ApiError::InvalidWalletSignature)?;
    let identity_payload = serde_json::from_str::<JoyIdIdentityPayload>(&wallet.signature.identity)
        .map_err(|_| ApiError::InvalidWalletSignature)?;

    let pubkey = wallet
        .signature
        .pubkey
        .as_deref()
        .or(signature_payload.pubkey.as_deref())
        .unwrap_or(identity_payload.public_key.as_str())
        .trim()
        .trim_start_matches("0x");
    let key_type = wallet
        .signature
        .key_type
        .as_deref()
        .or(signature_payload.key_type.as_deref())
        .unwrap_or(identity_payload.key_type.as_str())
        .trim();
    let alg = wallet
        .signature
        .alg
        .as_ref()
        .unwrap_or(&signature_payload.alg);
    let challenge = wallet
        .signature
        .challenge
        .as_deref()
        .or(signature_payload.challenge.as_deref())
        .unwrap_or(wallet.message.as_str());

    if signature_payload.signature.trim().is_empty()
        || signature_payload.message.trim().is_empty()
        || alg.is_null()
        || !is_joyid_key_type(key_type)
        || !is_hex_public_key(pubkey)
    {
        return Err(ApiError::InvalidWalletSignature);
    }

    let Some(signed_challenge) = joyid_signed_challenge(&signature_payload.message) else {
        return Err(ApiError::InvalidWalletSignature);
    };

    if signed_challenge != wallet.message || challenge != wallet.message {
        return Err(ApiError::InvalidWalletSignature);
    }

    if !verify_joyid_signature(
        key_type,
        alg,
        pubkey,
        &signature_payload.message,
        &signature_payload.signature,
    ) {
        return Err(ApiError::InvalidWalletSignature);
    }

    Ok(())
}

fn verify_joyid_signature(
    key_type: &str,
    alg: &Value,
    pubkey_hex: &str,
    message: &str,
    signature_value: &str,
) -> bool {
    let Some(message_bytes) = decode_base64_url(message) else {
        return false;
    };
    let Some(signature_bytes) = decode_base64_url(signature_value) else {
        return false;
    };
    let Some(pubkey_bytes) = decode_hex(pubkey_hex) else {
        return false;
    };

    if matches!(key_type.trim(), "main_session_key" | "sub_session_key") {
        return verify_rs256(&pubkey_bytes, &message_bytes, &signature_bytes);
    }

    let Some(client_data_start) = find_client_data_start(&message_bytes) else {
        return false;
    };
    if client_data_start < 37 {
        return false;
    }
    let auth_data = &message_bytes[..37];
    let client_data = &message_bytes[client_data_start..];
    let client_hash = Sha256::digest(client_data);
    let mut signature_base = Vec::with_capacity(auth_data.len() + client_hash.len());
    signature_base.extend_from_slice(auth_data);
    signature_base.extend_from_slice(&client_hash);

    if joyid_alg_is_rs256(alg) {
        verify_rs256(&pubkey_bytes, &signature_base, &signature_bytes)
    } else if joyid_alg_is_es256(alg) {
        verify_es256(&pubkey_bytes, &signature_base, &signature_bytes)
    } else {
        false
    }
}

fn verify_es256(pubkey: &[u8], message: &[u8], der_signature: &[u8]) -> bool {
    let key_bytes = if pubkey.len() == 64 {
        let mut uncompressed = Vec::with_capacity(65);
        uncompressed.push(0x04);
        uncompressed.extend_from_slice(pubkey);
        uncompressed
    } else {
        pubkey.to_vec()
    };

    let Ok(key) = VerifyingKey::from_sec1_bytes(&key_bytes) else {
        return false;
    };
    let Ok(signature) = P256Signature::from_der(der_signature) else {
        return false;
    };

    key.verify(message, &signature).is_ok()
}

fn verify_rs256(pubkey: &[u8], message: &[u8], signature_bytes: &[u8]) -> bool {
    if pubkey.len() <= 4 {
        return false;
    }

    let exponent = pubkey[..3].iter().rev().copied().collect::<Vec<_>>();
    let modulus = pubkey[4..].iter().rev().copied().collect::<Vec<_>>();
    let components = RsaPublicKeyComponents {
        n: &modulus,
        e: &exponent,
    };

    components
        .verify(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            message,
            signature_bytes,
        )
        .is_ok()
}

fn joyid_alg_is_es256(value: &Value) -> bool {
    value.as_i64() == Some(-7)
        || value
            .as_str()
            .is_some_and(|value| value.eq_ignore_ascii_case("ES256"))
}

fn joyid_alg_is_rs256(value: &Value) -> bool {
    value.as_i64() == Some(-257)
        || value
            .as_str()
            .is_some_and(|value| value.eq_ignore_ascii_case("RS256"))
}

fn decode_base64_url(value: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value.as_bytes())
        .ok()
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    let trimmed = value.trim().trim_start_matches("0x");
    if !trimmed.len().is_multiple_of(2)
        || !trimmed
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return None;
    }

    (0..trimmed.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&trimmed[index..index + 2], 16).ok())
        .collect()
}

fn find_client_data_start(bytes: &[u8]) -> Option<usize> {
    bytes.windows(2).position(|window| window == b"{\"")
}

fn is_joyid_key_type(value: &str) -> bool {
    matches!(
        value.trim(),
        "main_key" | "sub_key" | "main_session_key" | "sub_session_key"
    )
}

fn is_hex_public_key(value: &str) -> bool {
    let trimmed = value.trim().trim_start_matches("0x");

    !trimmed.is_empty()
        && trimmed.len().is_multiple_of(2)
        && trimmed
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

fn joyid_signed_challenge(message: &str) -> Option<String> {
    let bytes = decode_base64_url(message)?;
    let client_data_start = find_client_data_start(&bytes).unwrap_or(0);
    let client_data = std::str::from_utf8(&bytes[client_data_start..]).ok()?;

    if let Ok(parsed) = serde_json::from_str::<Value>(client_data) {
        let encoded_challenge = parsed.get("challenge")?.as_str()?;
        let challenge_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded_challenge.as_bytes())
            .ok()?;

        return String::from_utf8(challenge_bytes).ok();
    }

    String::from_utf8(bytes).ok()
}
fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_csv_env(name: &str, default: Vec<String>) -> Vec<String> {
    env::var(name)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .unwrap_or(default)
}

fn parse_bool_env(name: &str, default: bool) -> bool {
    optional_env(name)
        .and_then(|value| match value.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        })
        .unwrap_or(default)
}

fn truncate_error_body(body: &str) -> String {
    const MAX_ERROR_BODY_CHARS: usize = 700;

    let trimmed = body.trim();
    if trimmed.chars().count() <= MAX_ERROR_BODY_CHARS {
        return trimmed.to_string();
    }

    let mut truncated = trimmed
        .chars()
        .take(MAX_ERROR_BODY_CHARS)
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn ai_generated_quest_file(
    content: String,
    kind: &str,
    short_seed: &str,
) -> Result<String, ApiError> {
    let trimmed = content.trim();
    if trimmed.chars().count() < 220 {
        return Err(ApiError::InvalidAiResponse);
    }

    let lower = trimmed.to_lowercase();
    let has_domain_signal = [
        "ckb",
        "cell",
        "witness",
        "script",
        "xudt",
        "fiber",
        "invoice",
        "ptlc",
        "htlc",
        "channel",
        "proof",
        "receipt",
        "payout",
        "runid",
        "ton",
        "ston.fi",
        "stonfi",
        "omniston",
        "jetton",
        "slippage",
        "minout",
        "min-out",
        "quote",
        "route",
        "swap",
        "ton connect",
        "transaction state",
    ]
    .iter()
    .any(|term| lower.contains(term));
    let has_denial_signal = [
        "reject", "block", "false", "throw", "invalid", "unpaid", "mismatch", "replay", "stale",
    ]
    .iter()
    .any(|term| lower.contains(term));
    let has_code_signal =
        lower.contains("export") || lower.contains("function") || lower.contains("const");
    let has_test_signal =
        lower.contains("test") || lower.contains("expect") || lower.contains("assert");

    let valid = match kind {
        "implementation" => has_code_signal && has_domain_signal,
        "test" => has_test_signal && has_domain_signal && has_denial_signal,
        _ => false,
    };

    if !valid {
        return Err(ApiError::InvalidAiResponse);
    }

    Ok(trimmed.replace("__RUN_SEED__", short_seed))
}

fn compact_quest_blueprint(
    mut quest: QuestBlueprint,
    learning_context: Option<&LearningQuestLink>,
) -> Result<QuestBlueprint, ApiError> {
    quest.title = compact_required_text(quest.title, 80)?;
    quest.premise = compact_required_text(quest.premise, 420)?;
    quest.build_objective = compact_required_text(quest.build_objective, 420)?;
    quest.boss_fight = compact_required_text(quest.boss_fight, 420)?;
    quest.reward_logic = compact_required_text(quest.reward_logic, 320)?;
    quest.code_explainer = compact_code_explainer(quest.code_explainer)?;

    quest.comprehension_gates = compact_required_list(quest.comprehension_gates, 3, 180)?;
    quest.ckb_fiber_hooks = compact_required_list(quest.ckb_fiber_hooks, 2, 220)?;

    if quest.workbench_files.len() < 2 {
        return Err(ApiError::InvalidAiResponse);
    }
    if quest.workbench_files.len() > 3 {
        quest.workbench_files.truncate(3);
    }

    let uuid_seed = Uuid::new_v4().simple().to_string();
    let short_seed = &uuid_seed[..8];

    for file in &mut quest.workbench_files {
        file.path = compact_required_text(std::mem::take(&mut file.path), 120)?;
        if file.language.trim().is_empty() {
            file.language = infer_workbench_language(&file.path).to_string();
        } else {
            file.language = clamp_text(std::mem::take(&mut file.language), 40);
        }
        file.content = ai_generated_quest_file(
            std::mem::take(&mut file.content),
            if file.path.to_lowercase().contains("test")
                || file.path.to_lowercase().contains("spec")
            {
                "test"
            } else {
                "implementation"
            },
            short_seed,
        )?;
        file.content = compact_file_content(&file.content, 95);
    }

    let challenge = quest
        .challenge_brief
        .take()
        .ok_or(ApiError::InvalidAiResponse)?;
    quest.challenge_brief = Some(compact_challenge_brief(challenge)?);
    validate_quest_quality(&quest)?;
    if let Some(context) = learning_context {
        validate_lesson_quest_alignment(&quest, context)?;
        validate_no_repeated_lesson_scaffold(&quest)?;
    }

    Ok(quest)
}

fn compact_required_text(value: String, limit: usize) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::InvalidAiResponse);
    }

    Ok(clamp_text(trimmed.to_string(), limit))
}

fn compact_required_list(
    values: Vec<String>,
    min_len: usize,
    max_len: usize,
) -> Result<Vec<String>, ApiError> {
    let mut compacted = values
        .into_iter()
        .map(|value| clamp_text(value.trim().to_string(), max_len))
        .filter(|value| !value.trim().is_empty())
        .take(min_len)
        .collect::<Vec<_>>();

    if compacted.len() < min_len {
        return Err(ApiError::InvalidAiResponse);
    }

    compacted.truncate(min_len);
    Ok(compacted)
}

fn validate_quest_quality(quest: &QuestBlueprint) -> Result<(), ApiError> {
    let workspace = quest
        .workbench_files
        .iter()
        .map(|file| format!("{}\n{}", file.path, file.content))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let prose = format!(
        "{} {} {} {} {}",
        quest.title,
        quest.premise,
        quest.build_objective,
        quest.boss_fight,
        quest.ckb_fiber_hooks.join(" ")
    )
    .to_lowercase();
    let challenge = quest
        .challenge_brief
        .as_ref()
        .ok_or(ApiError::InvalidAiResponse)?;
    let explainer_text = format!(
        "{} {} {} {} {} {} {} {}",
        quest.code_explainer.primary_invariant,
        quest.code_explainer.denial_path,
        quest.code_explainer.proof_label,
        quest.code_explainer.proof_artifact,
        quest.code_explainer.network_label,
        quest.code_explainer.network_boundary,
        quest.code_explainer.risk_focus,
        quest.code_explainer.inspect_steps.join(" "),
    )
    .to_lowercase();
    let challenge_text = format!(
        "{} {} {} {} {} {} {} {}",
        challenge.question,
        challenge.correct_answer,
        challenge.invariant,
        challenge.attack_scenario,
        challenge.code_focus,
        challenge.test_focus,
        challenge.hint,
        explainer_text,
    )
    .to_lowercase();

    let has_implementation = quest.workbench_files.iter().any(|file| {
        let path = file.path.to_lowercase();
        !path.contains("test") && !path.contains("spec")
    });
    let has_test = quest.workbench_files.iter().any(|file| {
        let haystack = format!("{}\n{}", file.path, file.content).to_lowercase();
        haystack.contains("test")
            || haystack.contains("assert")
            || haystack.contains("expect")
            || haystack.contains("throws")
    });
    let has_domain_signal = [
        "ckb",
        "cell",
        "witness",
        "script",
        "xudt",
        "fiber",
        "invoice",
        "ptlc",
        "htlc",
        "channel",
        "proof",
        "receipt",
        "payout",
        "ton",
        "ston.fi",
        "stonfi",
        "omniston",
        "jetton",
        "slippage",
        "minout",
        "min-out",
        "quote",
        "route",
        "swap",
        "ton connect",
    ]
    .iter()
    .any(|term| workspace.contains(term));
    let has_denial_signal = [
        "reject",
        "block",
        "false",
        "throw",
        "invalid",
        "unpaid",
        "deny",
        "mismatch",
        "unauthorized",
        "replay",
    ]
    .iter()
    .any(|term| workspace.contains(term));
    let has_specific_challenge = [
        "cell",
        "witness",
        "script",
        "xudt",
        "fiber",
        "invoice",
        "ptlc",
        "htlc",
        "channel",
        "proof",
        "receipt",
        "payout",
        "reader",
        "run",
        "content",
        "outpoint",
        "nonce",
        "ton",
        "ston.fi",
        "stonfi",
        "omniston",
        "jetton",
        "slippage",
        "minout",
        "min-out",
        "quote",
        "route",
        "swap",
        "ton connect",
        "transaction",
    ]
    .iter()
    .any(|term| workspace.contains(term) && challenge_text.contains(term));
    let rejects_generic_reward_logic = ![
        "ui renders",
        "reward amount exists",
        "enough files",
        "looks complete",
        "happy path only",
    ]
    .iter()
    .any(|phrase| challenge.correct_answer.to_lowercase().contains(phrase));
    let has_ai_explainer = quest.code_explainer.inspect_steps.len() >= 4
        && quest.code_explainer.mentor_prompts.len() >= 4
        && !quest.code_explainer.primary_invariant.trim().is_empty()
        && challenge_text.contains(&quest.code_explainer.risk_focus.to_lowercase());
    let prompt_relevance = quest
        .build_objective
        .split_whitespace()
        .filter(|word| word.len() >= 5)
        .take(12)
        .any(|word| {
            prose.contains(&word.to_lowercase()) || workspace.contains(&word.to_lowercase())
        });

    if has_implementation
        && has_test
        && has_domain_signal
        && has_denial_signal
        && has_specific_challenge
        && rejects_generic_reward_logic
        && has_ai_explainer
        && prompt_relevance
    {
        Ok(())
    } else {
        warn!(
            title = %quest.title,
            has_implementation,
            has_test,
            has_domain_signal,
            has_denial_signal,
            has_specific_challenge,
            rejects_generic_reward_logic,
            has_ai_explainer,
            prompt_relevance,
            "AI-authored quest failed quality gate"
        );
        Err(ApiError::InvalidAiResponse)
    }
}

fn validate_lesson_quest_alignment(
    quest: &QuestBlueprint,
    context: &LearningQuestLink,
) -> Result<(), ApiError> {
    let challenge = quest
        .challenge_brief
        .as_ref()
        .ok_or(ApiError::InvalidAiResponse)?;
    let haystack = format!(
        "{} {} {} {} {} {} {} {}",
        quest.title,
        quest.premise,
        quest.build_objective,
        quest.boss_fight,
        challenge.question,
        challenge.correct_answer,
        challenge.invariant,
        quest
            .workbench_files
            .iter()
            .map(|file| format!("{} {}", file.path, file.content))
            .collect::<Vec<_>>()
            .join(" "),
    )
    .to_lowercase();

    let concept_hits = context
        .concepts
        .iter()
        .flat_map(|concept| significant_context_terms(concept))
        .filter(|term| haystack.contains(term))
        .take(3)
        .count();
    let context_text = format!(
        "{} {} {} {} {}",
        context.lesson_title,
        context.checkpoint_question,
        context.correct_answer,
        context.misunderstanding,
        context.quest_bridge,
    );
    let mut matched_terms: Vec<String> = Vec::new();
    for term in significant_context_terms(&context_text) {
        if haystack.contains(&term) && !matched_terms.iter().any(|existing| existing == &term) {
            matched_terms.push(term);
        }
        if matched_terms.len() >= 4 {
            break;
        }
    }
    let generated_filler_marker = ["place", "holder"].join("");
    let generic_output = [
        "generic template",
        "generic challenge",
        "stock variable",
        "sample quest",
        "paywall reactor",
        "fiber proof run",
    ]
    .iter()
    .any(|phrase| haystack.contains(phrase))
        || haystack.contains(&generated_filler_marker);
    if generic_output || (concept_hits == 0 && matched_terms.len() < 3) {
        warn!(
            title = %quest.title,
            lesson = %context.lesson_title,
            concept_hits,
            matched_terms = matched_terms.len(),
            generic_output,
            "AI quest did not align to completed lesson context"
        );
        return Err(ApiError::InvalidAiResponse);
    }

    Ok(())
}

fn significant_context_terms(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .map(|word| word.trim().to_ascii_lowercase())
        .filter(|word| {
            matches!(word.as_str(), "ckb" | "xudt" | "ptlc" | "htlc")
                || (word.len() >= 4
                    && !matches!(
                        word.as_str(),
                        "this"
                            | "that"
                            | "with"
                            | "from"
                            | "into"
                            | "must"
                            | "should"
                            | "quest"
                            | "lesson"
                            | "code"
                            | "test"
                            | "generated"
                            | "correct"
                            | "answer"
                            | "before"
                            | "after"
                            | "include"
                            | "practice"
                            | "learner"
                    ))
        })
        .take(24)
        .collect()
}

fn validate_no_repeated_lesson_scaffold(quest: &QuestBlueprint) -> Result<(), ApiError> {
    let workspace = quest
        .workbench_files
        .iter()
        .map(|file| format!("{} {}", file.path, file.content))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let title = quest.title.to_ascii_lowercase();
    let explainer = format!(
        "{} {} {} {}",
        quest.code_explainer.primary_invariant,
        quest.code_explainer.denial_path,
        quest.code_explainer.proof_artifact,
        quest.code_explainer.network_boundary,
    )
    .to_ascii_lowercase();
    let text = format!("{} {} {}", title, workspace, explainer);
    let banned = [
        "cellverifier.ts",
        "cellverifier.test.ts",
        "verifyckbcellproof",
        "verifygeneratedreceipt",
        "src/quest.ts",
        "test/quest.test.ts",
        "active_run_id",
        "lesson_invariant",
        "fiber proof run",
        "paywall reactor",
        "practice quest",
        "the witness must bind run id, input index, outpoint, cell data hash, and lock/type scripts",
    ];

    if banned.iter().any(|pattern| text.contains(pattern)) {
        warn!(title = %quest.title, "AI quest repeated a rejected lesson scaffold");
        return Err(ApiError::InvalidAiResponse);
    }

    Ok(())
}

fn compact_code_explainer(
    mut explainer: QuestCodeExplainer,
) -> Result<QuestCodeExplainer, ApiError> {
    explainer.primary_invariant = compact_required_text(explainer.primary_invariant, 280)?;
    explainer.denial_path = compact_required_text(explainer.denial_path, 320)?;
    explainer.proof_label = compact_required_text(explainer.proof_label, 48)?;
    explainer.proof_artifact = compact_required_text(explainer.proof_artifact, 320)?;
    explainer.network_label = compact_required_text(explainer.network_label, 48)?;
    explainer.network_boundary = compact_required_text(explainer.network_boundary, 320)?;
    explainer.risk_focus = compact_required_text(explainer.risk_focus, 120)?;
    explainer.inspect_steps = compact_required_list(explainer.inspect_steps, 4, 180)?;
    explainer.mentor_prompts = compact_required_list(explainer.mentor_prompts, 4, 90)?;
    explainer.resources = compact_learning_resources(explainer.resources);
    if explainer.resources.is_empty() {
        explainer.resources = default_learning_resources().into_iter().take(2).collect();
    }
    Ok(explainer)
}

fn compact_challenge_brief(
    mut brief: QuestChallengeBrief,
) -> Result<QuestChallengeBrief, ApiError> {
    brief.question = compact_required_text(brief.question, 260)?;
    brief.correct_answer = compact_required_text(brief.correct_answer, 320)?;
    brief.invariant = compact_required_text(brief.invariant, 260)?;
    brief.attack_scenario = compact_required_text(brief.attack_scenario, 260)?;
    brief.code_focus = compact_required_text(brief.code_focus, 180)?;
    brief.test_focus = compact_required_text(brief.test_focus, 220)?;
    brief.hint = compact_required_text(brief.hint, 260)?;
    brief.follow_up_question = compact_required_text(brief.follow_up_question, 260)?;
    brief.wrong_answers = compact_wrong_answers(brief.wrong_answers)?;
    brief.resources = compact_learning_resources(brief.resources);
    if brief.resources.is_empty() {
        brief.resources = default_learning_resources().into_iter().take(2).collect();
    }
    Ok(brief)
}

fn compact_wrong_answers(
    values: Vec<ChallengeWrongAnswer>,
) -> Result<Vec<ChallengeWrongAnswer>, ApiError> {
    let answers = values
        .into_iter()
        .filter(|answer| !answer.label.trim().is_empty() && !answer.feedback.trim().is_empty())
        .map(|answer| ChallengeWrongAnswer {
            label: clamp_text(answer.label, 180),
            feedback: clamp_text(answer.feedback, 260),
        })
        .take(3)
        .collect::<Vec<_>>();

    if answers.len() < 3 {
        return Err(ApiError::InvalidAiResponse);
    }

    Ok(answers)
}

fn compact_boss_attempt(attempt: BossAttemptRequest) -> BossAttempt {
    BossAttempt {
        selected_index: attempt.selected_index.clamp(0, 12),
        selected_label: clamp_text(attempt.selected_label, 220),
        correct: attempt.correct,
        feedback: clamp_text(attempt.feedback, 360),
        follow_up_question: clamp_text(attempt.follow_up_question, 260),
        created_at: Utc::now(),
    }
}

fn non_empty_or(value: String, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

fn compact_learning_quest_link(link: LearningQuestLink) -> LearningQuestLink {
    LearningQuestLink {
        module_id: clamp_text(link.module_id, 120),
        lesson_id: clamp_text(link.lesson_id, 120),
        module_title: clamp_text(link.module_title, 140),
        lesson_title: clamp_text(link.lesson_title, 140),
        checkpoint_question: clamp_text(link.checkpoint_question, 260),
        quest_bridge: clamp_text(link.quest_bridge, 240),
        concepts: link
            .concepts
            .into_iter()
            .map(|concept| clamp_text(concept, 48))
            .filter(|concept| !concept.trim().is_empty())
            .take(8)
            .collect(),
        correct_answer: clamp_text(link.correct_answer, 220),
        misunderstanding: clamp_text(link.misunderstanding, 240),
        lesson_summary: clamp_text(link.lesson_summary, 260),
    }
}

fn learning_quest_link_from_generated_lesson(
    request: &GenerateLearningQuestRequest,
) -> Result<LearningQuestLink, ApiError> {
    if request.module_id.trim().is_empty()
        || request.module_title.trim().is_empty()
        || request.lesson.title.trim().is_empty()
    {
        return Err(ApiError::InvalidPrompt);
    }
    let correct_answer = request
        .lesson
        .checkpoint
        .options
        .get(request.lesson.checkpoint.correct_index)
        .map(|option| option.label.clone())
        .unwrap_or_else(|| request.lesson.checkpoint.explanation.clone());
    let misunderstanding = request
        .lesson
        .checkpoint
        .options
        .iter()
        .enumerate()
        .find(|(index, option)| {
            *index != request.lesson.checkpoint.correct_index && !option.feedback.trim().is_empty()
        })
        .map(|(_, option)| option.feedback.clone())
        .unwrap_or_else(|| request.lesson.checkpoint.follow_up_question.clone());

    Ok(compact_learning_quest_link(LearningQuestLink {
        module_id: request.module_id.clone(),
        lesson_id: request.lesson.id.clone(),
        module_title: request.module_title.clone(),
        lesson_title: request.lesson.title.clone(),
        checkpoint_question: request.lesson.checkpoint.question.clone(),
        quest_bridge: request.lesson.quest_bridge.clone(),
        concepts: request.lesson.concepts.clone(),
        correct_answer,
        misunderstanding,
        lesson_summary: request.lesson.explanation.clone(),
    }))
}

fn learning_quest_prompt_from_request(
    request: &GenerateLearningQuestRequest,
    ecosystem_id: &str,
) -> String {
    let ecosystem_directive = match ecosystem_id.to_ascii_lowercase().as_str() {
        "ckb" => {
            "Focus on CKB cells, OutPoints, scripts, witnesses, transaction proof boundaries, and denial tests for replay or mismatched cell state."
        }
        "fiber" => {
            "Focus on Fiber invoices, channels, PTLC/preimage evidence, receipt replay defense, routing assumptions, and unpaid denial paths."
        }
        "zcash" => {
            "Focus on Zcash shielded checkout, ZIP-321 payment request validation, viewing-key boundaries, memo/privacy safety, and denial tests for unsafe recipients or wrong-network state."
        }
        _ => {
            "Focus on one concrete protocol boundary, generated implementation files, verifier checks, and denial tests tied to the lesson."
        }
    };

    format!(
        "Generate a code quest from this completed VibeQuest lesson. Ecosystem: {ecosystem_id}. Topic: {topic}. Learner profile: {profile}. Module: {module}. Outcome: {outcome}. Lesson: {lesson}. Why it matters: {why}. Concepts: {concepts}. Checkpoint: {checkpoint}. Correct answer: {answer}. Misunderstanding to defend against: {misunderstanding}. Quest bridge: {bridge}. {ecosystem_directive} Return implementation files, tests, a code explainer, and a boss challenge. Keep the scope narrow and executable; do not broaden into a generic ecosystem overview.",
        topic = request
            .topic
            .as_deref()
            .unwrap_or("generated lesson practice"),
        profile = request.learner_profile,
        module = request.module_title,
        outcome = request.outcome,
        lesson = request.lesson.title,
        why = request.lesson.why_it_matters,
        concepts = request.lesson.concepts.join(", "),
        checkpoint = request.lesson.checkpoint.question,
        answer = request
            .lesson
            .checkpoint
            .options
            .get(request.lesson.checkpoint.correct_index)
            .map(|option| option.label.as_str())
            .unwrap_or("the checkpoint explanation"),
        misunderstanding = request
            .lesson
            .checkpoint
            .options
            .iter()
            .enumerate()
            .find(|(index, option)| {
                *index != request.lesson.checkpoint.correct_index
                    && !option.feedback.trim().is_empty()
            })
            .map(|(_, option)| option.feedback.as_str())
            .unwrap_or(request.lesson.checkpoint.follow_up_question.as_str()),
        bridge = request.lesson.quest_bridge,
    )
}

fn skill_track_for_learning_ecosystem(ecosystem_id: &str) -> &'static str {
    match ecosystem_id.to_ascii_lowercase().as_str() {
        "ckb" => "CKB Fundamentals",
        "fiber" => "Fiber Builder",
        "zcash" => "Zcash Shielded Payments",
        _ => "Protocol Builder",
    }
}

fn identity_binding_from_principal(principal: &AuthenticatedPrincipal) -> WalletBinding {
    WalletBinding {
        address: principal.user_id.clone(),
        identity: principal.provider_subject.clone(),
        sign_type: principal.provider.clone(),
        message: principal
            .email
            .clone()
            .or_else(|| principal.name.clone())
            .unwrap_or_else(|| "google-authenticated".to_string()),
    }
}

fn wallet_proof_from_principal(principal: &AuthenticatedPrincipal) -> WalletProof {
    WalletProof {
        address: principal.user_id.clone(),
        message: format!(
            "VibeQuest Google-authenticated quest for {}",
            principal.user_id
        ),
        signature: WalletSignature {
            signature: principal.assertion_id.clone(),
            identity: principal.provider_subject.clone(),
            sign_type: principal.provider.clone(),
            pubkey: None,
            key_type: None,
            challenge: None,
            alg: None,
        },
    }
}

fn wallet_binding_from_proof(wallet: &WalletProof) -> WalletBinding {
    WalletBinding {
        address: wallet.address.trim().to_string(),
        identity: wallet.signature.identity.trim().to_string(),
        sign_type: wallet.signature.sign_type.trim().to_string(),
        message: wallet.message.trim().to_string(),
    }
}

fn compact_string_list(values: Vec<String>, limit: usize, max_chars: usize) -> Vec<String> {
    values
        .into_iter()
        .map(|value| clamp_text(value, max_chars))
        .filter(|value| !value.trim().is_empty())
        .take(limit)
        .collect()
}

fn checkpoint_answers_document(values: std::collections::BTreeMap<String, i64>) -> Document {
    let mut document = Document::new();
    for (key, value) in values.into_iter().take(50) {
        if !key.trim().is_empty() {
            document.insert(key, value);
        }
    }
    document
}

fn document_to_checkpoint_answers(document: Document) -> std::collections::BTreeMap<String, i64> {
    document
        .into_iter()
        .filter_map(|(key, value)| match value {
            mongodb::bson::Bson::Int32(value) => Some((key, i64::from(value))),
            mongodb::bson::Bson::Int64(value) => Some((key, value)),
            mongodb::bson::Bson::Double(value) => Some((key, value as i64)),
            _ => None,
        })
        .collect()
}

fn compact_code_tutor_messages(messages: Vec<CodeTutorMessage>) -> Vec<CodeTutorMessage> {
    messages
        .into_iter()
        .filter(|message| {
            (message.role == "learner" || message.role == "mentor")
                && !message.text.trim().is_empty()
        })
        .map(|message| CodeTutorMessage {
            id: clamp_text(message.id, 80),
            role: message.role,
            text: clamp_text(message.text, 900),
            code_walkthrough: compact_string_list(message.code_walkthrough, 5, 220),
            common_misunderstanding: message
                .common_misunderstanding
                .map(|value| clamp_text(value, 360)),
            follow_up_question: message
                .follow_up_question
                .map(|value| clamp_text(value, 260)),
            references: compact_learning_resources(message.references),
            created_at: message.created_at,
        })
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn find_lesson_id_for_title(module: &LearningModule, lesson_title: &str) -> Option<String> {
    let normalized = lesson_title.trim().to_lowercase();
    module
        .lessons
        .iter()
        .find(|lesson| lesson.title.trim().to_lowercase() == normalized)
        .map(|lesson| lesson.id.clone())
}

fn compact_tutor_messages(messages: Vec<LearningTutorMessage>) -> Vec<LearningTutorMessage> {
    messages
        .into_iter()
        .filter(|message| {
            (message.role == "learner" || message.role == "mentor")
                && !message.text.trim().is_empty()
        })
        .map(|message| LearningTutorMessage {
            id: clamp_text(message.id, 80),
            role: message.role,
            text: clamp_text(message.text, 900),
            why: message.why.map(|why| clamp_text(why, 500)),
            follow_up: message
                .follow_up
                .map(|follow_up| clamp_text(follow_up, 260)),
            module_id: message.module_id.map(|value| clamp_text(value, 120)),
            module_title: message.module_title.map(|value| clamp_text(value, 140)),
            lesson_id: message.lesson_id.map(|value| clamp_text(value, 120)),
            lesson_title: message.lesson_title.map(|value| clamp_text(value, 140)),
            created_at: message.created_at,
        })
        .rev()
        .take(30)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn compact_learning_module(mut module: LearningModule) -> Result<LearningModule, ApiError> {
    module.title = clamp_text(clean_learning_module_title(&module.title), 80);
    module.learner_profile = clamp_text(module.learner_profile, 180);
    module.outcome = clamp_text(module.outcome, 220);
    module.capstone_quest_prompt = clamp_text(module.capstone_quest_prompt, 360);

    if module.lessons.len() > 5 {
        module.lessons.truncate(5);
    }

    if module.lessons.is_empty() {
        return Err(ApiError::InvalidAiResponse);
    }

    for (index, lesson) in module.lessons.iter_mut().enumerate() {
        if lesson.id.trim().is_empty() {
            lesson.id = format!("lesson-{}", index + 1);
        }
        lesson.title = clamp_text(lesson.title.clone(), 80);
        lesson.why_it_matters = clamp_text(lesson.why_it_matters.clone(), 620);
        lesson.explanation = clamp_text(lesson.explanation.clone(), 7000);
        lesson.quest_bridge = clamp_text(lesson.quest_bridge.clone(), 280);
        if lesson.concepts.len() > 5 {
            lesson.concepts.truncate(5);
        }
        lesson.concepts = lesson
            .concepts
            .iter()
            .map(|concept| clamp_text(concept.clone(), 80))
            .filter(|concept| !concept.trim().is_empty())
            .collect();
        if lesson.concepts.is_empty() {
            lesson.concepts.push("protocol trust boundary".to_string());
        }
        lesson.submodules =
            compact_learning_submodules(std::mem::take(&mut lesson.submodules), &lesson.concepts);
        lesson.resources = compact_learning_resources(std::mem::take(&mut lesson.resources));
        refresh_lesson_validation_metadata(&module.title, lesson);

        if lesson.checkpoint.options.len() > 4 {
            lesson.checkpoint.options.truncate(4);
        }
        while lesson.checkpoint.options.len() < 4 {
            lesson.checkpoint.options.push(LearningOption {
                label: "Not enough information to defend the system.".to_string(),
                feedback: "A strong answer must name the trusted state and the failure case."
                    .to_string(),
            });
        }
        if lesson.checkpoint.correct_index >= lesson.checkpoint.options.len() {
            lesson.checkpoint.correct_index = 0;
        }
        lesson.checkpoint.question = clamp_text(lesson.checkpoint.question.clone(), 260);
        lesson.checkpoint.explanation = clamp_text(lesson.checkpoint.explanation.clone(), 900);
        lesson.checkpoint.follow_up_question =
            clamp_text(lesson.checkpoint.follow_up_question.clone(), 260);
        for option in &mut lesson.checkpoint.options {
            option.label = clamp_text(option.label.clone(), 220);
            option.feedback = clamp_text(option.feedback.clone(), 280);
        }

        if lesson.title.trim().is_empty()
            || lesson.explanation.trim().is_empty()
            || lesson.checkpoint.question.trim().is_empty()
        {
            return Err(ApiError::InvalidAiResponse);
        }
    }

    module.resources = compact_learning_resources(module.resources);
    if module.resources.is_empty() {
        module.resources = default_learning_resources();
    }

    Ok(module)
}

fn compact_learning_submodules(
    submodules: Vec<LearningSubmodule>,
    concepts: &[String],
) -> Vec<LearningSubmodule> {
    let mut compacted = submodules
        .into_iter()
        .filter(|submodule| !submodule.title.trim().is_empty())
        .take(5)
        .enumerate()
        .map(|(index, submodule)| LearningSubmodule {
            id: if submodule.id.trim().is_empty() {
                format!("submodule-{}", index + 1)
            } else {
                clamp_text(submodule.id, 80)
            },
            title: clamp_text(submodule.title, 90),
            summary: clamp_text(submodule.summary, 260),
            children: submodule
                .children
                .into_iter()
                .filter(|child| !child.title.trim().is_empty())
                .take(3)
                .enumerate()
                .map(|(child_index, child)| LearningSubmodule {
                    id: if child.id.trim().is_empty() {
                        format!("submodule-{}-{}", index + 1, child_index + 1)
                    } else {
                        clamp_text(child.id, 80)
                    },
                    title: clamp_text(child.title, 90),
                    summary: clamp_text(child.summary, 220),
                    children: Vec::new(),
                })
                .collect(),
        })
        .collect::<Vec<_>>();

    if compacted.is_empty() {
        compacted = concepts
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, concept)| LearningSubmodule {
                id: format!("submodule-{}", index + 1),
                title: clamp_text(concept.clone(), 90),
                summary: format!(
                    "Read this as a focused submodule: define {}, identify the trusted evidence, then design one denial case that proves generated code cannot fake it.",
                    concept
                ),
                children: Vec::new(),
            })
            .collect();
    }

    compacted
}

fn compact_learning_resources(resources: Vec<LearningResource>) -> Vec<LearningResource> {
    let mut compacted = resources
        .into_iter()
        .filter(|resource| resource.title.trim().len() > 1 && resource.url.starts_with("https://"))
        .map(|resource| LearningResource {
            title: clamp_text(resource.title, 80),
            url: clamp_text(resource.url, 160),
            reason: clamp_text(resource.reason, 180),
        })
        .take(5)
        .collect::<Vec<_>>();

    if compacted.is_empty() {
        compacted = default_learning_resources();
    }

    compacted
}

fn deserialize_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::Array(items) => items
            .into_iter()
            .filter_map(value_to_compact_text)
            .collect(),
        Value::String(text) => text
            .split(',')
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
        Value::Object(_) => value_to_compact_text(value).into_iter().collect(),
        _ => Vec::new(),
    })
}

fn value_to_compact_text(value: Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text),
        Value::Object(object) => {
            let mut gate_number: Option<String> = None;
            let mut parts: Vec<String> = Vec::new();
            for key in [
                "title",
                "name",
                "check",
                "description",
                "task",
                "evidence",
                "text",
                "summary",
            ] {
                if let Some(Value::String(text)) = object.get(key) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && !parts.iter().any(|part| part == trimmed) {
                        parts.push(trimmed.to_string());
                    }
                }
            }
            if let Some(value) = object.get("gate") {
                gate_number = match value {
                    Value::Number(number) => Some(number.to_string()),
                    Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
                    _ => None,
                };
            }
            for value in object.values() {
                if let Value::String(text) = value {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && !parts.iter().any(|part| part == trimmed) {
                        parts.push(trimmed.to_string());
                    }
                }
            }
            if parts.is_empty() {
                Some(Value::Object(object).to_string())
            } else if let Some(number) = gate_number {
                Some(format!("Gate {number}: {}", parts.join(" - ")))
            } else {
                Some(parts.join(" - "))
            }
        }
        other => Some(other.to_string()),
    }
}

fn deserialize_workbench_file_content<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(text) => Ok(text),
        Value::Object(mut object) => {
            for key in ["content", "code", "source", "text"] {
                if let Some(Value::String(text)) = object.remove(key) {
                    return Ok(text);
                }
            }
            Err(serde::de::Error::custom(
                "workbench file content object must include a string content, code, source, or text field",
            ))
        }
        _ => Err(serde::de::Error::custom(
            "workbench file content must be a string or code object",
        )),
    }
}

fn default_learning_resources() -> Vec<LearningResource> {
    vec![
        LearningResource {
            title: "CKB Docs".to_string(),
            url: "https://docs.nervos.org/".to_string(),
            reason: "Reference cells, scripts, witnesses, transactions, and token state.".to_string(),
        },
        LearningResource {
            title: "Fiber Network Repository".to_string(),
            url: "https://github.com/nervosnetwork/fiber".to_string(),
            reason: "Reference Fiber payment channels, invoices, PTLC-based security, routing, and node behavior.".to_string(),
        },
        LearningResource {
            title: "Zcash Documentation".to_string(),
            url: "https://zcash.readthedocs.io/".to_string(),
            reason: "Reference shielded payments, payment requests, addresses, viewing keys, memos, and privacy boundaries.".to_string(),
        },
        LearningResource {
            title: "ZIP-321 Payment Request Standard".to_string(),
            url: "https://zips.z.cash/zip-0321".to_string(),
            reason: "Reference payment request URI structure, recipient fields, amounts, memos, and interoperability constraints.".to_string(),
        },
        LearningResource {
            title: "Ethereum Developer Docs".to_string(),
            url: "https://ethereum.org/developers/docs/".to_string(),
            reason: "Reference Web3 wallets, accounts, transactions, smart contracts, nodes, and consensus fundamentals.".to_string(),
        },
        LearningResource {
            title: "Stacks Documentation".to_string(),
            url: "https://docs.stacks.co/".to_string(),
            reason: "Reference Stacks, Clarity, transactions, wallets, sBTC, and Bitcoin-secured app development.".to_string(),
        },
        LearningResource {
            title: "Golem Ecosystem Fund".to_string(),
            url: "https://golem.network/ecosystem".to_string(),
            reason: "Reference Golem ecosystem goals, fund fit, builder value, and decentralized compute growth priorities.".to_string(),
        },
        LearningResource {
            title: "Golem Docs".to_string(),
            url: "https://docs.golem.network/".to_string(),
            reason: "Reference Golem requestor/provider concepts, Yagna, SDKs, dApps, and decentralized compute workflows.".to_string(),
        },
        LearningResource {
            title: "Golem Quickstarts".to_string(),
            url: "https://docs.golem.network/docs/quickstarts".to_string(),
            reason: "Reference first-task paths for learners moving from concept to practical execution.".to_string(),
        },
        LearningResource {
            title: "Golem JS SDK".to_string(),
            url: "https://docs.golem.network/docs/creators/javascript".to_string(),
            reason: "Reference JavaScript SDK requestor workflow, task execution, package structure, and app integration.".to_string(),
        },
        LearningResource {
            title: "Golem JS Task Model".to_string(),
            url: "https://docs.golem.network/docs/creators/javascript/guides/task-model".to_string(),
            reason: "Reference task lifecycle, work definition, provider execution, result collection, and cleanup boundaries.".to_string(),
        },
        LearningResource {
            title: "Golem JS Executing Tasks".to_string(),
            url: "https://docs.golem.network/docs/creators/javascript/examples/executing-tasks".to_string(),
            reason: "Reference practical task execution examples and result-handling patterns.".to_string(),
        },
        LearningResource {
            title: "Golem Requestor / Provider Interaction".to_string(),
            url: "https://docs.golem.network/docs/creators/common/requestor-provider-interaction".to_string(),
            reason: "Reference agreements, requestor/provider separation, market negotiation, and compute execution boundaries.".to_string(),
        },
        LearningResource {
            title: "Golem Python Quickstart".to_string(),
            url: "https://docs.golem.network/docs/creators/python/quickstarts/run-first-task-on-golem".to_string(),
            reason: "Reference Python task execution flow for builders learning non-JavaScript workloads.".to_string(),
        },
        LearningResource {
            title: "Golem Python Application Fundamentals".to_string(),
            url: "https://docs.golem.network/docs/creators/python/guides/application-fundamentals".to_string(),
            reason: "Reference Python app structure, executor behavior, task payloads, and result collection.".to_string(),
        },
        LearningResource {
            title: "Ray on Golem".to_string(),
            url: "https://docs.golem.network/docs/creators/ray".to_string(),
            reason: "Reference distributed Python and Ray workload patterns on Golem.".to_string(),
        },
        LearningResource {
            title: "Ray on Golem Limitations".to_string(),
            url: "https://docs.golem.network/docs/creators/ray/supported-versions-and-other-limitations".to_string(),
            reason: "Reference practical limits so generated lessons do not overclaim Ray or AI workload readiness.".to_string(),
        },
        LearningResource {
            title: "Golem dApp Hello World".to_string(),
            url: "https://docs.golem.network/docs/creators/dapps/hello-world-dapp".to_string(),
            reason: "Reference the simplest deployable Golem dApp flow and service lifecycle.".to_string(),
        },
        LearningResource {
            title: "Creating Golem dApps".to_string(),
            url: "https://docs.golem.network/docs/creators/dapps/creating-golem-dapps".to_string(),
            reason: "Reference descriptors, images, services, manifests, and dApp deployment structure.".to_string(),
        },
        LearningResource {
            title: "Golem Provider Overview".to_string(),
            url: "https://docs.golem.network/docs/providers".to_string(),
            reason: "Reference provider onboarding, compute contribution, node operation, and provider-side assumptions.".to_string(),
        },
        LearningResource {
            title: "Golem Provider Architecture".to_string(),
            url: "https://docs.golem.network/docs/golem/overview/provider".to_string(),
            reason: "Reference provider architecture when explaining boundaries between requestor code and provider execution.".to_string(),
        },
        LearningResource {
            title: "STON.fi DEX Overview".to_string(),
            url: "https://docs.ston.fi/developer-section/dex/overview".to_string(),
            reason: "Reference STON.fi DEX integration roles, swap flow, liquidity, pools, and routing context.".to_string(),
        },
        LearningResource {
            title: "STON.fi DEX SDK Documentation".to_string(),
            url: "https://docs.ston.fi/developer-section/dex/sdk".to_string(),
            reason: "Reference STON.fi swap quote, route, router, transaction, pool, and SDK integration behavior.".to_string(),
        },
        LearningResource {
            title: "STON.fi DEX Smart Contracts".to_string(),
            url: "https://docs.ston.fi/developer-section/dex/smart-contracts".to_string(),
            reason: "Reference router, pool, LP account, vault, and contract-level integration boundaries.".to_string(),
        },
        LearningResource {
            title: "STON.fi REST API Documentation".to_string(),
            url: "https://docs.ston.fi/developer-section/dex/api".to_string(),
            reason: "Reference pool, jetton, quote, and referral-fee data used by STON.fi integrations.".to_string(),
        },
        LearningResource {
            title: "STON.fi Omniston Widget Overview".to_string(),
            url: "https://docs.ston.fi/developer-section/widget".to_string(),
            reason: "Reference the Omniston widget integration surface before selecting the full widget or SDK path.".to_string(),
        },
        LearningResource {
            title: "STON.fi Omniston Widget Guide".to_string(),
            url: "https://docs.ston.fi/developer-section/widget/widget".to_string(),
            reason: "Reference Omniston widget loading, TON Connect manifest use, default assets, and integration UX.".to_string(),
        },
        LearningResource {
            title: "STON.fi Omniston SDK".to_string(),
            url: "https://docs.ston.fi/developer-section/omniston/sdk".to_string(),
            reason: "Reference Omniston SDK integration when a builder needs deeper control than the widget.".to_string(),
        },
        LearningResource {
            title: "TON Connect Documentation".to_string(),
            url: "https://docs.ton.org/applications/ton-connect/overview".to_string(),
            reason: "Reference wallet connection, manifest boundaries, wallet approval, and app authorization on TON.".to_string(),
        },
        LearningResource {
            title: "TON Connect UI Reference".to_string(),
            url: "https://docs.ton.org/applications/ton-connect/api-reference/ui".to_string(),
            reason: "Reference TON Connect UI behavior, connector reuse, wallet state, and transaction send boundaries.".to_string(),
        },
        LearningResource {
            title: "TON Token Overview".to_string(),
            url: "https://docs.ton.org/contracts/standard/tokens/overview".to_string(),
            reason: "Reference TON token standards before reasoning about jetton assets in a swap UI.".to_string(),
        },
        LearningResource {
            title: "TON Jetton Processing".to_string(),
            url: "https://docs.ton.org/applications/payments/jettons".to_string(),
            reason: "Reference practical jetton processing, deposits, withdrawals, and application payment handling.".to_string(),
        },
        LearningResource {
            title: "TON Jetton Interface".to_string(),
            url: "https://docs.ton.org/contracts/standard/tokens/jettons/api".to_string(),
            reason: "Reference jetton interface methods and contract expectations for verification checks.".to_string(),
        },
        LearningResource {
            title: "TON Jetton Architecture".to_string(),
            url: "https://docs.ton.org/contracts/standard/tokens/jettons/how-it-works".to_string(),
            reason: "Reference jetton master contracts, wallet contracts, metadata risks, and token verification boundaries.".to_string(),
        },
    ]
}

fn default_learning_resources_for_focus(focus: &str) -> Vec<LearningResource> {
    let lower = focus.to_ascii_lowercase();
    let all = default_learning_resources();
    if lower.contains("golem")
        || lower.contains("yagna")
        || lower.contains("requestor")
        || lower.contains("provider")
        || lower.contains("ray on golem")
        || lower.contains("decentralized compute")
    {
        return all
            .into_iter()
            .filter(|resource| {
                let text = format!("{} {}", resource.title, resource.url).to_ascii_lowercase();
                text.contains("golem.network")
                    || text.contains("golem docs")
                    || text.contains("golem js")
                    || text.contains("golem python")
                    || text.contains("ray on golem")
                    || text.contains("requestor")
                    || text.contains("provider")
                    || text.contains("dapp")
            })
            .collect();
    }
    if lower.contains("ston")
        || lower.contains("omniston")
        || lower.contains("ton connect")
        || lower.contains("jetton")
        || lower.contains("ton / ston")
        || lower.contains("ton-stonfi")
    {
        return all
            .into_iter()
            .filter(|resource| {
                let text = format!("{} {}", resource.title, resource.url).to_ascii_lowercase();
                text.contains("ston.fi")
                    || text.contains("docs.ston")
                    || text.contains("ton connect")
                    || text.contains("docs.ton")
                    || text.contains("ton token")
                    || text.contains("jetton")
            })
            .collect();
    }
    if lower.contains("stacks")
        || lower.contains("clarity")
        || lower.contains("sbtc")
        || lower.contains("bns")
    {
        return all
            .into_iter()
            .filter(|resource| {
                let text = format!("{} {}", resource.title, resource.url).to_ascii_lowercase();
                text.contains("stacks") || text.contains("docs.stacks")
            })
            .collect();
    }
    if lower.contains("zcash") || lower.contains("zip-321") || lower.contains("shielded") {
        return all
            .into_iter()
            .filter(|resource| {
                let text = format!("{} {}", resource.title, resource.url).to_ascii_lowercase();
                text.contains("zcash") || text.contains("zip-321") || text.contains("zips.z.cash")
            })
            .collect();
    }
    if lower.contains("fiber") || lower.contains("ptlc") || lower.contains("channel") {
        return all
            .into_iter()
            .filter(|resource| {
                let text = format!("{} {}", resource.title, resource.url).to_ascii_lowercase();
                text.contains("fiber") || text.contains("nervos")
            })
            .collect();
    }
    if lower.contains("ckb") || lower.contains("cell") || lower.contains("script") {
        return all
            .into_iter()
            .filter(|resource| {
                let text = format!("{} {}", resource.title, resource.url).to_ascii_lowercase();
                text.contains("ckb") || text.contains("nervos")
            })
            .collect();
    }
    if lower.contains("basic") || lower.contains("web3") || lower.contains("blockchain") {
        return all
            .into_iter()
            .filter(|resource| {
                let text = format!("{} {}", resource.title, resource.url).to_ascii_lowercase();
                text.contains("ethereum") || text.contains("zcash") || text.contains("nervos")
            })
            .take(3)
            .collect();
    }

    all
}

fn clamp_text(value: String, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let mut output = trimmed.chars().take(max_chars).collect::<String>();
    output.push_str("...");
    output
}

fn infer_workbench_language(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "rs" => "rust",
        "tsx" | "jsx" => "tsx",
        "ts" | "js" => "typescript",
        "md" => "markdown",
        _ => "text",
    }
}

fn compact_file_content(content: &str, max_lines: usize) -> String {
    let lines = content.lines().collect::<Vec<_>>();
    if lines.len() <= max_lines {
        return content.to_string();
    }

    let mut compacted = lines[..max_lines].join("\n");
    compacted.push_str("\n// VibeQuest clipped this file to keep the browser workbench fast.\n");
    compacted
}

fn parse_openai_json_response<T>(body: &str) -> Result<T, ApiError>
where
    T: for<'de> Deserialize<'de>,
{
    let response =
        serde_json::from_str::<OpenAiResponse>(body).map_err(|_| ApiError::InvalidAiResponse)?;
    let text = openai_response_text(response)?;
    let trimmed = text.trim();
    let json = extract_json_object(trimmed).unwrap_or(trimmed);

    serde_json::from_str::<T>(json).map_err(|error| {
        warn!(
            error = %error,
            extracted = %clamp_text(json.to_string(), 900),
            "Extracted OpenAI text did not deserialize into expected schema"
        );
        ApiError::InvalidAiResponse
    })
}

fn openai_response_text(response: OpenAiResponse) -> Result<String, ApiError> {
    if let Some(output_text) = response.output_text {
        return Ok(output_text);
    }

    let text = response
        .output
        .unwrap_or_default()
        .into_iter()
        .flat_map(|item| item.content.unwrap_or_default())
        .filter_map(
            |content| match (content.content_type.as_deref(), content.text) {
                (Some("output_text") | Some("text") | None, Some(text)) => Some(text),
                _ => None,
            },
        )
        .collect::<Vec<_>>()
        .join("\n");

    if text.trim().is_empty() {
        return Err(ApiError::InvalidAiResponse);
    }

    Ok(text)
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, character) in text[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        match character {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&text[start..start + offset + character.len_utf8()]);
                }
            }
            _ => {}
        }
    }

    None
}

fn build_learning_module_from_compact_ai(
    request: &GenerateLearningModuleRequest,
    compact: AiLearningModuleCompact,
) -> Result<LearningModule, ApiError> {
    if compact.l.is_empty() {
        return Err(ApiError::InvalidAiResponse);
    }

    let interests = request
        .interests
        .iter()
        .map(|interest| interest.trim())
        .filter(|interest| !interest.is_empty())
        .take(4)
        .collect::<Vec<_>>();
    let focus = if interests.is_empty() {
        "CKB/Fiber".to_string()
    } else {
        interests.join(" + ")
    };
    let background = if request.background.trim().is_empty() {
        "learner".to_string()
    } else {
        request.background.trim().to_string()
    };

    let mut prior_lessons = Vec::with_capacity(5);
    let mut compact_lessons = Vec::with_capacity(5);
    for (index, lesson) in compact.l.into_iter().take(5).enumerate() {
        validate_ai_learning_lesson_compact_for_request_with_context(
            request,
            &lesson,
            index,
            &prior_lessons,
        )?;
        prior_lessons.push(prior_learning_lesson_from_compact(&lesson));
        compact_lessons.push(lesson);
    }

    let lessons = compact_lessons
        .into_iter()
        .enumerate()
        .map(|(index, lesson)| {
            compact_ai_lesson_to_learning_lesson(index, &background, &focus, request, lesson)
        })
        .collect::<Result<Vec<_>, _>>()?;

    compact_learning_module(LearningModule {
        title: non_empty_or(compact.t, &learning_module_title(request)),
        learner_profile: learning_module_profile(request),
        outcome: learning_module_outcome(request),
        lessons,
        capstone_quest_prompt: learning_module_capstone_prompt(request),
        resources: default_learning_resources_for_focus(&learning_focus_label(request)),
    })
}

fn normalize_ai_learning_lesson_for_request(
    request: &GenerateLearningModuleRequest,
    lesson_index: usize,
    lesson: &mut AiLearningLessonCompact,
) {
    lesson.t = lesson.t.trim().to_string();
    lesson.e = lesson.e.trim().to_string();
    lesson.s = normalize_code_lens_edit_markers(&lesson.s);
    lesson.w = lesson.w.trim().to_string();
    lesson.j = lesson.j.trim().to_string();
    lesson.f = lesson.f.trim().to_string();
    lesson.q = lesson.q.trim().to_string();
    lesson.a = lesson.a.trim().to_string();

    normalize_learning_side_field(
        &mut lesson.w,
        35,
        "This matters because learners must separate source-backed protocol evidence from interface state before accepting generated code as safe.",
    );
    normalize_learning_side_field(
        &mut lesson.j,
        22,
        "The practice artifact should include one verifier map and one denial test that mutates the trusted field before completion.",
    );

    if learning_ecosystem_id(request) == "ton-stonfi" {
        normalize_ton_stonfi_ai_learning_lesson(request, lesson_index, lesson);
    }
    if learning_ecosystem_id(request) == "golem" {
        normalize_golem_ai_learning_lesson(request, lesson_index, lesson);
    }
}

fn normalize_code_lens_edit_markers(value: &str) -> String {
    value
        .trim()
        .replace("TODO:", "Learner edit:")
        .replace("TODO", "Learner edit")
        .replace("todo:", "Learner edit:")
        .replace("todo", "Learner edit")
}

fn normalize_learning_side_field(value: &mut String, minimum_words: usize, addition: &str) {
    if value.split_whitespace().count() >= minimum_words {
        return;
    }
    append_learning_sentence(value, addition);
}

fn normalize_ton_stonfi_ai_learning_lesson(
    request: &GenerateLearningModuleRequest,
    lesson_index: usize,
    lesson: &mut AiLearningLessonCompact,
) {
    let source_sentence = "Accuracy check: verify STON.fi quote and route behavior against official STON.fi documentation at docs.ston.fi, Omniston widget docs, TON Connect documentation, and TON Jetton documentation at docs.ton.org before trusting generated swap code.";
    if !lesson_has_official_source_anchor("ton-stonfi", &learning_lesson_full_text(lesson)) {
        append_learning_sentence(&mut lesson.e, source_sentence);
    }

    if !lesson_has_accuracy_nuance(&learning_lesson_full_text(lesson)) {
        append_learning_sentence(
            &mut lesson.e,
            "Denial test: reject stale quotes, mismatched jetton master addresses, missing min-out values, unsafe slippage, and pending transaction state instead of treating widget display or API quote text as settlement evidence.",
        );
    }

    if !lesson_mentions_role_specific_terms(
        "ton-stonfi",
        lesson_index,
        &learning_lesson_full_text(lesson).to_ascii_lowercase(),
    ) {
        append_learning_sentence(&mut lesson.e, ton_stonfi_role_sentence(lesson_index));
    }

    if lesson_index >= 4 {
        append_learning_sentence(&mut lesson.e, ton_stonfi_final_lab_denial_checklist());
        append_learning_sentence(
            &mut lesson.j,
            "Final lab artifact: a reviewed safe-swap proof map plus denial tests for fake jettons, stale quotes, unsafe min-out, wallet rejection, manifest mismatch, duplicate connector state, hidden fees, pending-as-success, and REST API settlement assumptions.",
        );
    }

    if wants_code_snippets_for_request(request) {
        lesson.s = ton_stonfi_curated_code_lens(lesson_index).to_string();
    }
}

fn ton_stonfi_final_lab_denial_checklist() -> &'static str {
    "Final lab denial checklist: fake jetton master address, misleading token metadata, changed token pair after quote, stale quote timestamp, missing min-out, min-out set too low, wallet disconnected before submission, rejected wallet approval, pending transaction treated as success, wrong TON Connect manifest domain, duplicate TON Connect connector state, missing referral-fee disclosure, and REST API response treated as settlement proof."
}

fn ton_stonfi_curated_code_lens(lesson_index: usize) -> &'static str {
    match lesson_index.min(4) {
        0 => {
            r#"const manifestUrl = new URL('/tonconnect-manifest.json', window.location.origin).toString();
export function canStartStonfiSwap(wallet) {
  if (!wallet?.account?.address) return { ok: false, reason: 'wallet-disconnected' };
  if (!manifestUrl.startsWith(window.location.origin)) return { ok: false, reason: 'manifest-domain-mismatch' };
  return { ok: true, manifestUrl, walletAddress: wallet.account.address };
}"#
        }
        1 => {
            r#"export function validateStonfiQuote({ quote, selectedRoute, nowMs }) {
  const maxAgeMs = 30_000;
  if (!quote?.createdAtMs || nowMs - quote.createdAtMs > maxAgeMs) throw new Error('stale-quote');
  if (quote.routeId !== selectedRoute.routeId) throw new Error('route-mismatch');
  if (!quote.transactionPayload) throw new Error('missing-swap-transaction');
  // Learner edit: lower maxAgeMs only if the UI refreshes quotes more often.
  return { routeId: quote.routeId, transactionPayload: quote.transactionPayload };
}"#
        }
        2 => {
            r#"export function assertJettonAllowed({ jetton, allowlist }) {
  const master = jetton?.masterAddress?.toLowerCase();
  if (!master || !allowlist.has(master)) throw new Error('untrusted-jetton-master');
  if (jetton.walletContract && !jetton.walletContract.startsWith('EQ')) throw new Error('invalid-jetton-wallet-contract');
  if (jetton.symbolOnlyMatch) throw new Error('metadata-is-not-identity');
  return master;
}"#
        }
        3 => {
            r#"export function enforceSwapIntent({ quote, minOut, slippageBps, referralFeeBps }) {
  if (!Number.isFinite(minOut) || minOut <= 0) throw new Error('missing-min-out');
  if (slippageBps > 100) throw new Error('slippage-too-wide');
  if (referralFeeBps > 0 && !quote.referralDisclosureShown) throw new Error('fee-not-disclosed');
  if (quote.expectedOut < minOut) throw new Error('min-out-violated');
  return { minOut, slippageBps, referralFeeBps };
}"#
        }
        _ => {
            r#"export function finalStonfiLabChecks(state) {
  const failures = [];
  if (state.fakeJettonMaster) failures.push('fake-jetton-master');
  if (state.misleadingTokenMetadata) failures.push('misleading-token-metadata');
  if (state.changedTokenPairAfterQuote) failures.push('changed-token-pair');
  if (state.staleQuoteTimestamp) failures.push('stale-quote');
  if (!state.minOut) failures.push('missing-min-out');
  if (state.minOutTooLow) failures.push('unsafe-min-out');
  if (state.walletDisconnected) failures.push('wallet-disconnected');
  if (state.rejectedWalletApproval) failures.push('wallet-rejection');
  if (state.pendingTransactionTreatedAsSuccess) failures.push('pending-transaction-not-success');
  if (state.wrongTonConnectManifestDomain) failures.push('wrong-ton-connect-manifest-domain');
  if (state.duplicateTonConnectConnectorState) failures.push('duplicate-ton-connect-connector-state');
  if (!state.referralFeeDisclosed) failures.push('referral-fee-disclosure-missing');
  if (state.restApiResponseUsedAsSettlementProof) failures.push('rest-api-response-not-settlement-proof');
  return { pass: failures.length === 0, failures };
}"#
        }
    }
}

fn ton_stonfi_role_sentence(lesson_index: usize) -> &'static str {
    match lesson_index.min(4) {
        0 => {
            "Module focus: a safe TON / STON.fi swap separates wallet connection, swap quote, route, and confirmed transaction evidence before any learner treats completion as real."
        }
        1 => {
            "Module focus: the STON.fi SDK quote, router route, and swap transaction payload must be checked before wallet approval."
        }
        2 => {
            "Module focus: jetton master, jetton wallet contract, metadata, and allowlist checks prevent fake-token confusion."
        }
        3 => {
            "Module focus: slippage, min-out, referral fee, and stale quote denial define whether the integration respects user intent."
        }
        _ => {
            "Module focus: the final quest ties TON Connect, STON.fi route evidence, transaction state, and denial tests into one reviewable artifact."
        }
    }
}

fn normalize_golem_ai_learning_lesson(
    request: &GenerateLearningModuleRequest,
    lesson_index: usize,
    lesson: &mut AiLearningLessonCompact,
) {
    let source_sentence = "Accuracy check: verify the Golem compute workflow against official Golem docs at docs.golem.network, including requestor/provider interaction, JS SDK, Python/Ray, dApp deployment, provider docs, and documented Ray limitations before trusting generated compute code.";
    if !lesson_has_official_source_anchor("golem", &learning_lesson_full_text(lesson)) {
        append_learning_sentence(&mut lesson.e, source_sentence);
    }

    if !lesson_has_accuracy_nuance(&learning_lesson_full_text(lesson)) {
        append_learning_sentence(
            &mut lesson.e,
            "Failure-case test: reject provider unavailable states, task timeout, missing result, corrupted result, wrong GVMI image or runtime, agreement mismatch, budget exceeded, Yagna disconnect, and Ray limitation assumptions instead of treating provider output as automatically correct.",
        );
    }

    if !lesson_mentions_role_specific_terms(
        "golem",
        lesson_index,
        &learning_lesson_full_text(lesson).to_ascii_lowercase(),
    ) {
        append_learning_sentence(&mut lesson.e, golem_role_sentence(lesson_index));
    }

    if lesson_index >= 4 {
        append_learning_sentence(&mut lesson.e, golem_final_lab_failure_checklist());
        append_learning_sentence(
            &mut lesson.j,
            "Final compute lab artifact: a Golem execution plan plus failure matrix covering requestor/provider boundaries, Yagna setup, agreement and budget checks, task execution, result validation, provider failure, timeout, wrong image, unsupported Ray path, and cleanup.",
        );
    }

    if wants_code_snippets_for_request(request) {
        lesson.s = golem_curated_code_lens(lesson_index).to_string();
    }
}

fn golem_final_lab_failure_checklist() -> &'static str {
    "Final Golem compute lab failure checklist: provider unavailable, provider timeout, failed task execution, missing result, corrupted result, wrong GVMI image, wrong runtime version, agreement mismatch, budget exceeded, Yagna disconnected, network failure, Ray unsupported-version limitation, and provider output treated as automatically trusted."
}

fn golem_curated_code_lens(lesson_index: usize) -> &'static str {
    match lesson_index.min(4) {
        0 => {
            r#"export function describeGolemBoundary({ requestor, provider, yagna }) {
  if (!requestor?.appKey) throw new Error('missing-requestor-app-key');
  if (!yagna?.running) throw new Error('yagna-not-running');
  // Learner edit: add the provider capabilities this workload actually needs.
  return { requestorId: requestor.id, providerPool: provider?.market ?? 'open-market' };
}"#
        }
        1 => {
            r#"export function validateGolemTaskResult({ task, result, provider }) {
  if (!task?.command) throw new Error('missing-task-command');
  if (!provider?.id) throw new Error('missing-provider');
  if (!result?.stdout && !result?.artifactUrl) throw new Error('missing-result');
  // Learner edit: add a checksum or semantic validator for the expected output.
  return { taskId: task.id, providerId: provider.id, outputReady: true };
}"#
        }
        2 => {
            r#"export function chooseGolemPythonPath({ workload, ray }) {
  if (workload.requiresSharedGpu) throw new Error('unsupported-capability-claim');
  if (ray?.enabled && !ray.supportedVersion) throw new Error('ray-version-not-supported');
  // Learner edit: split the workload only when tasks can be independently verified.
  return ray?.enabled ? 'ray-on-golem' : 'python-sdk-task-execution';
}"#
        }
        3 => {
            r#"export function validateGolemDappManifest({ descriptor, image, service }) {
  if (!descriptor?.services?.length) throw new Error('missing-dapp-services');
  if (!image?.gvmiHash) throw new Error('missing-gvmi-image');
  if (!service?.healthcheck) throw new Error('missing-service-healthcheck');
  // Learner edit: bind exposed ports and proxy assumptions to the actual service.
  return { serviceCount: descriptor.services.length, gvmiHash: image.gvmiHash };
}"#
        }
        _ => {
            r#"export function finalGolemComputeQuestChecks(state) {
  const failures = [];
  if (!state.requestorAppKey) failures.push('missing-requestor-app-key');
  if (!state.yagnaRunning) failures.push('yagna-disconnected');
  if (!state.providerSelected) failures.push('provider-unavailable');
  if (state.agreementMismatch) failures.push('agreement-mismatch');
  if (state.budgetExceeded) failures.push('budget-exceeded');
  if (state.taskTimeout) failures.push('task-timeout');
  if (state.wrongGvmiImage) failures.push('wrong-gvmi-image');
  if (state.unsupportedRayVersion) failures.push('ray-limitation');
  if (!state.resultValidated) failures.push('unverified-provider-output');
  // Learner edit: add one workload-specific semantic result check.
  return { pass: failures.length === 0, failures };
}"#
        }
    }
}

fn golem_role_sentence(lesson_index: usize) -> &'static str {
    match lesson_index.min(4) {
        0 => {
            "Module focus: a safe Golem learner separates requestor intent, Yagna coordination, provider execution, agreement/payment boundaries, and result validation before trusting a decentralized compute job."
        }
        1 => {
            "Module focus: the JS SDK path must define the task, select provider capacity, execute the workload, collect outputs, validate results, and clean up the Golem allocation."
        }
        2 => {
            "Module focus: Python and Ray workloads need explicit supported-version checks, split-workload reasoning, result verification, and fallback behavior when Ray is not the right execution path."
        }
        3 => {
            "Module focus: Golem dApp deployment ties descriptor, GVMI image, service lifecycle, logs, proxies, and health checks into one reviewable compute service."
        }
        _ => {
            "Module focus: the final Golem quest proves requestor/provider boundaries, Yagna setup, agreement and budget checks, task lifecycle, result validation, and failure-state denial tests."
        }
    }
}

fn append_learning_sentence(value: &mut String, sentence: &str) {
    let normalized_value = value.to_ascii_lowercase();
    let normalized_sentence = sentence.to_ascii_lowercase();
    if normalized_value.contains(&normalized_sentence) {
        return;
    }
    if !value.trim().is_empty() && !value.chars().last().is_some_and(char::is_whitespace) {
        value.push(' ');
    }
    value.push_str(sentence);
}

fn ai_learning_lesson_validation_failures(
    request: &GenerateLearningModuleRequest,
    lesson: &AiLearningLessonCompact,
    lesson_index: usize,
    prior_lessons: &[PriorLearningLesson],
) -> Vec<String> {
    let mut failures = ai_learning_lesson_basic_validation_failures(lesson);
    let ecosystem_id = learning_ecosystem_id(request);
    let combined = learning_lesson_full_text(lesson).to_ascii_lowercase();

    if !lesson_mentions_required_ecosystem_terms(&ecosystem_id, &combined) {
        failures.push(format!(
            "missing required ecosystem term for {ecosystem_id}"
        ));
    }
    if !lesson_has_official_source_anchor(&ecosystem_id, &combined) {
        failures.push(format!("missing official source anchor for {ecosystem_id}"));
    }
    if contains_unrequested_ecosystem_leakage(&ecosystem_id, request, &combined) {
        failures.push(format!(
            "contains unrequested cross-ecosystem leakage for {ecosystem_id}"
        ));
    }
    if !lesson_mentions_role_specific_terms(&ecosystem_id, lesson_index, &combined) {
        failures.push(format!(
            "missing lesson {} role terms for {ecosystem_id}: {}",
            lesson_index + 1,
            role_specific_terms(&ecosystem_id, lesson_index).join(" | ")
        ));
    }
    if validate_no_prior_lesson_redundancy(lesson, prior_lessons).is_err() {
        failures
            .push("repeats a prior lesson title, checkpoint, code lens, or body shape".to_string());
    }
    if failures.is_empty() {
        failures.push("unknown validation failure".to_string());
    }
    failures
}

fn ai_learning_lesson_basic_validation_failures(lesson: &AiLearningLessonCompact) -> Vec<String> {
    let wrong_answer_count = lesson
        .b
        .iter()
        .filter(|label| !label.trim().is_empty())
        .count();
    let wrong_feedback_count = lesson
        .bf
        .iter()
        .filter(|feedback| !feedback.trim().is_empty())
        .count();
    let explainer_words = lesson.e.split_whitespace().count();
    let why_words = lesson.w.split_whitespace().count();
    let bridge_words = lesson.j.split_whitespace().count();
    let combined_prose = learning_lesson_prose(lesson);
    let combined_with_code = learning_lesson_full_text(lesson);

    let mut failures = Vec::new();
    if lesson.t.trim().is_empty() {
        failures.push("missing lesson title".to_string());
    }
    if explainer_words < 500 {
        failures.push(format!(
            "lesson explainer too short: {explainer_words}/500 words"
        ));
    }
    if lesson.s.trim().is_empty() {
        failures.push("missing code lens".to_string());
    }
    if why_words < 35 {
        failures.push(format!("why-it-matters too short: {why_words}/35 words"));
    }
    if bridge_words < 22 {
        failures.push(format!("quest bridge too short: {bridge_words}/22 words"));
    }
    if lesson.f.trim().is_empty() {
        failures.push("missing follow-up question".to_string());
    }
    if lesson.q.trim().is_empty() {
        failures.push("missing checkpoint question".to_string());
    }
    if generic_learning_checkpoint_question(&lesson.q) {
        failures.push("generic checkpoint question".to_string());
    }
    if contains_placeholder_learning_text(&combined_prose) {
        failures.push("placeholder or generic AI text detected".to_string());
    }
    if !lesson_has_official_source_anchor("generic", &combined_with_code) {
        failures.push("missing generic official source anchor".to_string());
    }
    if !lesson_has_accuracy_nuance(&combined_with_code) {
        failures.push("missing accuracy nuance and denial/failure terms".to_string());
    }
    if lesson.a.trim().is_empty() {
        failures.push("missing correct answer".to_string());
    }
    if wrong_answer_count != 3 {
        failures.push(format!(
            "wrong answer count is {wrong_answer_count}, expected 3"
        ));
    }
    if wrong_feedback_count != 3 {
        failures.push(format!(
            "wrong feedback count is {wrong_feedback_count}, expected 3"
        ));
    }
    failures
}

fn validate_ai_learning_lesson_compact(lesson: &AiLearningLessonCompact) -> Result<(), ApiError> {
    let wrong_answer_count = lesson
        .b
        .iter()
        .filter(|label| !label.trim().is_empty())
        .count();
    let wrong_feedback_count = lesson
        .bf
        .iter()
        .filter(|feedback| !feedback.trim().is_empty())
        .count();

    let explainer_words = lesson.e.split_whitespace().count();
    let why_words = lesson.w.split_whitespace().count();
    let bridge_words = lesson.j.split_whitespace().count();

    let combined_prose = learning_lesson_prose(lesson);
    let combined_with_code = learning_lesson_full_text(lesson);

    if lesson.t.trim().is_empty()
        || explainer_words < 500
        || lesson.s.trim().is_empty()
        || why_words < 35
        || bridge_words < 22
        || lesson.f.trim().is_empty()
        || lesson.q.trim().is_empty()
        || generic_learning_checkpoint_question(&lesson.q)
        || contains_placeholder_learning_text(&combined_prose)
        || !lesson_has_official_source_anchor("generic", &combined_with_code)
        || !lesson_has_accuracy_nuance(&combined_with_code)
        || lesson.a.trim().is_empty()
        || wrong_answer_count != 3
        || wrong_feedback_count != 3
    {
        return Err(ApiError::InvalidAiResponse);
    }

    Ok(())
}

fn validate_ai_learning_lesson_compact_for_request(
    request: &GenerateLearningModuleRequest,
    lesson: &AiLearningLessonCompact,
) -> Result<(), ApiError> {
    validate_ai_learning_lesson_compact_for_request_without_role(request, lesson)
}

fn validate_ai_learning_lesson_compact_for_request_without_role(
    request: &GenerateLearningModuleRequest,
    lesson: &AiLearningLessonCompact,
) -> Result<(), ApiError> {
    validate_ai_learning_lesson_compact(lesson)?;

    let ecosystem_id = learning_ecosystem_id(request);
    let combined = learning_lesson_full_text(lesson).to_ascii_lowercase();

    if !lesson_mentions_required_ecosystem_terms(&ecosystem_id, &combined) {
        return Err(ApiError::InvalidAiResponse);
    }
    if !lesson_has_official_source_anchor(&ecosystem_id, &combined) {
        return Err(ApiError::InvalidAiResponse);
    }
    if contains_unrequested_ecosystem_leakage(&ecosystem_id, request, &combined) {
        return Err(ApiError::InvalidAiResponse);
    }

    Ok(())
}

fn validate_ai_learning_lesson_compact_for_request_with_context(
    request: &GenerateLearningModuleRequest,
    lesson: &AiLearningLessonCompact,
    lesson_index: usize,
    prior_lessons: &[PriorLearningLesson],
) -> Result<(), ApiError> {
    validate_ai_learning_lesson_compact_for_request_without_role(request, lesson)?;

    let ecosystem_id = learning_ecosystem_id(request);
    let combined = learning_lesson_full_text(lesson).to_ascii_lowercase();

    if !lesson_mentions_role_specific_terms(&ecosystem_id, lesson_index, &combined) {
        return Err(ApiError::InvalidAiResponse);
    }
    validate_no_prior_lesson_redundancy(lesson, prior_lessons)?;

    Ok(())
}

fn learning_lesson_prose(lesson: &AiLearningLessonCompact) -> String {
    [
        lesson.t.as_str(),
        lesson.e.as_str(),
        lesson.w.as_str(),
        lesson.j.as_str(),
        lesson.f.as_str(),
        lesson.q.as_str(),
        lesson.a.as_str(),
    ]
    .join("\n")
}

fn learning_lesson_full_text(lesson: &AiLearningLessonCompact) -> String {
    [
        lesson.t.as_str(),
        lesson.e.as_str(),
        lesson.s.as_str(),
        lesson.w.as_str(),
        lesson.j.as_str(),
        lesson.f.as_str(),
        lesson.q.as_str(),
        lesson.a.as_str(),
    ]
    .join("\n")
}

fn prior_learning_lesson_from_compact(lesson: &AiLearningLessonCompact) -> PriorLearningLesson {
    PriorLearningLesson {
        title: clamp_text(lesson.t.clone(), 160),
        checkpoint_question: clamp_text(lesson.q.clone(), 260),
        summary: clamp_text(lesson.e.clone(), 1600),
        code_lens: clamp_text(lesson.s.clone(), 700),
    }
}

fn lesson_has_official_source_anchor(ecosystem_id: &str, text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let source_terms = official_source_terms_for_ecosystem(ecosystem_id);
    let has_source_name = source_terms.iter().any(|term| lower.contains(term));
    let source_posture_count = [
        "official",
        "docs",
        "documentation",
        "standard",
        "spec",
        "repository",
        "source pack",
        "further study",
        "verify",
        "confirm",
    ]
    .iter()
    .filter(|term| lower.contains(**term))
    .count();

    has_source_name && source_posture_count >= 2
}

fn official_source_terms_for_ecosystem(ecosystem_id: &str) -> &'static [&'static str] {
    match ecosystem_id {
        "stacks" => &[
            "official stacks",
            "stacks documentation",
            "docs.stacks.co",
            "clarity documentation",
            "sbtc documentation",
            "bns documentation",
        ],
        "golem" => &[
            "official golem",
            "golem docs",
            "docs.golem.network",
            "golem documentation",
            "golem js sdk",
            "js sdk",
            "golem python",
            "ray on golem",
            "golem dapp",
            "requestor/provider",
            "provider overview",
        ],
        "ton-stonfi" => &[
            "official ston.fi",
            "ston.fi documentation",
            "docs.ston.fi",
            "ston.fi dex sdk",
            "omniston widget",
            "ton connect documentation",
            "ton jetton documentation",
            "docs.ton.org",
        ],
        "zcash" => &[
            "official zcash",
            "zcash documentation",
            "zcash docs",
            "zip-321",
            "zip321",
            "zips.z.cash",
            "zip-316",
        ],
        "ckb" => &[
            "official ckb",
            "ckb docs",
            "docs.nervos.org",
            "nervos rfc",
            "ckb documentation",
        ],
        "fiber" => &[
            "fiber network repository",
            "github.com/nervosnetwork/fiber",
            "fiber repository",
            "ckb docs",
            "official ckb",
        ],
        "ckb-fiber" => &[
            "official ckb",
            "ckb docs",
            "fiber network repository",
            "fiber repository",
            "joyid",
        ],
        "basics" => &[
            "ethereum developer docs",
            "mdn web docs",
            "bitcoin developer reference",
            "official docs",
            "official documentation",
        ],
        _ => &[
            "official docs",
            "official documentation",
            "official ckb",
            "ckb docs",
            "fiber network repository",
            "zcash documentation",
            "official zcash",
            "stacks documentation",
            "official stacks",
            "ston.fi documentation",
            "docs.ston.fi",
            "ton connect documentation",
            "docs.ton.org",
            "golem docs",
            "official golem",
            "docs.golem.network",
            "golem js sdk",
            "ray on golem",
            "ethereum developer docs",
            "mdn web docs",
            "source pack",
            "standard",
            "spec",
        ],
    }
}

fn lesson_has_accuracy_nuance(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let verification_terms = [
        "verify",
        "validate",
        "confirm",
        "evidence",
        "source",
        "docs",
        "spec",
        "standard",
        "repository",
        "proof boundary",
        "trust boundary",
    ];
    let failure_terms = [
        "failure mode",
        "denial test",
        "reject",
        "unsafe",
        "wrong",
        "stale",
        "replay",
        "mismatch",
        "tamper",
        "attacker",
        "leak",
        "malformed",
    ];
    let nuance_terms = [
        "does not automatically",
        "does not",
        "not automatically",
        "not as",
        "not interchangeable",
        "do not",
        "should not",
        "must not",
        "cannot",
        "unless",
        "only if",
        "before",
        "however",
        "but",
        "separate",
        "instead of",
    ];

    count_terms(&lower, &verification_terms) >= 2
        && count_terms(&lower, &failure_terms) >= 2
        && count_terms(&lower, &nuance_terms) >= 2
}

fn count_terms(text: &str, terms: &[&str]) -> usize {
    terms.iter().filter(|term| text.contains(**term)).count()
}

fn lesson_mentions_role_specific_terms(
    ecosystem_id: &str,
    lesson_index: usize,
    text: &str,
) -> bool {
    let terms = role_specific_terms(ecosystem_id, lesson_index);
    if terms.is_empty() {
        return true;
    }
    let required_hits = if terms.len() >= 5 { 2 } else { 1 };
    count_terms(text, terms) >= required_hits
}

fn role_specific_terms(ecosystem_id: &str, lesson_index: usize) -> &'static [&'static str] {
    match (ecosystem_id, lesson_index.min(4)) {
        ("zcash", 0) => &["shielded", "checkout", "privacy", "address", "payment"],
        ("zcash", 1) => &[
            "zip-321",
            "zip321",
            "payment request",
            "recipient",
            "amount",
            "network",
        ],
        ("zcash", 2) => &["viewing key", "memo", "disclosure", "privacy", "address"],
        ("zcash", 3) => &[
            "denial",
            "malformed",
            "replay",
            "wrong-network",
            "wrong network",
            "transparent",
            "unsafe",
        ],
        ("zcash", 4) => &[
            "quest",
            "verifier",
            "denial test",
            "payment evidence",
            "completion",
        ],
        ("stacks", 0) => &[
            "bitcoin",
            "proof of transfer",
            "stacks block",
            "settlement",
            "transaction",
        ],
        ("stacks", 1) => &[
            "clarity",
            "public function",
            "principal",
            "post-condition",
            "map",
        ],
        ("stacks", 2) => &[
            "wallet",
            "signature",
            "transaction",
            "authorization",
            "frontend",
        ],
        ("stacks", 3) => &["sbtc", "bns", "bitcoin-backed", "name", "identity"],
        ("stacks", 4) => &["quest", "clarity", "denial test", "authorization", "proof"],
        ("ton-stonfi", 0) => &["ton", "ston.fi", "wallet", "swap", "transaction"],
        ("ton-stonfi", 1) => &["quote", "route", "sdk", "router", "swap"],
        ("ton-stonfi", 2) => &[
            "jetton",
            "master",
            "wallet contract",
            "metadata",
            "allowlist",
        ],
        ("ton-stonfi", 3) => &["slippage", "min-out", "referral", "fee", "stale"],
        ("ton-stonfi", 4) => &[
            "quest",
            "ton connect",
            "denial test",
            "transaction state",
            "ston.fi",
        ],
        ("golem", 0) => &["requestor", "provider", "yagna", "agreement", "payment"],
        ("golem", 1) => &["js sdk", "javascript", "task", "result", "cleanup"],
        ("golem", 2) => &[
            "python",
            "ray",
            "supported version",
            "limitation",
            "workload",
        ],
        ("golem", 3) => &["dapp", "gvmi", "descriptor", "service", "lifecycle"],
        ("golem", 4) => &["quest", "requestor", "provider", "failure", "result"],
        ("ckb", 0) => &["cell", "live cell", "capacity", "state"],
        ("ckb", 1) => &["outpoint", "input", "output", "transaction"],
        ("ckb", 2) => &["script", "witness", "lock", "type"],
        ("ckb", 3) => &["denial", "stale", "copied", "replay", "fake"],
        ("ckb", 4) => &["quest", "verifier", "evidence", "denial test"],
        ("fiber", 0) => &["channel", "settlement", "payment", "ckb"],
        ("fiber", 1) => &["invoice", "ptlc", "preimage", "route", "channel state"],
        ("fiber", 2) => &["receipt", "paid", "replay", "access"],
        ("fiber", 3) => &["amount", "balance", "payout", "integrity"],
        ("fiber", 4) => &["quest", "verifier", "denial test", "payment"],
        ("basics", 0) => &["blockchain", "block", "shared history", "node"],
        ("basics", 1) => &[
            "wallet",
            "address",
            "recovery phrase",
            "signature",
            "private key",
        ],
        ("basics", 2) => &["transaction", "fee", "confirmation", "explorer", "mempool"],
        ("basics", 3) => &["connect wallet", "approve", "network", "phishing", "prompt"],
        ("basics", 4) => &["quest", "denial test", "wallet", "transaction", "safety"],
        ("ckb-fiber", 0) => &["ckb", "fiber", "cell", "channel", "evidence"],
        ("ckb-fiber", 1) => &["outpoint", "invoice", "transaction", "payment"],
        ("ckb-fiber", 2) => &["script", "witness", "signature", "joyid"],
        ("ckb-fiber", 3) => &["denial", "stale", "replay", "mismatch"],
        ("ckb-fiber", 4) => &["quest", "verifier", "denial test", "proof"],
        _ => &[],
    }
}

fn validate_no_prior_lesson_redundancy(
    lesson: &AiLearningLessonCompact,
    prior_lessons: &[PriorLearningLesson],
) -> Result<(), ApiError> {
    if prior_lessons.is_empty() {
        return Ok(());
    }

    let new_title = normalized_fingerprint(&lesson.t);
    let new_checkpoint = normalized_fingerprint(&lesson.q);
    let new_code = normalized_fingerprint(&lesson.s);
    let new_explanation = redundancy_body_text(&lesson.e);
    let new_text = format!("{}\n{}\n{}", lesson.t, new_explanation, lesson.q);
    let new_tokens = significant_token_set(&new_text);
    let new_shingles = normalized_shingles(&new_explanation, 9);

    for prior in prior_lessons.iter().take(4) {
        let prior_title = normalized_fingerprint(&prior.title);
        let prior_checkpoint = normalized_fingerprint(&prior.checkpoint_question);
        let prior_code = normalized_fingerprint(&prior.code_lens);

        if !new_title.is_empty() && new_title == prior_title {
            return Err(ApiError::InvalidAiResponse);
        }
        if !new_checkpoint.is_empty() && new_checkpoint == prior_checkpoint {
            return Err(ApiError::InvalidAiResponse);
        }
        if new_code.chars().count() > 30 && new_code == prior_code {
            return Err(ApiError::InvalidAiResponse);
        }

        let prior_summary = redundancy_body_text(&prior.summary);
        let prior_text = format!(
            "{}\n{}\n{}",
            prior.title, prior_summary, prior.checkpoint_question
        );
        let prior_tokens = significant_token_set(&prior_text);
        let overlap = token_overlap_ratio(&new_tokens, &prior_tokens);
        let repeated_shingles =
            common_shingle_count(&new_shingles, &normalized_shingles(&prior_summary, 9));

        if overlap > 0.78 || (overlap > 0.62 && repeated_shingles >= 3) {
            return Err(ApiError::InvalidAiResponse);
        }
    }

    Ok(())
}

fn redundancy_body_text(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let mut cutoff = value.len();
    for marker in ["accuracy check:", "further study:"] {
        if let Some(index) = lower.find(marker) {
            cutoff = cutoff.min(index);
        }
    }
    value[..cutoff].trim().to_string()
}

fn normalized_fingerprint(value: &str) -> String {
    significant_tokens(value).join(" ")
}

fn significant_token_set(value: &str) -> BTreeSet<String> {
    significant_tokens(value).into_iter().collect()
}

fn significant_tokens(value: &str) -> Vec<String> {
    value
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '-')
        .map(str::trim)
        .filter(|token| token.chars().count() > 2 && !is_learning_stopword(token))
        .map(str::to_string)
        .collect()
}

fn is_learning_stopword(token: &str) -> bool {
    matches!(
        token,
        "the"
            | "and"
            | "for"
            | "with"
            | "that"
            | "this"
            | "from"
            | "into"
            | "before"
            | "after"
            | "must"
            | "should"
            | "could"
            | "would"
            | "about"
            | "because"
            | "through"
            | "which"
            | "what"
            | "when"
            | "where"
            | "your"
            | "their"
            | "there"
            | "then"
            | "than"
            | "only"
            | "also"
            | "lesson"
            | "module"
            | "generated"
            | "learner"
            | "learners"
            | "code"
            | "field"
            | "fields"
            | "value"
            | "values"
    )
}

fn token_overlap_ratio(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f32 {
    let denominator = left.len().min(right.len());
    if denominator == 0 {
        return 0.0;
    }
    let shared = left.intersection(right).count();
    shared as f32 / denominator as f32
}

fn normalized_shingles(value: &str, size: usize) -> BTreeSet<String> {
    let tokens = significant_tokens(value);
    if tokens.len() < size {
        return BTreeSet::new();
    }
    tokens
        .windows(size)
        .map(|window| window.join(" "))
        .collect::<BTreeSet<_>>()
}

fn common_shingle_count(left: &BTreeSet<String>, right: &BTreeSet<String>) -> usize {
    left.intersection(right).count()
}

fn lesson_mentions_required_ecosystem_terms(ecosystem_id: &str, text: &str) -> bool {
    let required_terms: &[&str] = match ecosystem_id {
        "stacks" => &[
            "stacks",
            "clarity",
            "sbtc",
            "bns",
            "proof of transfer",
            "bitcoin",
        ],
        "golem" => &[
            "golem",
            "yagna",
            "requestor",
            "provider",
            "task",
            "agreement",
            "allocation",
            "result",
            "decentralized compute",
        ],
        "ton-stonfi" => &[
            "ston.fi",
            "stonfi",
            "omniston",
            "ton connect",
            "jetton",
            "slippage",
            "min-out",
            "quote",
            "swap",
        ],
        "zcash" => &[
            "zcash",
            "zip-321",
            "shielded",
            "viewing key",
            "memo",
            "zatoshi",
            "orchard",
            "sapling",
        ],
        "ckb" => &["ckb", "cell", "outpoint", "script", "witness", "capacity"],
        "fiber" => &[
            "fiber", "channel", "invoice", "ptlc", "preimage", "route", "receipt",
        ],
        "ckb-fiber" => &[
            "ckb", "fiber", "cell", "outpoint", "script", "witness", "invoice", "channel", "joyid",
        ],
        "basics" => &[
            "blockchain",
            "web3",
            "wallet",
            "transaction",
            "confirmation",
            "address",
            "signature",
        ],
        _ => &["trust boundary", "denial test"],
    };

    required_terms.iter().any(|term| text.contains(term))
}

fn contains_unrequested_ecosystem_leakage(
    ecosystem_id: &str,
    request: &GenerateLearningModuleRequest,
    text: &str,
) -> bool {
    let requested_comparison = request
        .topic
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("compare")
        || request
            .learning_intents
            .iter()
            .any(|intent| intent.to_ascii_lowercase().contains("compare"));
    if requested_comparison || ecosystem_id == "basics" {
        return false;
    }

    let blocked: &[&str] = match ecosystem_id {
        "stacks" => &[
            "joyid",
            "xudt",
            "fiber invoice",
            "zip-321",
            "zatoshi",
            "orchard receiver",
        ],
        "golem" => &[
            "joyid",
            "xudt",
            "fiber invoice",
            "ckb cell",
            "outpoint lineage",
            "zip-321",
            "zatoshi",
            "orchard receiver",
            "clarity contract",
            "sbtc",
            "bns",
            "ston.fi",
            "omniston",
            "ton connect",
            "jetton",
        ],
        "ton-stonfi" => &[
            "joyid",
            "xudt",
            "ckb cell",
            "outpoint lineage",
            "zip-321",
            "zatoshi",
            "orchard receiver",
            "clarity contract",
            "sbtc",
            "bns",
        ],
        "zcash" => &[
            "joyid",
            "xudt",
            "fiber invoice",
            "ckb cell",
            "outpoint lineage",
            "clarity contract",
            "sbtc",
        ],
        "ckb" => &[
            "zip-321",
            "zatoshi",
            "orchard",
            "shielded address",
            "clarity",
            "sbtc",
            "bns",
        ],
        "fiber" => &[
            "zip-321",
            "zatoshi",
            "orchard",
            "shielded address",
            "clarity",
            "sbtc",
            "bns",
        ],
        "ckb-fiber" => &[
            "zip-321",
            "zatoshi",
            "orchard",
            "shielded address",
            "clarity",
            "sbtc",
            "bns",
        ],
        _ => &[],
    };

    blocked.iter().any(|term| text.contains(term))
}

fn contains_placeholder_learning_text(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let blocked = [
        "placeholder",
        "lorem ipsum",
        "todo:",
        "tbd",
        "coming soon",
        "insert ",
        "fill in ",
        "replace this",
        "example.com",
        "sample text",
        "as an ai",
        "i cannot",
        "generic proof boundary",
        "exact proof boundary for this lesson",
    ];

    blocked.iter().any(|needle| lower.contains(needle))
}

fn generic_learning_checkpoint_question(question: &str) -> bool {
    let lower = question.trim().to_ascii_lowercase();
    let names_domain_term = [
        "cell",
        "outpoint",
        "witness",
        "script",
        "channel",
        "invoice",
        "nonce",
        "ptlc",
        "joyid",
        "xudt",
        "fiber",
        "receipt",
        "capacity",
        "lock",
        "type",
        "zcash",
        "zip-321",
        "zip321",
        "shielded",
        "viewing",
        "memo",
        "zatoshi",
        "orchard",
        "payment request",
        "wallet",
        "transaction",
        "address",
        "confirmation",
        "mempool",
        "finality",
        "node",
        "stacks",
        "clarity",
        "sbtc",
        "bns",
        "post-condition",
        "principal",
        "proof of transfer",
        "ton",
        "ston.fi",
        "stonfi",
        "omniston",
        "ton connect",
        "jetton",
        "jetton master",
        "wallet contract",
        "slippage",
        "min-out",
        "quote",
        "route",
    ]
    .iter()
    .any(|term| lower.contains(term));

    lower.split_whitespace().count() < 9
        || lower.contains("exact proof boundary for this lesson")
        || (lower.contains("what is the proof boundary") && !names_domain_term)
        || (lower.contains("what must be proven for this lesson") && !names_domain_term)
}

fn compact_ai_lesson_to_learning_lesson(
    index: usize,
    _background: &str,
    focus: &str,
    request: &GenerateLearningModuleRequest,
    lesson: AiLearningLessonCompact,
) -> Result<LearningLesson, ApiError> {
    validate_ai_learning_lesson_compact_for_request_with_context(request, &lesson, index, &[])?;

    let title = lesson.t.trim().to_string();
    let why_it_matters = lesson.w.trim().to_string();
    let quest_bridge = lesson.j.trim().to_string();
    let follow_up = lesson.f.trim().to_string();
    let question = lesson.q.trim().to_string();
    let correct_answer = lesson.a.trim().to_string();

    let concepts = infer_learning_concepts(focus, &lesson);
    let correct_index = checkpoint_correct_index(index, lesson.ci);
    let correct_option = LearningOption {
        label: correct_answer,
        feedback: format!(
            "Correct. {}",
            checkpoint_explanation_for_lesson(&lesson, &concepts)
        ),
    };
    let mut wrong_answers =
        learning_wrong_options(lesson.b.clone(), lesson.bf.clone())?.into_iter();
    let options = (0..4)
        .map(|option_index| {
            if option_index == correct_index {
                Ok(correct_option.clone())
            } else {
                wrong_answers.next().ok_or(ApiError::InvalidAiResponse)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(LearningLesson {
        id: format!("module-{}-lesson-1", index + 1),
        title: title.clone(),
        why_it_matters,
        explanation: expanded_learning_explanation(&lesson),
        concepts: concepts.clone(),
        submodules: compact_learning_submodules(Vec::new(), &concepts),
        resources: default_learning_resources_for_focus(focus),
        evidence_map: learning_evidence_map_for_lesson(request, &title, &concepts),
        quality_score: learning_quality_score_for_compact(request, &lesson),
        checkpoint: LearningCheckpoint {
            question,
            options,
            correct_index,
            explanation: checkpoint_explanation_for_lesson(&lesson, &concepts),
            follow_up_question: follow_up,
        },
        quest_bridge,
    })
}

fn expanded_learning_explanation(lesson: &AiLearningLessonCompact) -> String {
    format!("{}\n\nCode lens:\n{}", lesson.e.trim(), lesson.s.trim())
}

fn checkpoint_explanation_for_lesson(
    lesson: &AiLearningLessonCompact,
    concepts: &[String],
) -> String {
    let concept_list = if concepts.is_empty() {
        "the lesson's trusted proof boundary".to_string()
    } else {
        concepts.join(", ")
    };
    format!(
        "The answer must connect '{}' to {} and to a denial test that mutates the trusted field. If the learner cannot state that attack in their own words, the generated code is still a black box.",
        lesson.a.trim(),
        concept_list,
    )
}

fn checkpoint_correct_index(lesson_index: usize, model_index: usize) -> usize {
    let jitter = Uuid::new_v4().as_bytes()[0] as usize;
    (model_index.min(3) + lesson_index + jitter) % 4
}

fn learning_evidence_map_for_lesson(
    request: &GenerateLearningModuleRequest,
    title: &str,
    concepts: &[String],
) -> Vec<LearningEvidence> {
    let resources = default_learning_resources_for_focus(&learning_focus_label(request));
    resources
        .into_iter()
        .take(3)
        .enumerate()
        .map(|(index, resource)| {
            let concept = concepts
                .get(index)
                .cloned()
                .unwrap_or_else(|| learning_ecosystem_label(request).to_string());
            LearningEvidence {
                claim: format!(
                    "{} is grounded by the {} source pack for {}.",
                    clamp_text(concept, 80),
                    learning_ecosystem_label(request),
                    clamp_text(title.to_string(), 80)
                ),
                source_title: resource.title,
                source_url: resource.url,
                lesson_section: if index == 0 {
                    "lesson body"
                } else {
                    "further study"
                }
                .to_string(),
                confidence: "source-pack".to_string(),
            }
        })
        .collect()
}

fn learning_quality_score_for_compact(
    request: &GenerateLearningModuleRequest,
    lesson: &AiLearningLessonCompact,
) -> LearningQualityScore {
    let combined = [
        lesson.t.as_str(),
        lesson.e.as_str(),
        lesson.s.as_str(),
        lesson.w.as_str(),
        lesson.j.as_str(),
        lesson.f.as_str(),
        lesson.q.as_str(),
        lesson.a.as_str(),
    ]
    .join("\n")
    .to_ascii_lowercase();
    let source_coverage = if combined.contains("further study:") {
        100
    } else {
        75
    };
    let technical_depth = ((lesson.e.split_whitespace().count().min(650) as u16) * 100 / 650) as u8;
    let checkpoint_quality = if generic_learning_checkpoint_question(&lesson.q) {
        35
    } else {
        95
    };
    let placeholder_free = !contains_placeholder_learning_text(&combined);
    let ecosystem_alignment =
        lesson_mentions_required_ecosystem_terms(&learning_ecosystem_id(request), &combined)
            && !contains_unrequested_ecosystem_leakage(
                &learning_ecosystem_id(request),
                request,
                &combined,
            );
    LearningQualityScore {
        source_coverage,
        technical_depth,
        checkpoint_quality,
        placeholder_free,
        ecosystem_alignment,
        passed: source_coverage >= 75
            && technical_depth >= 75
            && checkpoint_quality >= 75
            && placeholder_free
            && ecosystem_alignment,
    }
}

fn refresh_lesson_validation_metadata(request_focus: &str, lesson: &mut LearningLesson) {
    if lesson.evidence_map.is_empty() {
        lesson.evidence_map = lesson
            .resources
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, resource)| LearningEvidence {
                claim: format!(
                    "{} is tied to {} source guidance.",
                    lesson
                        .concepts
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| request_focus.to_string()),
                    resource.title
                ),
                source_title: resource.title.clone(),
                source_url: resource.url.clone(),
                lesson_section: if index == 0 {
                    "lesson body"
                } else {
                    "related resources"
                }
                .to_string(),
                confidence: "source-pack".to_string(),
            })
            .collect();
    }

    let explanation_words = lesson.explanation.split_whitespace().count();
    let source_coverage = if lesson.evidence_map.is_empty() {
        0
    } else {
        100
    };
    let technical_depth = ((explanation_words.min(650) as u16) * 100 / 650) as u8;
    let checkpoint_quality = if generic_learning_checkpoint_question(&lesson.checkpoint.question) {
        40
    } else {
        95
    };
    let placeholder_free = !contains_placeholder_learning_text(&format!(
        "{}\n{}\n{}\n{}",
        lesson.title, lesson.explanation, lesson.checkpoint.question, lesson.quest_bridge
    ));
    lesson.quality_score = LearningQualityScore {
        source_coverage,
        technical_depth,
        checkpoint_quality,
        placeholder_free,
        ecosystem_alignment: true,
        passed: source_coverage >= 75
            && technical_depth >= 60
            && checkpoint_quality >= 75
            && placeholder_free,
    };
}

fn learning_wrong_options(
    labels: Vec<String>,
    feedbacks: Vec<String>,
) -> Result<Vec<LearningOption>, ApiError> {
    let labels = labels
        .into_iter()
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty())
        .take(3)
        .collect::<Vec<_>>();
    let feedbacks = feedbacks
        .into_iter()
        .map(|feedback| feedback.trim().to_string())
        .filter(|feedback| !feedback.is_empty())
        .take(3)
        .collect::<Vec<_>>();

    if labels.len() != 3 || feedbacks.len() != 3 {
        return Err(ApiError::InvalidAiResponse);
    }

    Ok(labels
        .into_iter()
        .zip(feedbacks)
        .map(|(label, feedback)| LearningOption { label, feedback })
        .collect())
}

fn infer_learning_concepts(focus: &str, lesson: &AiLearningLessonCompact) -> Vec<String> {
    let lower = format!(
        "{} {} {} {} {}",
        focus, lesson.t, lesson.e, lesson.s, lesson.q
    )
    .to_lowercase();
    if focus.to_ascii_lowercase().contains("basic")
        || focus.to_ascii_lowercase().contains("web3")
        || focus
            .to_ascii_lowercase()
            .contains("blockchain fundamentals")
    {
        let mut concepts = Vec::new();
        for (needle, concept) in [
            ("wallet", "wallet"),
            ("private key", "private key"),
            ("public key", "public key"),
            ("signature", "signature"),
            ("address", "address"),
            ("transaction", "transaction"),
            ("mempool", "mempool"),
            ("confirmation", "confirmation"),
            ("finality", "finality"),
            ("reorg", "reorg safety"),
            ("utxo", "UTXO model"),
            ("account", "account model"),
            ("smart contract", "smart contract"),
            ("node", "node"),
            ("nonce", "nonce"),
            ("replay", "replay resistance"),
        ] {
            if lower.contains(needle) && !concepts.iter().any(|value| value == concept) {
                concepts.push(concept.to_string());
            }
        }
        if concepts.is_empty() {
            concepts.extend([
                "wallet".to_string(),
                "signature".to_string(),
                "transaction".to_string(),
                "node".to_string(),
                "confirmation".to_string(),
            ]);
        }
        concepts.truncate(5);
        return concepts;
    }

    let mut concepts = Vec::new();
    for (needle, concept) in [
        ("cell", "CKB cell"),
        ("outpoint", "OutPoint"),
        ("script", "script"),
        ("lock", "lock script"),
        ("type", "type script"),
        ("xudt", "xUDT"),
        ("witness", "witness"),
        ("signature", "signature"),
        ("joyid", "JoyID proof"),
        ("nonce", "nonce"),
        ("invoice", "Fiber invoice"),
        ("ptlc", "PTLC"),
        ("htlc", "HTLC"),
        ("channel", "channel state"),
        ("replay", "replay defense"),
        ("preimage", "preimage"),
        ("payout", "payout split"),
        ("reward", "reward claim"),
        ("zcash", "Zcash"),
        ("zip-321", "ZIP-321 payment request"),
        ("zip321", "ZIP-321 payment request"),
        ("shielded", "shielded payment"),
        ("viewing", "viewing key boundary"),
        ("memo", "memo disclosure boundary"),
        ("zatoshi", "zatoshi amount"),
        ("orchard", "Orchard receiver"),
        ("stacks", "Stacks"),
        ("clarity", "Clarity contract"),
        ("sbtc", "sBTC"),
        ("bns", "BNS"),
        ("post-condition", "post-condition"),
        ("principal", "principal"),
        ("proof of transfer", "Proof of Transfer"),
        ("golem", "Golem"),
        ("yagna", "Yagna"),
        ("requestor", "requestor"),
        ("provider", "provider"),
        ("agreement", "market agreement"),
        ("allocation", "allocation"),
        ("js sdk", "Golem JS SDK"),
        ("javascript sdk", "Golem JS SDK"),
        ("python", "Golem Python SDK"),
        ("ray", "Ray on Golem"),
        ("dapp", "Golem dApp"),
        ("gvmi", "GVMI image"),
        ("task", "task lifecycle"),
        ("result", "result validation"),
        ("budget", "budget boundary"),
        ("ston.fi", "STON.fi integration"),
        ("stonfi", "STON.fi integration"),
        ("omniston", "Omniston widget"),
        ("ton connect", "TON Connect"),
        ("jetton master", "jetton master contract"),
        ("jetton", "jetton"),
        ("slippage", "slippage boundary"),
        ("min-out", "min-out constraint"),
        ("quote", "quote freshness"),
        ("route", "swap route"),
        ("referral", "referral fee disclosure"),
    ] {
        if lower.contains(needle) && !concepts.iter().any(|value| value == concept) {
            concepts.push(concept.to_string());
        }
    }

    if concepts.is_empty() {
        concepts.push(clamp_text(focus.to_string(), 40));
        concepts.push("trust boundary".to_string());
        concepts.push("denial test".to_string());
    }

    concepts.truncate(5);
    concepts
}

fn learning_ecosystem_id(request: &GenerateLearningModuleRequest) -> String {
    let raw = request
        .ecosystem_id
        .as_deref()
        .or(request.path_id.as_deref())
        .unwrap_or("ckb-fiber")
        .to_ascii_lowercase();

    if raw.contains("basic") || raw.contains("web") || raw.contains("blockchain") {
        "basics".to_string()
    } else if raw.contains("golem")
        || raw.contains("yagna")
        || raw.contains("requestor")
        || raw.contains("provider")
        || raw.contains("decentralized compute")
    {
        "golem".to_string()
    } else if raw.contains("ton-stonfi")
        || raw.contains("ston.fi")
        || raw.contains("stonfi")
        || raw.contains("omniston")
        || raw.contains("jetton")
    {
        "ton-stonfi".to_string()
    } else if raw.contains("stacks")
        || raw.contains("clarity")
        || raw.contains("sbtc")
        || raw.contains("bns")
    {
        "stacks".to_string()
    } else if raw.contains("zcash") {
        "zcash".to_string()
    } else if raw.contains("fiber") && !raw.contains("ckb") {
        "fiber".to_string()
    } else if raw.contains("ckb") && !raw.contains("fiber") {
        "ckb".to_string()
    } else {
        "ckb-fiber".to_string()
    }
}

fn learning_ecosystem_label(request: &GenerateLearningModuleRequest) -> &'static str {
    match learning_ecosystem_id(request).as_str() {
        "stacks" => "Stacks",
        "ton-stonfi" => "TON / STON.fi",
        "golem" => "Golem",
        "zcash" => "Zcash",
        "fiber" => "Fiber",
        "ckb" => "CKB",
        "basics" => "Web3 + Blockchain Basics",
        _ => "CKB/Fiber",
    }
}

fn learning_topic_label(request: &GenerateLearningModuleRequest) -> Option<String> {
    request
        .topic
        .as_deref()
        .map(str::trim)
        .filter(|topic| !topic.is_empty())
        .map(|topic| clamp_text(topic.to_string(), 80))
}

fn learning_intent_label(request: &GenerateLearningModuleRequest) -> String {
    let intents = request
        .learning_intents
        .iter()
        .map(|intent| intent.trim())
        .filter(|intent| !intent.is_empty())
        .take(4)
        .collect::<Vec<_>>();

    if intents.is_empty() {
        "understand generated code, name the trust boundary, and design denial tests".to_string()
    } else {
        intents.join("; ")
    }
}

fn learning_module_title(request: &GenerateLearningModuleRequest) -> String {
    let focus = remove_duplicate_ecosystem_prefix(
        &learning_focus_label(request),
        learning_ecosystem_label(request),
    );
    format!("{} Deep Dive", clamp_text(focus, 64))
}

fn clean_learning_module_title(raw: &str) -> String {
    let mut title = raw.trim();
    let lower = title.to_ascii_lowercase();
    if lower.starts_with("vibequest:") {
        title = title["vibequest:".len()..].trim();
    } else if lower.starts_with("vibequest -") {
        title = title["vibequest -".len()..].trim();
    } else if lower.starts_with("vibequest —") {
        title = title["vibequest —".len()..].trim();
    }

    let mut cleaned = title.to_string();
    for ecosystem in [
        "Golem",
        "Stacks",
        "TON / STON.fi",
        "STON.fi",
        "TON",
        "Zcash",
        "CKB",
        "Fiber",
        "CKB/Fiber",
        "Web3 + Blockchain",
        "Web3 + Blockchain Basics",
    ] {
        cleaned = remove_duplicate_ecosystem_prefix(&cleaned, ecosystem);
    }
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "Learning Track".to_string()
    } else {
        cleaned.to_string()
    }
}

fn remove_duplicate_ecosystem_prefix(raw: &str, ecosystem: &str) -> String {
    let trimmed = raw.trim();
    let prefix = format!("{}:", ecosystem);
    if !trimmed
        .to_ascii_lowercase()
        .starts_with(&prefix.to_ascii_lowercase())
    {
        return trimmed.to_string();
    }

    let after_prefix = trimmed[prefix.len()..].trim_start();
    let lower_after = after_prefix.to_ascii_lowercase();
    let lower_ecosystem = ecosystem.to_ascii_lowercase();
    if lower_after == lower_ecosystem
        || lower_after.starts_with(&format!("{} ", lower_ecosystem))
        || lower_after.starts_with(&format!("{}:", lower_ecosystem))
        || lower_after.starts_with(&format!("{} -", lower_ecosystem))
        || lower_after.starts_with(&format!("{} —", lower_ecosystem))
    {
        after_prefix.to_string()
    } else {
        trimmed.to_string()
    }
}

fn learning_focus_label(request: &GenerateLearningModuleRequest) -> String {
    let interests = request
        .interests
        .iter()
        .map(|interest| interest.trim())
        .filter(|interest| !interest.is_empty())
        .take(4)
        .collect::<Vec<_>>();
    let topic = learning_topic_label(request);

    if let Some(topic) = topic {
        if interests.is_empty() {
            format!("{}: {topic}", learning_ecosystem_label(request))
        } else {
            format!(
                "{}: {topic} ({})",
                learning_ecosystem_label(request),
                interests.join(" + ")
            )
        }
    } else if interests.is_empty() {
        learning_ecosystem_label(request).to_string()
    } else {
        format!(
            "{}: {}",
            learning_ecosystem_label(request),
            interests.join(" + ")
        )
    }
}

fn learning_background_label(request: &GenerateLearningModuleRequest) -> String {
    request
        .learning_profile
        .as_deref()
        .map(str::trim)
        .filter(|profile| !profile.is_empty())
        .or_else(|| {
            let background = request.background.trim();
            (!background.is_empty()).then_some(background)
        })
        .unwrap_or("learner")
        .to_string()
}

fn learning_module_profile(request: &GenerateLearningModuleRequest) -> String {
    format!(
        "A {} learning {} through live AI-authored deep modules, code snippets, checkpoints, tutor support, and practical quest handoffs. Learning intents: {}.",
        learning_background_label(request),
        learning_focus_label(request),
        learning_intent_label(request)
    )
}

fn learning_module_outcome(request: &GenerateLearningModuleRequest) -> String {
    format!(
        "Explain {} trust boundaries, read generated verifier code, answer code-aware checkpoints, and turn passed lessons into quests. Target intents: {}.",
        learning_focus_label(request),
        learning_intent_label(request)
    )
}

fn learning_module_capstone_prompt(request: &GenerateLearningModuleRequest) -> String {
    match learning_ecosystem_id(request).as_str() {
        "zcash" => format!(
            "Generate a Zcash shielded-checkout verifier quest for {} with ZIP-321/payment request validation, privacy-boundary explanation, denial tests, and server-owned completion evidence.",
            learning_focus_label(request)
        ),
        "fiber" => format!(
            "Generate a Fiber verifier quest for {} with invoice/channel-state binding, replay denial tests, a boss question, and reward-safe ship gate evidence.",
            learning_focus_label(request)
        ),
        "ckb" => format!(
            "Generate a CKB verifier quest for {} with cell, script, witness, or transaction proof binding plus denial tests and server-owned completion evidence.",
            learning_focus_label(request)
        ),
        "stacks" => format!(
            "Generate a Stacks learning quest for {} with Stacks/Bitcoin reasoning, Clarity-safe authorization, sBTC or BNS product context where relevant, checkpoint evidence, and one denial test for unsafe app assumptions.",
            learning_focus_label(request)
        ),
        "ton-stonfi" => format!(
            "Generate a final TON / STON.fi safe-swap integration lab for {} with SDK or Omniston widget code, TON Connect wallet boundary, jetton master verification, slippage/min-out checks, referral-fee disclosure, transaction-state evidence, and at least eight denial tests covering fake jettons, stale quotes, missing min-out, wallet rejection, pending-as-success, wrong manifest domain, duplicate connector state, and REST API responses treated as settlement proof.",
            learning_focus_label(request)
        ),
        "golem" => format!(
            "Generate a final Golem compute execution quest for {} with requestor/provider boundaries, Yagna/app-key setup, JS SDK or Python/Ray task execution, dApp deployment where relevant, result validation, budget/payment awareness, provider failure handling, and at least seven failure cases covering provider unavailable, task timeout, missing result, corrupted result, wrong image or runtime, agreement mismatch, budget exceeded, Yagna disconnect, and unsupported Ray limitations.",
            learning_focus_label(request)
        ),
        "basics" => format!(
            "Generate a beginner Web3 foundations quest for {} with plain-language wallet, transaction, block explorer, network safety, and confirmation reasoning plus one practical denial test.",
            learning_focus_label(request)
        ),
        _ => format!(
            "Generate a CKB/Fiber verifier quest for {} with proof binding, denial tests, a boss question, and a reward-safe ship gate.",
            learning_focus_label(request)
        ),
    }
}

fn learning_lesson_role(
    request: &GenerateLearningModuleRequest,
    lesson_index: usize,
) -> &'static str {
    let discriminator = request
        .path_id
        .as_deref()
        .unwrap_or_else(|| request.ecosystem_id.as_deref().unwrap_or_default())
        .to_ascii_lowercase();
    let roles = if discriminator.contains("zcash") {
        [
            "shielded-payment mental model and privacy-preserving checkout scope",
            "ZIP-321/payment request structure, recipient safety, amount bounds, and network mismatch denial",
            "viewing-key, memo, address, and disclosure boundaries in generated app code",
            "denial testing for malformed requests, transparent memo misuse, replay, wrong-network, and unsafe recipient cases",
            "turning Zcash shielded-checkout understanding into a generated verifier quest",
        ]
    } else if discriminator.contains("golem")
        || discriminator.contains("yagna")
        || discriminator.contains("requestor")
        || discriminator.contains("decentralized compute")
    {
        [
            "Golem compute mental model: requestors, providers, Yagna, agreements, tasks, results, and payment boundaries",
            "Golem JS SDK task execution: packages, task model, provider execution, result handling, and cleanup",
            "Golem Python and Ray workloads: executor flow, workload splitting, supported versions, and practical limitations",
            "Golem dApp deployment lifecycle: GVMI, descriptors, services, logs, proxies, and lifecycle control",
            "turning Golem understanding into a final compute quest with provider, task, result, budget, and failure-state checks",
        ]
    } else if discriminator.contains("ton-stonfi")
        || discriminator.contains("ston.fi")
        || discriminator.contains("stonfi")
        || discriminator.contains("omniston")
        || discriminator.contains("jetton")
    {
        [
            "TON DeFi mental model for STON.fi integrations: wallet connection, swaps, and transaction evidence",
            "STON.fi SDK or Omniston quote flow: route construction, quote freshness, and transaction building",
            "jetton verification: master contract identity, wallet contract distinction, metadata spoofing, and allowlists",
            "slippage, min-out, referral fee disclosure, stale quote denial, and wallet approval UX",
            "turning STON.fi understanding into a safe swap integration quest with denial tests",
        ]
    } else if discriminator.contains("stacks")
        || discriminator.contains("clarity")
        || discriminator.contains("sbtc")
        || discriminator.contains("bns")
    {
        [
            "Stacks and Bitcoin mental model: settlement, blocks, transactions, and application scope",
            "Clarity basics: predictable smart contracts, public functions, maps, principals, and post-conditions",
            "wallet authorization: addresses, signatures, transaction submission, and unsafe frontend assumptions",
            "sBTC and BNS product flows: user identity, Bitcoin-backed assets, naming, and application safety",
            "turning Stacks understanding into a generated Clarity-oriented quest",
        ]
    } else if discriminator.contains("basic")
        || discriminator.contains("web")
        || discriminator.contains("blockchain")
    {
        [
            "absolute beginner mental model: what a blockchain is and why shared history matters",
            "wallet basics: accounts, addresses, recovery phrases, signing, and what not to share",
            "transaction basics: sending value, fees, confirmations, explorers, and stuck or failed transactions",
            "Web3 app basics: connect wallet, approve actions, verify network, and avoid phishing prompts",
            "turning beginner Web3 understanding into a generated implementation quest",
        ]
    } else if discriminator.contains("fiber") && !discriminator.contains("ckb") {
        [
            "payment channel mental model and CKB-backed settlement assumptions",
            "invoice, PTLC, route, and channel-state proof boundaries",
            "paid-content receipt verification and replay-resistant access control",
            "amount, balance transition, and payout integrity checks",
            "turning Fiber payment understanding into a generated verifier quest",
        ]
    } else if discriminator.contains("ckb") && !discriminator.contains("fiber") {
        [
            "state model and live-cell evidence",
            "OutPoint lineage, inputs, outputs, and transaction scope",
            "lock scripts, type scripts, witnesses, and local verifier trust",
            "denial testing for copied cell data, stale witnesses, and fake frontend payloads",
            "turning CKB cell understanding into a generated verifier quest",
        ]
    } else if discriminator.contains("security") {
        [
            "threat modeling multi-ecosystem flows before trusting generated code",
            "replay, mismatch, stale state, and witness-substitution attacks",
            "account identity scope, nonce freshness, and action intent",
            "denial tests that mutate the exact proof boundary under review",
            "turning audit findings into a generated fix-and-defend quest",
        ]
    } else {
        [
            "core mental model for the selected ecosystem topic",
            "proof boundary and generated-code reading habit",
            "account, payment, state, privacy, or verifier integration risk",
            "attack case and denial test design",
            "turning the lesson into a practical generated quest",
        ]
    };

    roles[lesson_index.min(4)]
}

fn learning_speciality_directive(background: &str) -> &'static str {
    let lower = background.to_ascii_lowercase();
    if lower.contains("vibecoder") {
        "The learner uses AI to generate code quickly. Teach them how to slow down at the proof boundary, read generated code, name trusted fields, and design denial tests before believing the AI output."
    } else if lower.contains("backend") {
        "The learner writes backend services. Emphasize verifier placement, database versus protocol evidence, request tampering, authorization scope, replay prevention, and tests that run outside the frontend."
    } else if lower.contains("frontend") {
        "The learner builds interfaces. Explain what the UI may display versus what the backend or protocol must verify, how auth or payment UX can mislead, and how to surface proof state without trusting client labels."
    } else if lower.contains("security") || lower.contains("auditor") {
        "The learner reviews systems for risk. Emphasize threat models, attacker-controlled fields, stale proofs, replay paths, denial tests, and how to prove generated code rejects the intended attack."
    } else if lower.contains("product") || lower.contains("community") {
        "The learner may not write every line of code. Explain value, risk, trust boundaries, user stories, and plain-language failure cases while still pointing to the technical evidence that matters."
    } else {
        "Teach through concrete ecosystem examples, generated-code reading habits, proof boundaries, and denial tests that fit the learner's stated background."
    }
}

fn learning_focus_directive(request: &GenerateLearningModuleRequest) -> &'static str {
    let discriminator = request
        .path_id
        .as_deref()
        .unwrap_or_else(|| request.ecosystem_id.as_deref().unwrap_or_default())
        .to_ascii_lowercase();
    if discriminator.contains("zcash") {
        "Ground the lesson in Zcash shielded-payment UX, ZIP-321/payment requests, address/network safety, viewing-key and memo disclosure boundaries, payment lifecycle, privacy expectations, and denial cases that a generated checkout verifier must reject."
    } else if discriminator.contains("golem")
        || discriminator.contains("yagna")
        || discriminator.contains("requestor")
        || discriminator.contains("decentralized compute")
    {
        "Ground the lesson in Golem decentralized compute execution: requestor/provider separation, Yagna and app keys, agreements and allocations, JS SDK task execution, Python and Ray workload choices, dApp deployment lifecycle, provider selection, budget/payment awareness, result validation, cleanup, and failure cases that prevent treating provider output, UI state, or broad AI/GPU claims as automatically reliable."
    } else if discriminator.contains("ton-stonfi")
        || discriminator.contains("ston.fi")
        || discriminator.contains("stonfi")
        || discriminator.contains("omniston")
        || discriminator.contains("jetton")
    {
        "Ground the lesson in TON / STON.fi DeFi integration: STON.fi DEX SDK, Omniston widget flows, TON Connect wallet authorization, jetton master versus jetton wallet contracts, quote freshness, swap routes, slippage/min-out checks, referral-fee disclosure, and denial cases that prevent trusting UI state, token metadata, REST responses, or pending wallet state as final transaction evidence."
    } else if discriminator.contains("stacks")
        || discriminator.contains("clarity")
        || discriminator.contains("sbtc")
        || discriminator.contains("bns")
    {
        "Ground the lesson in Stacks and Bitcoin app development: Proof of Transfer mental model, Clarity contract behavior, principals, post-conditions, wallet authorization, transaction safety, sBTC basics, BNS product identity, and denial cases that prevent trusting frontend state as protocol evidence."
    } else if discriminator.contains("basic")
        || discriminator.contains("web")
        || discriminator.contains("blockchain")
    {
        "Ground the lesson in absolute beginner Web3 and blockchain fundamentals: what a blockchain is, blocks, shared history, wallets as user-controlled accounts, addresses, recovery phrases, signing, transaction fees, confirmations, block explorers, wrong networks, phishing prompts, and practical safety habits. Avoid advanced jargon unless you define it first."
    } else if discriminator.contains("fiber") && !discriminator.contains("ckb") {
        "Ground the lesson in Fiber payment channels, invoices, PTLC-based security, routing, off-chain channel state, CKB settlement assumptions, and paid-access receipt verification."
    } else if discriminator.contains("ckb") && !discriminator.contains("fiber") {
        "Ground the lesson in CKB cells, capacity, cell data, OutPoint lineage, inputs and outputs, lock/type scripts, witnesses, transaction evidence, and local verifier boundaries."
    } else if discriminator.contains("security") {
        "Ground the lesson in replay defense, witness mismatch, stale Fiber state, account authorization scope, Zcash privacy leakage, xUDT payout integrity, denial tests, and reward-safe ship gates."
    } else {
        "Ground the lesson in the selected ecosystem interests and make the proof boundary clear enough to become a practical quest."
    }
}

fn learning_source_grounding_directive(request: &GenerateLearningModuleRequest) -> &'static str {
    match learning_ecosystem_id(request).as_str() {
        "stacks" => {
            "Ground facts in official Stacks sources without quoting them: Stacks docs https://docs.stacks.co/ for Stacks/Bitcoin, Clarity, wallets, transactions, sBTC, and BNS concepts. Do not use CKB, Fiber, or Zcash examples unless the learner explicitly asked to compare ecosystems."
        }
        "golem" => {
            "Ground facts in official Golem sources without quoting them. Make the lesson feel like a practical decentralized-compute onboarding lab, not a docs summary and not a generic AI lesson. Use at least two relevant source categories from this source pack when possible: Golem docs https://docs.golem.network/, quickstarts https://docs.golem.network/docs/quickstarts, JS SDK https://docs.golem.network/docs/creators/javascript, JS task model https://docs.golem.network/docs/creators/javascript/guides/task-model, JS executing tasks https://docs.golem.network/docs/creators/javascript/examples/executing-tasks, requestor/provider interaction https://docs.golem.network/docs/creators/common/requestor-provider-interaction, Python quickstart https://docs.golem.network/docs/creators/python/quickstarts/run-first-task-on-golem, Python application fundamentals https://docs.golem.network/docs/creators/python/guides/application-fundamentals, Ray on Golem https://docs.golem.network/docs/creators/ray, Ray limitations https://docs.golem.network/docs/creators/ray/supported-versions-and-other-limitations, dApp deployment https://docs.golem.network/docs/creators/dapps/hello-world-dapp, creating dApps https://docs.golem.network/docs/creators/dapps/creating-golem-dapps, provider overview https://docs.golem.network/docs/providers, and provider architecture https://docs.golem.network/docs/golem/overview/provider. Do not use CKB, Fiber, Zcash, Stacks, TON, or STON.fi examples unless the learner explicitly asked to compare ecosystems. Do not claim Golem is a smart-contract chain or that VibeQuest certifies production deployments. Every Golem module must identify what runs locally, what Yagna coordinates, what a provider executes, what output must be validated, and what failure would stop the learner from trusting the job."
        }
        "ton-stonfi" => {
            "Ground facts in official STON.fi and TON sources without quoting them. Use at least two relevant source categories from this source pack when possible: STON.fi DEX overview https://docs.ston.fi/developer-section/dex/overview, DEX SDK https://docs.ston.fi/developer-section/dex/sdk, DEX smart contracts https://docs.ston.fi/developer-section/dex/smart-contracts, REST API https://docs.ston.fi/developer-section/dex/api, Omniston widget https://docs.ston.fi/developer-section/widget/widget, Omniston SDK https://docs.ston.fi/developer-section/omniston/sdk, TON Connect overview https://docs.ton.org/applications/ton-connect/overview, TON Connect UI reference https://docs.ton.org/applications/ton-connect/api-reference/ui, TON token overview https://docs.ton.org/contracts/standard/tokens/overview, TON jetton processing https://docs.ton.org/applications/payments/jettons, TON jetton interface https://docs.ton.org/contracts/standard/tokens/jettons/api, and TON jetton architecture https://docs.ton.org/contracts/standard/tokens/jettons/how-it-works. Do not use CKB, Fiber, Zcash, or Stacks examples unless the learner explicitly asked to compare ecosystems."
        }
        "zcash" => {
            "Ground facts in official Zcash sources without quoting them: Zcash docs https://zcash.readthedocs.io/ and ZIP-321 https://zips.z.cash/zip-0321 for shielded payments, payment requests, privacy boundaries, memos, viewing keys, and confirmation safety."
        }
        "fiber" => {
            "Ground facts in Fiber and CKB sources without quoting them: Fiber repository https://github.com/nervosnetwork/fiber and CKB docs https://docs.nervos.org/ for channels, invoices, PTLCs, routing, receipts, and settlement boundaries."
        }
        "ckb" => {
            "Ground facts in official CKB sources without quoting them: CKB docs https://docs.nervos.org/ for cells, scripts, witnesses, transactions, capacity, and verifier boundaries."
        }
        "basics" => {
            "Ground facts in beginner-safe Web3 sources without quoting them: Ethereum developer docs https://ethereum.org/developers/docs/, MDN Web Docs https://developer.mozilla.org/, and relevant ecosystem docs only as examples after defining the basics."
        }
        _ => {
            "Ground facts in the selected ecosystem source pack without quoting sources. Avoid unrelated ecosystem concepts unless the learner explicitly asked to compare ecosystems."
        }
    }
}

fn learning_lesson_prompt(
    request: &GenerateLearningModuleRequest,
    lesson_index: usize,
    repair: bool,
    prior_lessons: &[PriorLearningLesson],
) -> String {
    let nonce = Uuid::new_v4();
    let interests = request
        .interests
        .iter()
        .map(|interest| interest.trim())
        .filter(|interest| !interest.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join(", ");
    let interests = if interests.is_empty() {
        format!(
            "{} foundations, {}",
            learning_ecosystem_label(request),
            learning_intent_label(request)
        )
    } else {
        interests
    };
    let background = learning_background_label(request);
    let background = background.as_str();
    let wants_code_snippets = wants_code_snippets_for_request(request);
    let code_snippet_directive = if wants_code_snippets {
        "The learner selected interactive code samples. s must be a compact but real TypeScript or Rust snippet of 8-24 lines with comments and one safe learner edit point comment labeled \"Learner edit:\". Do not use TODO, placeholder, example.com, or filler wording. The snippet must be directly tied to the lesson."
    } else {
        "s is one matching TypeScript/Rust code lens line."
    };
    let repair_directive = if repair {
        "The previous lesson was rejected because it was short, generic, incomplete, repetitive, inaccurate-looking, weakly sourced, or did not name concrete proof-boundary fields. Return a complete lesson this time: the e field alone must be at least 560 words and must teach with concrete examples, failure cases, official-resource guidance, and no filler."
    } else {
        ""
    };
    let prior_lesson_context = prior_lesson_context_directive(prior_lessons);

    format!(
        r#"Return minified JSON only with keys exactly: t,e,s,w,j,f,q,a,b,bf,ci. No markdown or prose outside JSON.

VibeQuest module {module_number}/5. Role: {module_role}. Interests: {interests}. Learner goal: {goal}. Speciality: {background}. Pace: {pace}. Focus: {focus_directive}. Speciality lens: {speciality_directive}. {repair_directive}

{source_grounding_directive}
{prior_lesson_context}

Global VibeQuest accuracy standard: teach as if a reviewer will compare every claim against the official source pack. Separate protocol evidence from app state. Name the exact verification boundary, one edge case, one denial test, and one nuance where a common shortcut would be wrong. Do not repeat a prior module's title, opening, checkpoint, code lens, or proof boundary.

e must be at least 520 words of real teaching prose with paragraphs. Define the key terms, explain how the idea appears in generated TypeScript or Rust, name one realistic builder mistake, describe one denial-test idea, include one nested submodule path using the phrase "Submodule path:", include one sentence starting with "Accuracy check:" that tells the learner how to verify the claim against official docs/specs, and add a short "Further study:" sentence naming official docs/specs to read. {code_snippet_directive} w is 35-60 words on why it matters to this speciality. j is 22-45 words naming the practice quest artifact and denial test. f is one follow-up reasoning question. q is one checkpoint about this lesson's exact proof boundary and must name concrete fields or concepts from the selected ecosystem, such as cell, OutPoint, witness, script, channel, invoice, nonce, PTLC, ZIP-321 request, zatoshi amount, shielded address, viewing key, memo, wallet address, signature domain, transaction hash, node, mempool, confirmation depth, Stacks block, Clarity contract, principal, post-condition, sBTC, BNS name, Proof of Transfer, STON.fi swap quote, Omniston widget config, TON Connect manifest, jetton master address, jetton wallet contract, slippage, min-out, stale quote, referral fee, route, transaction state, Golem requestor, provider, Yagna app key, agreement, allocation, task, result, budget, JS SDK, Python SDK, Ray limitation, dApp descriptor, GVMI image, service lifecycle, or provider timeout. Do not ask generic questions like "What is the exact proof boundary for this lesson?". a is the specific correct answer. b has exactly 3 plausible wrong answer labels. bf has exactly 3 matching feedback strings. ci is 0-3 and must vary. Avoid meta labels such as old fallback wording. Seed: {nonce}."#,
        module_number = lesson_index + 1,
        module_role = learning_lesson_role(request, lesson_index),
        goal = request.learner_goal.trim(),
        background = background,
        pace = request.pace.trim(),
        focus_directive = learning_focus_directive(request),
        source_grounding_directive = learning_source_grounding_directive(request),
        speciality_directive = learning_speciality_directive(background),
        code_snippet_directive = code_snippet_directive,
        prior_lesson_context = prior_lesson_context,
    )
}

fn prior_lesson_context_directive(prior_lessons: &[PriorLearningLesson]) -> String {
    let prior = prior_lessons
        .iter()
        .take(4)
        .enumerate()
        .map(|(index, lesson)| {
            format!(
                "Prior module {}: title='{}'; checkpoint='{}'",
                index + 1,
                clamp_text(lesson.title.clone(), 80),
                clamp_text(lesson.checkpoint_question.clone(), 120)
            )
        })
        .collect::<Vec<_>>();

    if prior.is_empty() {
        String::new()
    } else {
        format!(
            "Already generated modules for this course: {}. The next module must advance the path, use a different concrete failure mode, and avoid repeated wording.",
            prior.join(" | ")
        )
    }
}

fn code_tutor_prompt(request: &CodeTutorRequest) -> String {
    let files = request
        .files
        .iter()
        .map(|file| {
            format!(
                "FILE: {path} ({language})\n```\n{content}\n```",
                path = file.path.trim(),
                language = file.language.trim(),
                content = file.content.trim(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let challenge = request
        .challenge
        .as_ref()
        .map(|brief| {
            format!(
                "Invariant: {invariant}\nAttack scenario: {attack}\nCode focus: {code}\nTest focus: {test}",
                invariant = brief.invariant.trim(),
                attack = brief.attack_scenario.trim(),
                code = brief.code_focus.trim(),
                test = brief.test_focus.trim(),
            )
        })
        .unwrap_or_else(|| "No structured challenge brief supplied.".to_string());

    format!(
        r#"Return minified JSON only.
No markdown. No prose outside JSON.

Quest: {title}
Objective: {objective}
Challenge context:
{challenge}

Generated files:
{files}

Learner question: {question}

Keys exactly: answer,code_walkthrough,common_misunderstanding,follow_up_question,references.
Rules:
- Ground the answer in the generated files. Mention file paths, functions, fields, and tests when useful.
- Teach the selected ecosystem or protocol concept behind the code, then explain the AI-coding mistake this prevents.
- If the learner asks for a patch, describe the change and the denial test to add.
- code_walkthrough: 3-5 short bullets, each tied to a concrete line/function/field in the generated files.
- common_misunderstanding: name the likely wrong mental model and correct it.
- follow_up_question: ask one question that checks whether the learner truly understood this code.
- references: 2-3 authoritative links with title,url,reason. Prefer official docs/specs/canonical repos: CKB Docs, Fiber repo, Zcash docs, ZIP-321, Stacks docs, STON.fi docs, TON docs, Ethereum developer docs, or JoyID docs when relevant.
- Keep answer under 170 words."#,
        title = request.quest_title.trim(),
        objective = request.quest_objective.trim(),
        challenge = challenge,
        files = files,
        question = request.question.trim(),
    )
}

fn learning_tutor_prompt(request: &LearningTutorRequest) -> String {
    format!(
        r#"Return minified JSON only. No markdown. No prose outside JSON.

Module: {module}
Active lesson: {lesson}
Structured active generated lesson context:
{context}

Learner question: {question}

Keys exactly: answer,why_it_matters,follow_up_question,references.
Rules:
- Treat the structured active generated lesson context as the primary source of truth.
- Anchor the answer to the lesson title, code lens, checkpoint, selected answer state, and practice quest bridge when present.
- If the learner asks for a walkthrough, explain: concept gist, how the code lens works, what the checkpoint is testing, the likely vibecoding mistake, and one concrete denial-test habit.
- If the learner is wrong or vague, correct the misunderstanding using the checkpoint options/feedback and ask a different related follow-up question.
- Do not answer as a generic CKB/Fiber overview unless the lesson context is missing; connect every outside concept back to this active lesson.
- references: 2-3 authoritative links with title,url,reason. Prefer official docs/specs/canonical repos: CKB Docs, Fiber repo, Zcash docs, ZIP-321, Stacks docs, STON.fi docs, TON docs, Ethereum developer docs, or JoyID docs when relevant.
- Keep answer under 230 words. Keep why_it_matters under 90 words. follow_up_question must be one question tied to this lesson."#,
        module = request.module_title.trim(),
        lesson = request.lesson_title.trim(),
        context = request.lesson_context.trim(),
        question = request.question.trim(),
    )
}

fn is_learning_only_prompt(prompt: &str) -> bool {
    let normalized = prompt.trim().to_lowercase();
    if normalized.is_empty() {
        return false;
    }

    let learning_openers = [
        "teach",
        "explain",
        "learn",
        "what is",
        "what are",
        "how does",
        "help me understand",
        "i want to learn",
        "tell me about",
    ];
    let build_terms = [
        "build",
        "create",
        "implement",
        "code",
        "write",
        "test",
        "verifier",
        "function",
        "app",
        "contract",
        "script",
        "patch",
        "debug",
        "ship",
        "generate a quest",
    ];

    learning_openers
        .iter()
        .any(|opener| normalized.starts_with(opener))
        && !build_terms.iter().any(|term| normalized.contains(term))
}

fn quest_prompt(
    build_prompt: &str,
    track: &str,
    difficulty: &Difficulty,
    learning_context: Option<&LearningQuestLink>,
    repair: bool,
) -> String {
    let nonce = Uuid::new_v4();
    let learning_context = learning_context
        .map(|context| {
            let concepts = if context.concepts.is_empty() {
                "none supplied".to_string()
            } else {
                context.concepts.join(", ")
            };
            format!(
                "LESSON-DERIVED QUEST CONTEXT: module='{module}', lesson='{lesson}', concepts='{concepts}', checkpoint='{checkpoint}', correct_answer='{answer}', misconception_to_test='{misunderstanding}', lesson_summary='{summary}', practice_quest_bridge='{bridge}'. Generate the quest from this exact lesson evidence, not from a generic template.",
                module = context.module_title.trim(),
                lesson = context.lesson_title.trim(),
                concepts = concepts,
                checkpoint = context.checkpoint_question.trim(),
                answer = context.correct_answer.trim(),
                misunderstanding = context.misunderstanding.trim(),
                summary = context.lesson_summary.trim(),
                bridge = context.quest_bridge.trim(),
            )
        })
        .unwrap_or_else(|| "Learning source: direct quest request. Generate from the user's requested build scenario.".to_string());
    let repair_directive = if repair {
        "Previous response failed validation because it was incomplete, generic, or missing a denial-focused generated test. Return a complete original quest object with all required fields and code."
    } else {
        ""
    };
    format!(
        r#"Return one minified JSON object only. No markdown. No prose outside JSON.

Request: {build_prompt}
Skill track: {track}
Difficulty: {difficulty:?}
{learning_context}
{repair_directive}
Seed: {nonce}

Top-level keys exactly: title, premise, build_objective, comprehension_gates, boss_fight, challenge_brief, code_explainer, reward_logic, ckb_fiber_hooks, workbench_files.

Hard rules:
- Every field must be authored for this exact request or lesson context. Do not use a generic paywall, generic quiz, stock variable names, or filler prose.
- For lesson-derived quests, invent names from the lesson. Do not use cellVerifier, verifyCkbCellProof, verifyGeneratedReceipt, src/quest.ts, test/quest.test.ts, ACTIVE_RUN_ID, LESSON_INVARIANT, Fiber Proof Run, Paywall Reactor, or titles ending in Practice Quest.
- title: specific to the generated quest, max 80 chars.
- premise: 2 concise sentences explaining the concrete protocol or integration risk the learner is practicing. If the lesson mentions TON, STON.fi, Omniston, or jettons, this must be a TON / STON.fi integration risk.
- build_objective: concrete objective from the request/lesson, max 420 chars.
- comprehension_gates: exactly 3 specific gates. Gate 1 explains the invariant. Gate 2 runs or reads the denial test. Gate 3 ships only after defending the generated diff.
- boss_fight: code-specific challenge tied to the verifier function and denial test.
- reward_logic: explain when XP/badge/reward claim unlocks, no fake payout promises.
- ckb_fiber_hooks: legacy schema name. Return exactly 2 concrete protocol hooks. If the lesson mentions TON, STON.fi, Omniston, or jettons, make them one TON Connect/wallet-state hook and one STON.fi SDK/widget/route hook. Otherwise use one CKB-side and one Fiber-side hook.
- workbench_files: exactly 2 files. One implementation file and one test file. Each has path, language, content. File paths must be specific to the lesson scenario.
- implementation content: TypeScript or Rust, 45-95 lines max. Export types and one verifier/settlement function whose name is specific to the lesson. It must mention concrete selected-domain terms. For TON / STON.fi lessons, use terms such as TON Connect, manifest, wallet approval, jetton master, jetton wallet, STON.fi, Omniston, quote, route, minOut, slippage, stale quote, referral fee, transaction hash, pending, or confirmed. For CKB/Fiber lessons, use terms such as cell, OutPoint, witness, script, xUDT, invoice, PTLC/HTLC, channel state, nonce, JoyID challenge, receipt, or payout.
- test content: 35-85 lines max. Import/call the implementation. Include one valid case and at least two denial cases that mutate the exact trusted fields. Use words like reject, block, false, throw, invalid, unpaid, mismatch, or replay.
- code_explainer must have keys exactly: primary_invariant, denial_path, proof_label, proof_artifact, network_label, network_boundary, risk_focus, inspect_steps, mentor_prompts, resources.
- code_explainer must be custom to the generated files. inspect_steps has exactly 4 concrete reading steps. mentor_prompts has exactly 4 code-specific learner questions. resources has 2 objects with title, url, reason.
- challenge_brief must have keys exactly: question, correct_answer, wrong_answers, invariant, attack_scenario, code_focus, test_focus, hint, follow_up_question, resources.
- challenge_brief.question must name the generated function or trusted fields.
- challenge_brief.correct_answer must be the exact strongest answer and must not be answer A by default; the frontend will shuffle, so do not depend on order.
- wrong_answers: exactly 3 objects with label and feedback. Each wrong answer must be plausible and teach why it is unsafe.
- invariant, attack_scenario, code_focus, test_focus, hint, and follow_up_question must reference the generated code and denial test.
- resources: 2 objects with title, url, reason. Use official/meaningful CKB/Fiber/JoyID resources when relevant.
"#
    )
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            ApiError::InvalidPrompt
            | ApiError::LearningRequestNeedsModule
            | ApiError::MissingWalletAddress
            | ApiError::MissingWalletSignature
            | ApiError::InvalidWalletProofMessage
            | ApiError::UnsupportedWalletSignature
            | ApiError::InvalidWalletSignature
            | ApiError::MissingFiberInvoice
            | ApiError::InvalidFiberInvoice => StatusCode::BAD_REQUEST,
            ApiError::MissingOpenAiKey
            | ApiError::DatabaseUnavailable
            | ApiError::FiberPayoutUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::QuestNotFound => StatusCode::NOT_FOUND,
            ApiError::WalletMismatch => StatusCode::FORBIDDEN,
            ApiError::CompletionNotVerified | ApiError::RewardAlreadyProcessed => {
                StatusCode::CONFLICT
            }
            ApiError::OpenAiTransport(_)
            | ApiError::OpenAiStatus { .. }
            | ApiError::InvalidAiResponse => StatusCode::BAD_GATEWAY,
            ApiError::Database(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::FiberPayout(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };

        (
            status,
            Json(ErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use tower::ServiceExt;

    const ROUTER_TEST_KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn router_test_state(auth: AuthVerifier) -> Arc<AppState> {
        Arc::new(AppState {
            auth,
            runner: runner::RunnerService::disabled(),
            config: AppConfig {
                port: 8080,
                app_env: "test".to_string(),
                cors_origins: vec!["http://localhost:3000".to_string()],
                ckb_rpc_url: None,
                fiber_rpc_url: None,
                fiber_payout_rpc_url: None,
                fiber_payout_enabled: false,
                reward_amount_shannons: 400,
                reward_currency: "Fibd".to_string(),
                mongodb_uri: None,
                mongodb_database: "vibequest".to_string(),
                mongodb_v3_database: platform::DEFAULT_V3_DATABASE.to_string(),
            },
            registry: EcosystemRegistry::built_in().expect("valid test registry"),
            platform_store: PlatformStore::new(None, platform::DEFAULT_V3_DATABASE.to_string()),
            openai: OpenAiClient {
                http: Client::new(),
                api_key: None,
                model: DEFAULT_OPENAI_MODEL.to_string(),
                base_url: DEFAULT_OPENAI_BASE_URL.to_string(),
                reasoning_effort: DEFAULT_OPENAI_REASONING_EFFORT,
                disable_response_storage: true,
                timeout: Duration::from_secs(1),
            },
            fiber: FiberPayoutClient {
                http: Client::new(),
                rpc_url: None,
                enabled: false,
                timeout: Duration::from_secs(1),
            },
            store: MongoStore::disabled(),
        })
    }

    fn router_test_assertion() -> (AuthVerifier, String, String) {
        let encoded_key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ROUTER_TEST_KEY);
        let verifier = AuthVerifier::from_key_config(
            format!("router-test:{encoded_key}"),
            encoded_key,
            "vibequest-web".to_string(),
            "vibequest-core".to_string(),
        )
        .expect("valid router verifier");
        let provider_subject = "router-google-subject";
        let identity_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, ROUTER_TEST_KEY);
        let identity_digest = ring::hmac::sign(
            &identity_key,
            format!("google:{provider_subject}").as_bytes(),
        );
        let user_id = format!(
            "usr_{}",
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(identity_digest.as_ref())
                [..32]
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("current time")
            .as_secs() as i64;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "alg": "HS256",
                "typ": "JWT",
                "kid": "router-test"
            }))
            .expect("header"),
        );
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "iss": "vibequest-web",
                "aud": "vibequest-core",
                "sub": user_id,
                "provider": "google",
                "provider_sub": provider_subject,
                "email": "learner@example.com",
                "name": "Learner",
                "iat": now,
                "exp": now + 60,
                "jti": "router-assertion"
            }))
            .expect("claims"),
        );
        let input = format!("{header}.{payload}");
        let signing_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, ROUTER_TEST_KEY);
        let signature = ring::hmac::sign(&signing_key, input.as_bytes());
        let assertion = format!(
            "{input}.{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.as_ref())
        );

        (verifier, assertion, user_id)
    }

    #[tokio::test]
    async fn router_enforces_the_v3_identity_boundary() {
        let (verifier, assertion, expected_user_id) = router_test_assertion();
        let app = build_router(router_test_state(verifier));

        let catalog = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v3/catalog")
                    .body(Body::empty())
                    .expect("catalog request"),
            )
            .await
            .expect("catalog response");
        assert_eq!(catalog.status(), StatusCode::OK);
        let catalog_body = to_bytes(catalog.into_body(), 64 * 1024)
            .await
            .expect("catalog body");
        let catalog_json: serde_json::Value =
            serde_json::from_slice(&catalog_body).expect("catalog JSON");
        assert_eq!(
            catalog_json["ecosystems"][0]["tracks"][0]["runner_status"],
            "review-required"
        );
        assert_eq!(
            catalog_json["ecosystems"][0]["tracks"][0]["runner_version"],
            runner::RUNNER_VERSION
        );

        let curriculum = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v3/catalog/zcash/tracks/shielded-payments-safety/curriculum")
                    .body(Body::empty())
                    .expect("curriculum request"),
            )
            .await
            .expect("curriculum response");
        assert_eq!(curriculum.status(), StatusCode::OK);
        let curriculum_body = to_bytes(curriculum.into_body(), 256 * 1024)
            .await
            .expect("curriculum body");
        let curriculum_json: serde_json::Value =
            serde_json::from_slice(&curriculum_body).expect("curriculum JSON");
        assert_eq!(curriculum_json["lessons"].as_array().map(Vec::len), Some(5));
        assert_eq!(
            curriculum_json["runner_manifest_version"],
            runner::RUNNER_MANIFEST_VERSION
        );
        let curriculum_text =
            String::from_utf8(curriculum_body.to_vec()).expect("curriculum UTF-8");
        assert!(!curriculum_text.contains("correct_option_id"));
        assert!(!curriculum_text.contains("seeded_defects"));
        assert!(!curriculum_text.contains("privacy-google-wallet-linkage"));

        let spoofed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v3/me")
                    .header("x-vibequest-user-id", "usr_attacker")
                    .body(Body::empty())
                    .expect("spoofed request"),
            )
            .await
            .expect("spoofed response");
        assert_eq!(spoofed.status(), StatusCode::UNAUTHORIZED);

        let authenticated = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v3/me")
                    .header(header::AUTHORIZATION, format!("Bearer {assertion}"))
                    .body(Body::empty())
                    .expect("authenticated request"),
            )
            .await
            .expect("authenticated response");
        assert_eq!(authenticated.status(), StatusCode::OK);
        let authenticated_body = to_bytes(authenticated.into_body(), 64 * 1024)
            .await
            .expect("authenticated body");
        let authenticated_json: serde_json::Value =
            serde_json::from_slice(&authenticated_body).expect("identity JSON");
        assert_eq!(authenticated_json["user_id"], expected_user_id);
        assert_eq!(authenticated_json["persistence_enabled"], false);

        let legacy = app
            .oneshot(
                Request::builder()
                    .uri("/users/legacy-wallet/quests")
                    .body(Body::empty())
                    .expect("legacy request"),
            )
            .await
            .expect("legacy response");
        assert_eq!(legacy.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn authenticated_submissions_fail_closed_pending_runner_review() {
        let (verifier, assertion, _) = router_test_assertion();
        let app = build_router(router_test_state(verifier));
        let body = serde_json::to_vec(&serde_json::json!({
            "scenario_id": runner::RUNNER_SCENARIO_ID,
            "scenario_manifest_version": runner::runner_manifest().scenario_manifest_version,
            "source": runner::SOLUTION_SOURCE
        }))
        .expect("submission JSON");
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v3/submissions")
                    .header(header::AUTHORIZATION, format!("Bearer {assertion}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("submission request"),
            )
            .await
            .expect("submission response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        let response_body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("runner error body");
        let response_json: serde_json::Value =
            serde_json::from_slice(&response_body).expect("runner error JSON");
        assert_eq!(response_json["code"], "runner-review-required");
    }

    fn ai_impl_fixture() -> String {
        "export type Receipt={reader:string;contentId:string;runId:string;ckbCell:string;invoice:string;preimage:string;channelState:string;witness:string;nonce:string};\nexport type Claim={reader:string;contentId:string;runId:string;ckbCell:string;nonce:string};\nexport function auditReceiptAccess(receipt:Receipt|undefined,claim:Claim){\n  if(!receipt)return false;\n  const sameReader=receipt.reader===claim.reader;\n  const sameContent=receipt.contentId===claim.contentId;\n  const sameRun=receipt.runId===claim.runId;\n  const sameNonce=receipt.nonce===claim.nonce&&receipt.witness.includes(claim.nonce);\n  const sameCell=receipt.ckbCell===claim.ckbCell&&receipt.channelState.includes(claim.ckbCell)&&receipt.witness.includes(claim.ckbCell);\n  const fiberProof=receipt.invoice.startsWith('fiber:')&&receipt.preimage.startsWith('PTLC-proof-');\n  return sameReader&&sameContent&&sameRun&&sameNonce&&sameCell&&fiberProof;\n}".to_string()
    }

    fn ai_test_fixture() -> String {
        "import {auditReceiptAccess,type Receipt,type Claim} from '../src/fiberReceiptAccess';\nconst claim:Claim={reader:'alice',contentId:'lesson',runId:'vq-__RUN_SEED__',ckbCell:'cell:__RUN_SEED__',nonce:'nonce-__RUN_SEED__'};\nconst receipt:Receipt={...claim,invoice:'fiber:invoice-__RUN_SEED__',preimage:'PTLC-proof-__RUN_SEED__',channelState:'open:cell:__RUN_SEED__',witness:'ckb-witness:cell:__RUN_SEED__:nonce-__RUN_SEED__'};\ntest('accepts Fiber receipt bound to reader content run nonce and CKB cell',()=>expect(auditReceiptAccess(receipt,claim)).toBe(true));\ntest('rejects replay with wrong run id',()=>expect(auditReceiptAccess({...receipt,runId:'vq-old'},claim)).toBe(false));\ntest('blocks mismatched CKB cell state',()=>expect(auditReceiptAccess({...receipt,ckbCell:'cell:attacker'},claim)).toBe(false));\ntest('rejects replay with copied PTLC proof but wrong nonce',()=>expect(auditReceiptAccess({...receipt,nonce:'nonce-attacker'},claim)).toBe(false));".to_string()
    }

    fn ai_quest_json(title: &str) -> String {
        serde_json::json!({
            "title": title,
            "premise": "A learner inspects an AI-generated Fiber receipt verifier and proves it cannot replay a CKB cell proof across another run. The quest is generated for this request, then blocked unless denial tests and the boss answer match the code.",
            "build_objective": "Build a Fiber paid content verifier that binds reader, content, run id, nonce, CKB cell witness, and PTLC payment evidence.",
            "comprehension_gates": [
                "Explain how reader, content, run id, nonce, CKB cell, witness, invoice, and PTLC proof form one invariant.",
                "Verify the generated denial tests mutate run id, nonce, and CKB cell evidence instead of only checking the happy path.",
                "Ship only after defending why copied Fiber proof material cannot unlock another content item or quest run."
            ],
            "boss_fight": "In auditReceiptAccess, explain why a copied PTLC preimage is insufficient unless the receipt also binds nonce, run id, reader, content, CKB cell, witness, and channel state.",
            "challenge_brief": {
                "question": "What must auditReceiptAccess prove before this generated Fiber/CKB quest is safe to ship?",
                "correct_answer": "It must bind reader, contentId, runId, nonce, CKB cell witness, Fiber invoice, PTLC proof, and channelState, then reject denial tests that mutate run id, nonce, or cell evidence.",
                "wrong_answers": [
                    {"label":"Trust the connected JoyID wallet because the UI shows the learner address.","feedback":"Wallet presence does not prove this receipt, nonce, CKB cell, or Fiber payment state belongs to the requested access grant."},
                    {"label":"Accept any invoice that starts with fiber: and any PTLC-looking preimage.","feedback":"A payment-looking string can be copied unless it is scoped to reader, content, run id, cell evidence, nonce, and channel state."},
                    {"label":"Ship once the valid receipt test passes.","feedback":"The learner needs denial tests that mutate the same fields the verifier trusts, especially run id, nonce, and CKB cell witness data."}
                ],
                "invariant": "The receipt must bind actor, content, run, nonce, CKB cell witness, invoice, PTLC proof, and channel state.",
                "attack_scenario": "An attacker copies a valid PTLC proof into another request while changing run id, nonce, or CKB cell state to unlock unpaid content.",
                "code_focus": "Trace auditReceiptAccess from its final return back through sameRun, sameNonce, sameCell, and fiberProof.",
                "test_focus": "Inspect the denial tests that mutate run id, CKB cell, and nonce while reusing valid-looking payment fields.",
                "hint": "Start from the accepting return statement and list every trusted field that must be impossible to replay independently.",
                "follow_up_question": "Which additional field would you mutate to prove the verifier rejects a copied receipt across another content item?",
                "resources": [
                    {"title":"CKB Docs","url":"https://docs.nervos.org/","reason":"Reference cells, scripts, witnesses, transactions, and token state."},
                    {"title":"Fiber Network Repository","url":"https://github.com/nervosnetwork/fiber","reason":"Reference payment channels, invoices, PTLC-based security, routing, and node behavior."}
                ]
            },
            "code_explainer": {
                "primary_invariant": "auditReceiptAccess must bind reader, contentId, runId, nonce, CKB cell witness, Fiber invoice, PTLC proof, and channelState before allowing paid access.",
                "denial_path": "The denial tests mutate runId, ckbCell, and nonce while reusing valid-looking payment evidence, proving replayed receipts are rejected.",
                "proof_label": "Receipt proof",
                "proof_artifact": "The artifact is a Fiber invoice/PTLC proof plus CKB witness and channel state scoped to one claim.",
                "network_label": "CKB/Fiber boundary",
                "network_boundary": "CKB witness and cell state identify the chain evidence; Fiber invoice, PTLC proof, and channel state represent off-chain payment evidence.",
                "risk_focus": "receipt replay across run, nonce, and CKB cell evidence",
                "inspect_steps": [
                    "Trace auditReceiptAccess from the final return statement back to sameRun, sameNonce, sameCell, and fiberProof.",
                    "Match each trusted receipt field to the Claim object and the witness/channel evidence.",
                    "Read the denial tests that mutate runId, ckbCell, and nonce while reusing payment-looking strings.",
                    "Explain why a PTLC preimage alone cannot authorize another reader, content item, or CKB cell."
                ],
                "mentor_prompts": [
                    "Why is nonce binding needed here?",
                    "Which field blocks a copied receipt first?",
                    "What is CKB evidence versus Fiber evidence?",
                    "How would you add a content replay test?"
                ],
                "resources": [
                    {"title":"CKB Docs","url":"https://docs.nervos.org/","reason":"Reference cells, scripts, witnesses, transactions, and token state."},
                    {"title":"Fiber Network Repository","url":"https://github.com/nervosnetwork/fiber","reason":"Reference payment channels, invoices, PTLC-based security, routing, and node behavior."}
                ]
            },
            "reward_logic": "XP and badge state unlock only after generated file checks pass and the learner answers the boss challenge from the generated verifier and denial tests.",
            "ckb_fiber_hooks": [
                "CKB side: witness and cell state must bind the accepted receipt to the expected cell evidence.",
                "Fiber side: invoice, PTLC proof, nonce, and channel state must bind payment evidence to the requested access grant."
            ],
            "workbench_files": [
                {"path":"src/fiberReceiptAccess.ts","language":"ts","content":ai_impl_fixture()},
                {"path":"test/fiberReceiptAccess.test.ts","language":"ts","content":ai_test_fixture()}
            ]
        })
        .to_string()
    }

    #[test]
    fn parses_openai_output_text() {
        let body = serde_json::json!({
            "output_text": ai_quest_json("Receipt Raid"),
            "output": null
        })
        .to_string();

        let parsed = parse_openai_json_response::<QuestBlueprint>(&body).unwrap();

        assert_eq!(parsed.title, "Receipt Raid");
        assert!(
            parsed.workbench_files[0]
                .content
                .contains("auditReceiptAccess")
        );
    }

    #[test]
    fn parses_openai_nested_output_text() {
        let body = serde_json::json!({
            "output_text": null,
            "output": [{
                "content": [{
                    "type": "output_text",
                    "text": ai_quest_json("Witness Lab")
                }]
            }]
        })
        .to_string();

        let parsed = parse_openai_json_response::<QuestBlueprint>(&body).unwrap();

        assert_eq!(parsed.title, "Witness Lab");
        assert!(
            parsed
                .challenge_brief
                .unwrap()
                .correct_answer
                .contains("nonce")
        );
    }

    #[test]
    fn parses_openai_json_when_provider_wraps_text() {
        let body = serde_json::json!({
            "output_text": format!("Here is the generated quest:\n{}\nDone.", ai_quest_json("Split Lab")),
            "output": null
        })
        .to_string();

        let parsed = parse_openai_json_response::<QuestBlueprint>(&body).unwrap();

        assert_eq!(parsed.title, "Split Lab");
    }

    #[test]
    fn compact_ai_quest_blueprint_keeps_authored_challenge() {
        let quest =
            serde_json::from_str::<QuestBlueprint>(&ai_quest_json("AI Authored Fiber Receipt Lab"))
                .unwrap();
        let compacted = compact_quest_blueprint(quest, None).unwrap();

        assert_eq!(compacted.workbench_files.len(), 2);
        assert_eq!(compacted.comprehension_gates.len(), 3);
        assert_eq!(
            compacted
                .challenge_brief
                .as_ref()
                .unwrap()
                .wrong_answers
                .len(),
            3
        );
        assert!(compacted.boss_fight.contains("auditReceiptAccess"));
    }

    #[test]
    fn parses_provider_config_values() {
        assert_eq!(
            ReasoningEffort::parse("xhigh"),
            Some(ReasoningEffort::Xhigh)
        );
        assert_eq!(
            ReasoningEffort::parse(" HIGH "),
            Some(ReasoningEffort::High)
        );
        assert_eq!(
            ReasoningEffort::Xhigh.serverless_safe(),
            ReasoningEffort::Minimal
        );
        assert_eq!(
            ReasoningEffort::High.serverless_safe(),
            ReasoningEffort::Minimal
        );
        assert_eq!(
            ReasoningEffort::Medium.serverless_safe(),
            ReasoningEffort::Minimal
        );
    }

    #[test]
    fn provider_metadata_redacts_secret_endpoint_details() {
        let client = OpenAiClient {
            http: Client::new(),
            api_key: Some("sk-test-secret".to_string()),
            model: "open-model".to_string(),
            base_url: "https://share-ai.ckbdev.com/openai/v1?api_key=leaked".to_string(),
            reasoning_effort: ReasoningEffort::Minimal,
            disable_response_storage: true,
            timeout: Duration::from_secs(42),
        };

        let metadata = client.provider_metadata();
        let serialized = serde_json::to_string(&metadata).expect("metadata serializes");

        assert_eq!(metadata.provider_kind, "openai-compatible");
        assert_eq!(metadata.endpoint_origin, "https://share-ai.ckbdev.com");
        assert_eq!(metadata.timeout_seconds, 42);
        assert!(metadata.response_storage_disabled);
        assert!(metadata.configured);
        assert!(!serialized.contains("sk-test-secret"));
        assert!(!serialized.contains("leaked"));
        assert!(!serialized.contains("api_key"));
    }

    #[test]
    fn learning_eval_artifact_hashes_request_and_excludes_identity() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("zcash".to_string()),
            ecosystem_id: Some("zcash".to_string()),
            topic: Some("Shielded checkout".to_string()),
            learning_profile: Some("Backend dev".to_string()),
            learning_intents: vec!["Understand payment evidence".to_string()],
            interests: vec!["Zcash".to_string()],
            learner_goal: "Teach learner@example.com how to validate shielded checkout evidence"
                .to_string(),
            background: "Backend".to_string(),
            pace: "Focused".to_string(),
        };
        let lesson = reviewed_learning_lesson("lesson-1", "Shielded checkout source boundary");
        let module = LearningModule {
            title: "Shielded checkout".to_string(),
            learner_profile: "Backend dev".to_string(),
            outcome: "Validate payment evidence".to_string(),
            lessons: vec![lesson],
            capstone_quest_prompt: "Prove the payment boundary".to_string(),
            resources: vec![LearningResource {
                title: "Zcash Documentation".to_string(),
                url: "https://zcash.readthedocs.io/".to_string(),
                reason: "Official Zcash reference".to_string(),
            }],
        };
        let provider = AiProviderMetadata {
            provider_kind: "openai-compatible".to_string(),
            model: "test-model".to_string(),
            endpoint_origin: "https://share-ai.ckbdev.com".to_string(),
            reasoning_effort: ReasoningEffort::Minimal,
            response_storage_disabled: true,
            timeout_seconds: 90,
            configured: true,
        };

        let artifact = learning_eval_artifact(&request, &module, provider);
        let serialized = serde_json::to_string(&artifact).expect("artifact serializes");

        assert_eq!(artifact.artifact_version, "vibequest-learning-eval-v1");
        assert_eq!(artifact.ecosystem_id, "zcash");
        assert_eq!(artifact.lesson_count, 1);
        assert_eq!(artifact.request_hash.len(), 64);
        assert!(!serialized.contains("learner@example.com"));
        assert!(!serialized.contains("Teach learner"));
        assert!(!serialized.contains("api_key"));
        assert!(serialized.contains("Zcash Documentation"));
    }

    #[test]
    fn learning_eval_artifact_flags_repeated_lessons() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("zcash".to_string()),
            ecosystem_id: Some("zcash".to_string()),
            topic: Some("Shielded checkout".to_string()),
            learning_profile: Some("Security auditor".to_string()),
            learning_intents: vec!["Find repeated generated lessons".to_string()],
            interests: vec!["Zcash".to_string()],
            learner_goal: "Audit generated shielded checkout lessons".to_string(),
            background: "Security".to_string(),
            pace: "Deep dive".to_string(),
        };
        let first = reviewed_learning_lesson("lesson-1", "Repeated boundary lesson");
        let second = reviewed_learning_lesson("lesson-2", "Repeated boundary lesson");
        let module = LearningModule {
            title: "Shielded checkout".to_string(),
            learner_profile: "Security auditor".to_string(),
            outcome: "Audit generated lessons".to_string(),
            lessons: vec![first, second],
            capstone_quest_prompt: "Prove uniqueness".to_string(),
            resources: Vec::new(),
        };
        let provider = AiProviderMetadata {
            provider_kind: "openai-compatible".to_string(),
            model: "test-model".to_string(),
            endpoint_origin: "https://share-ai.ckbdev.com".to_string(),
            reasoning_effort: ReasoningEffort::Minimal,
            response_storage_disabled: true,
            timeout_seconds: 90,
            configured: true,
        };

        let artifact = learning_eval_artifact(&request, &module, provider);

        assert!(!artifact.validation.repetition_check);
        assert!(!artifact.validation.passed);
        assert!(
            artifact
                .warnings
                .iter()
                .any(|warning| warning.contains("repeated lesson titles"))
        );
    }

    #[test]
    fn missing_integrations_include_ckb_and_fiber() {
        let state = AppState {
            auth: AuthVerifier::disabled(),
            runner: runner::RunnerService::disabled(),
            config: AppConfig {
                port: 8080,
                app_env: "test".to_string(),
                cors_origins: vec!["http://localhost:3000".to_string()],
                ckb_rpc_url: None,
                fiber_rpc_url: None,
                fiber_payout_rpc_url: None,
                fiber_payout_enabled: false,
                reward_amount_shannons: 400,
                reward_currency: "Fibd".to_string(),
                mongodb_uri: None,
                mongodb_database: "vibequest".to_string(),
                mongodb_v3_database: platform::DEFAULT_V3_DATABASE.to_string(),
            },
            registry: EcosystemRegistry::built_in().expect("valid test registry"),
            platform_store: PlatformStore::new(None, platform::DEFAULT_V3_DATABASE.to_string()),
            openai: OpenAiClient {
                http: Client::new(),
                api_key: Some("key".to_string()),
                model: DEFAULT_OPENAI_MODEL.to_string(),
                base_url: DEFAULT_OPENAI_BASE_URL.to_string(),
                reasoning_effort: DEFAULT_OPENAI_REASONING_EFFORT,
                disable_response_storage: true,
                timeout: Duration::from_secs(1),
            },
            fiber: FiberPayoutClient {
                http: Client::new(),
                rpc_url: None,
                enabled: false,
                timeout: Duration::from_secs(1),
            },
            store: MongoStore::disabled(),
        };

        let integrations = IntegrationStatus {
            openai: true,
            ckb_rpc: false,
            fiber_rpc: false,
            fiber_payout: false,
            mongodb: false,
        };

        assert_eq!(
            missing_integrations(&state, &integrations),
            vec!["CKB_RPC_URL", "FIBER_RPC_URL", "MONGODB_URI"]
        );
    }

    #[test]
    fn checkpoint_answers_round_trip_through_document() {
        let values = std::collections::BTreeMap::from([
            ("lesson-1".to_string(), 2_i64),
            ("lesson-2".to_string(), 0_i64),
        ]);
        let document = checkpoint_answers_document(values.clone());

        assert_eq!(document_to_checkpoint_answers(document), values);
    }

    fn reviewed_learning_lesson(id: &str, title: &str) -> LearningLesson {
        LearningLesson {
            id: id.to_string(),
            title: title.to_string(),
            why_it_matters: "This reviewed lesson names the source-grounded trust boundary clearly.".to_string(),
            explanation: "Concept: official source pack evidence. How it works: the learner separates protocol evidence from app state. Common mistake: trusting a frontend flag. Denial test: mutate the authoritative field and reject unsafe reuse. Further study: official documentation.".to_string(),
            concepts: vec!["source pack".to_string(), "trust boundary".to_string()],
            submodules: Vec::new(),
            resources: default_learning_resources_for_focus("Zcash"),
            evidence_map: vec![LearningEvidence {
                claim: "The lesson is grounded in official source-pack guidance.".to_string(),
                source_title: "Zcash Documentation".to_string(),
                source_url: "https://zcash.readthedocs.io/".to_string(),
                lesson_section: "lesson body".to_string(),
                confidence: "source-pack".to_string(),
            }],
            quality_score: LearningQualityScore {
                source_coverage: 100,
                technical_depth: 92,
                checkpoint_quality: 95,
                placeholder_free: true,
                ecosystem_alignment: true,
                passed: true,
            },
            checkpoint: LearningCheckpoint {
                question: "Which Zcash shielded checkout fields form the trusted boundary?".to_string(),
                options: vec![
                    LearningOption { label: "ZIP-321 request, shielded address, memo policy, viewing-key limit, and confirmation evidence".to_string(), feedback: "Correct.".to_string() },
                    LearningOption { label: "The frontend paid flag".to_string(), feedback: "Unsafe.".to_string() },
                    LearningOption { label: "Any explorer screenshot".to_string(), feedback: "Incomplete.".to_string() },
                    LearningOption { label: "The user display name".to_string(), feedback: "Irrelevant.".to_string() },
                ],
                correct_index: 0,
                explanation: "The trusted boundary is source-grounded payment evidence, not frontend state.".to_string(),
                follow_up_question: "Which denial case would catch a wrong memo?".to_string(),
            },
            quest_bridge: "Build a denial test that mutates memo and address evidence.".to_string(),
        }
    }

    #[test]
    fn module_generation_statuses_preserve_failed_and_validated_modules() {
        let lesson =
            reviewed_learning_lesson("module-1-lesson-1", "Shielded checkout source boundary");
        let module = LearningModule {
            title: "Zcash shielded checkout".to_string(),
            learner_profile: "Reviewer".to_string(),
            outcome: "Validate source-grounded learning status.".to_string(),
            lessons: vec![lesson],
            capstone_quest_prompt: "Final reviewed quest".to_string(),
            resources: default_learning_resources_for_focus("Zcash"),
        };

        let statuses = compact_module_generation_statuses(
            vec![LearningModuleGenerationState {
                lesson_index: 2,
                lesson_id: None,
                status: "failed".to_string(),
                validation: LearningModuleValidationState::default(),
                error: Some("provider timeout".to_string()),
                updated_at: String::new(),
            }],
            &module,
            5,
        );

        assert_eq!(statuses.len(), 5);
        assert_eq!(statuses[0].status, "validated");
        assert!(statuses[0].validation.passed);
        assert_eq!(statuses[1].status, "queued");
        assert_eq!(statuses[2].status, "failed");
        assert_eq!(statuses[2].error.as_deref(), Some("provider timeout"));
    }

    #[test]
    fn learning_metrics_summary_counts_core_events() {
        let now = Utc::now();
        let events = vec![
            LearningEventRecord {
                event_id: "1".to_string(),
                event_type: "course_generated".to_string(),
                module_id: Some("course-a".to_string()),
                lesson_id: None,
                ecosystem_id: Some("zcash".to_string()),
                course_title: None,
                metadata: BTreeMap::new(),
                created_at: now,
            },
            LearningEventRecord {
                event_id: "2".to_string(),
                event_type: "checkpoint_attempted".to_string(),
                module_id: Some("course-a".to_string()),
                lesson_id: Some("lesson-a".to_string()),
                ecosystem_id: Some("zcash".to_string()),
                course_title: None,
                metadata: BTreeMap::new(),
                created_at: now,
            },
            LearningEventRecord {
                event_id: "3".to_string(),
                event_type: "checkpoint_passed".to_string(),
                module_id: Some("course-a".to_string()),
                lesson_id: Some("lesson-a".to_string()),
                ecosystem_id: Some("zcash".to_string()),
                course_title: None,
                metadata: BTreeMap::new(),
                created_at: now,
            },
            LearningEventRecord {
                event_id: "4".to_string(),
                event_type: "tutor_used".to_string(),
                module_id: Some("course-a".to_string()),
                lesson_id: Some("lesson-a".to_string()),
                ecosystem_id: Some("zcash".to_string()),
                course_title: None,
                metadata: BTreeMap::new(),
                created_at: now,
            },
        ];

        let summary = summarize_learning_events(&events);
        assert_eq!(summary.total_events, 4);
        assert_eq!(summary.courses_generated, 1);
        assert_eq!(summary.checkpoints_attempted, 1);
        assert_eq!(summary.checkpoints_passed, 1);
        assert_eq!(summary.tutor_used, 1);
        assert_eq!(summary.by_ecosystem.get("zcash"), Some(&4));
    }

    #[test]
    fn compact_tutor_messages_keeps_recent_valid_messages() {
        let now = Utc::now();
        let messages = (0..40)
            .map(|index| LearningTutorMessage {
                id: format!("m-{index}"),
                role: if index % 2 == 0 { "learner" } else { "mentor" }.to_string(),
                text: format!("message {index}"),
                why: None,
                follow_up: None,
                module_id: Some("ckb-cells".to_string()),
                module_title: Some("AI CKB Cells Focus".to_string()),
                lesson_id: Some("ckb-cells-lesson-1".to_string()),
                lesson_title: Some("AI Generated Cell State Drill".to_string()),
                created_at: now,
            })
            .collect::<Vec<_>>();

        let compacted = compact_tutor_messages(messages);
        assert_eq!(compacted.len(), 30);
        assert_eq!(compacted.first().unwrap().id, "m-10");
        assert_eq!(
            compacted.first().unwrap().lesson_id.as_deref(),
            Some("ckb-cells-lesson-1")
        );
    }

    #[test]
    fn compact_ai_module_builder_keeps_authored_text() {
        let request = GenerateLearningModuleRequest {
            path_id: None,
            ecosystem_id: Some("ckb-fiber".to_string()),
            topic: Some("CKB/Fiber verifier code".to_string()),
            learning_profile: Some("Backend dev".to_string()),
            learning_intents: vec!["Understand generated CKB/Fiber verifier code".to_string()],
            interests: vec!["CKB Foundations".to_string(), "Fiber Payments".to_string()],
            learner_goal: "Understand generated CKB/Fiber verifier code".to_string(),
            background: "Backend dev".to_string(),
            pace: "Deep dive".to_string(),
        };
        let ckb_fiber_body = |role: &str| -> String {
            format!(
                "{} Accuracy check: verify each claim against official CKB docs, the Fiber Network repository, and JoyID signing guidance before trusting the generated verifier. Submodule path: source claim inventory -> protocol evidence map -> app state denial -> regression test. Further study: read official CKB transaction documentation, the Fiber Network repository, and JoyID challenge signing rules.",
                role.repeat(12)
            )
        };
        let lesson_specs = [
            (
                "CKB/Fiber evidence inventory",
                "Backend checks should treat a JoyID-signed payload as evidence for one exact action, not as a reusable login token. In a CKB/Fiber flow, the learner should trace the CKB cell, Fiber channel, wallet address, challenge nonce, domain, action name, and evidence source before signature verification. The failure mode is subtle: a vibe-coded route may accept a database flag while the protocol evidence belongs to another cell or channel. The denial test should reject unsafe mismatches between CKB state and Fiber channel evidence. ",
                "const evidence = bindCkbCellToFiberChannel({ cell, channel, nonce });",
                "Which CKB cell, Fiber channel, nonce, and evidence source must be bound before accepting the first proof boundary?",
                "The exact CKB cell, Fiber channel evidence, nonce, signer, and server-side source of truth",
            ),
            (
                "OutPoint and invoice lineage",
                "A backend verifier must keep OutPoint lineage separate from an invoice string because a transaction input, output, amount, payment request, and Fiber payment route do not prove the same thing automatically. The common shortcut is to store an invoice as paid and forget to verify the CKB transaction evidence tied to the same run. The denial test mutates the OutPoint, invoice amount, transaction hash, or payment context and expects a hard reject before user progress changes. ",
                "const ok = verifyOutPointInvoicePair({ outPoint, invoice, amount, txHash });",
                "Which OutPoint, invoice, transaction, payment amount, and run id fields prevent copied payment lineage?",
                "The exact OutPoint, invoice amount, transaction context, payment route, and run id",
            ),
            (
                "Script witness and JoyID signature scope",
                "Generated code should not collapse script execution, witness parsing, and JoyID signature verification into one vague authenticated flag. A lock script, type script, witness, signer challenge, and action scope each answer a different verification question. The unsafe failure mode is a valid signature replayed over a request whose witness or script group changed. The denial test changes the witness, script hash, JoyID challenge, or action label and requires the verifier to reject the request. ",
                "const signed = verifyJoyIDWitnessScope({ scriptHash, witness, challenge, signature });",
                "Which script, witness, signature, and JoyID challenge fields prove the user approved this exact action?",
                "The script hash, parsed witness, exact JoyID challenge, signature, nonce, and action scope",
            ),
            (
                "Stale proof and replay denial",
                "The pragmatic audit question is not whether the happy path works but whether stale, copied, replayed, or mismatched proof material fails before state changes. CKB/Fiber apps are vulnerable when generated code accepts old channel state, an old witness, or a copied invoice because the frontend says the learner finished. The denial test mutates stale channel state, replay nonce, copied witness, and mismatched signer context until the verifier proves it rejects unsafe reuse. ",
                "const denied = rejectReplay({ nonce, witness, channelState, signer, runId });",
                "Which stale, replayed, copied, or mismatched CKB/Fiber fields must fail the denial path?",
                "The stale witness, replay nonce, copied invoice proof, mismatched channel state, and wrong run id",
            ),
            (
                "Verifier quest proof package",
                "The final module turns the learning into a quest artifact: a verifier, source-grounded proof map, denial test suite, and explanation of why the proof cannot be faked by frontend state. The learner must show which CKB evidence, Fiber payment proof, JoyID signature, server run id, and completion state are authoritative. The failure case is accepting a quest result when a copied proof package passes visual checks but fails official-source verification. ",
                "const questReady = verifyQuestProofPackage({ proofMap, denialTests, sourcePack });",
                "Which verifier, denial test, proof package, and official source evidence make the final quest safe?",
                "The verifier output, passing denial tests, official source map, proof package, and server-owned completion state",
            ),
        ];
        let compact = AiLearningModuleCompact {
            t: "CKB Fiber JoyID from vibecoded code".to_string(),
            l: lesson_specs
                .iter()
                .enumerate()
                .map(|(index, (title, body, code, question, answer))| AiLearningLessonCompact {
                    t: (*title).to_string(),
                    e: ckb_fiber_body(body),
                    s: (*code).to_string(),
                    w: "For a backend developer, this matters because generated wallet and payment code can confuse signer presence with authorization. The lesson forces the learner to connect JoyID proof, CKB state evidence, Fiber payment context, source checks, and denial tests before trusting a generated verifier.".to_string(),
                    j: "Generate a verifier quest that binds JoyID challenge fields to CKB cell evidence and Fiber invoice context, then rejects replayed run ids or mismatched payment state.".to_string(),
                    f: "Which signed or protocol field would you mutate first to prove the verifier rejects unsafe reuse?".to_string(),
                    q: (*question).to_string(),
                    a: (*answer).to_string(),
                    b: vec!["CSS theme".to_string(), "Gas price chart".to_string(), "Frontend route".to_string()],
                    bf: vec![
                        "Visual styling does not prove signer intent, CKB state, or Fiber payment context.".to_string(),
                        "Gas information is not the authorization boundary for a generated verifier.".to_string(),
                        "A frontend route can be bypassed; backend proof binding must enforce the action.".to_string(),
                    ],
                    ci: index % 4,
                })
                .collect(),
        };

        let mut prior_lessons = Vec::new();
        for (index, lesson) in compact.l.iter().enumerate() {
            validate_ai_learning_lesson_compact_for_request_with_context(
                &request,
                lesson,
                index,
                &prior_lessons,
            )
            .unwrap_or_else(|error| panic!("lesson {index} failed validation: {error:?}"));
            prior_lessons.push(prior_learning_lesson_from_compact(lesson));
        }

        let module = build_learning_module_from_compact_ai(&request, compact).unwrap();

        assert_eq!(module.lessons.len(), 5);
        assert!(
            module.lessons[0]
                .explanation
                .contains("JoyID-signed payload")
        );
        assert!(module.lessons[0].explanation.split_whitespace().count() >= 500);
        assert!(module.lessons[0].explanation.contains("Code lens:"));
        assert!(
            module.lessons[0]
                .explanation
                .contains("bindCkbCellToFiberChannel")
        );
        assert_eq!(module.lessons[0].checkpoint.options.len(), 4);
        assert!(
            module.lessons[0]
                .why_it_matters
                .to_lowercase()
                .contains("backend")
        );
    }

    fn quality_gate_test_lesson() -> AiLearningLessonCompact {
        let sentence = "A Zcash shielded checkout verifier must bind the ZIP-321 request, zatoshi amount, shielded address, memo policy, viewing-key boundary, confirmation depth, and network before generated code marks an order as paid. It must reject unsafe malformed requests, wrong-network recipients, replayed evidence, and mismatched payment state instead of trusting frontend state. ";
        let body = format!(
            "{} Accuracy check: verify each claim against the official Zcash documentation and ZIP-321 payment request standard before shipping the generated checkout verifier. Submodule path: request parsing -> address policy -> memo safety -> denial tests. Further study: read the official Zcash documentation and ZIP-321 payment request standard.",
            sentence.repeat(72)
        );

        AiLearningLessonCompact {
            t: "ZIP-321 shielded checkout trust boundary".to_string(),
            e: body,
            s: "export function verifyPayment(request: string) {\n  // TODO: bind the zatoshi amount before accepting\n  return parseZip321(request).network === 'testnet';\n}".to_string(),
            w: "For a backend developer, this matters because generated checkout code can confuse a frontend paid flag with protocol evidence. The lesson forces the learner to bind request fields, network, amount, memo policy, and confirmation state before accepting payment.".to_string(),
            j: "Build a shielded checkout verifier artifact and denial tests that mutate the ZIP-321 amount, recipient network, memo handling, and confirmation state before the order can unlock.".to_string(),
            f: "Which checkout field would you mutate first to prove the verifier rejects unsafe payment evidence?".to_string(),
            q: "Which ZIP-321 request, zatoshi amount, shielded address, memo, viewing key, network, and confirmation fields form the checkout proof boundary?".to_string(),
            a: "The bound ZIP-321 recipient, zatoshi amount, memo policy, viewing-key scope, network, and confirmation state".to_string(),
            b: vec![
                "The frontend paid button text".to_string(),
                "The user's Google profile name".to_string(),
                "Any transaction hash pasted into the form".to_string(),
            ],
            bf: vec![
                "Button text is application state, not Zcash payment evidence.".to_string(),
                "Google identity can bind a learner account, but it cannot prove a shielded payment occurred.".to_string(),
                "A transaction hash alone does not prove the expected recipient, amount, memo policy, or confirmations.".to_string(),
            ],
            ci: 2,
        }
    }

    #[test]
    fn ai_learning_quality_rejects_placeholder_prose() {
        let mut lesson = quality_gate_test_lesson();
        lesson
            .e
            .push_str(" placeholder content should never pass a production learning gate.");

        assert!(validate_ai_learning_lesson_compact(&lesson).is_err());
    }

    #[test]
    fn ai_learning_quality_allows_intentional_code_todo_only_in_code_lens() {
        let lesson = quality_gate_test_lesson();

        assert!(lesson.s.contains("TODO"));
        assert!(validate_ai_learning_lesson_compact(&lesson).is_ok());
    }

    #[test]
    fn ai_learning_quality_rejects_unsourced_lesson_claims() {
        let mut lesson = quality_gate_test_lesson();
        lesson.e = lesson
            .e
            .replace(
                "official Zcash documentation and ZIP-321 payment request standard",
                "popular blog posts",
            )
            .replace(
                "official Zcash documentation and ZIP-321 payment request standard",
                "community notes",
            );

        assert!(validate_ai_learning_lesson_compact(&lesson).is_err());
    }

    #[test]
    fn progressive_learning_rejects_redundant_prior_lessons() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("zcash-shielded-payments".to_string()),
            ecosystem_id: Some("zcash".to_string()),
            topic: Some("ZIP-321 shielded checkout".to_string()),
            learning_profile: Some("Backend dev".to_string()),
            learning_intents: vec!["Understand the trust boundary".to_string()],
            interests: vec!["Zcash Shielded Payments".to_string()],
            learner_goal: "Understand Zcash shielded checkout denial cases".to_string(),
            background: "Backend dev".to_string(),
            pace: "Deep dive".to_string(),
        };
        let lesson = quality_gate_test_lesson();
        let prior = vec![prior_learning_lesson_from_compact(&lesson)];
        let mut repeated = quality_gate_test_lesson();
        repeated.t = "ZIP-321 shielded checkout trust boundary repeated".to_string();

        assert!(
            validate_ai_learning_lesson_compact_for_request_with_context(
                &request, &repeated, 1, &prior,
            )
            .is_err()
        );
    }

    #[test]
    fn learning_module_titles_do_not_use_product_prefix() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("stacks-builder-basics".to_string()),
            ecosystem_id: Some("stacks".to_string()),
            topic: Some("Stacks and Bitcoin mental model".to_string()),
            learning_profile: Some("Builder".to_string()),
            learning_intents: vec!["Understand Stacks settlement boundaries".to_string()],
            interests: Vec::new(),
            learner_goal: "Understand how Stacks apps relate to Bitcoin evidence".to_string(),
            background: "Builder".to_string(),
            pace: "Deep dive".to_string(),
        };

        let title = learning_module_title(&request);
        assert_eq!(title, "Stacks and Bitcoin mental model Deep Dive");
        assert!(!title.starts_with("VibeQuest:"));
        assert_eq!(
            clean_learning_module_title(
                "VibeQuest: Stacks: Stacks and Bitcoin mental model Deep Dive"
            ),
            "Stacks and Bitcoin mental model Deep Dive"
        );
        assert_eq!(
            clean_learning_module_title(
                "VibeQuest: Golem: Golem compute lab: requestor/provider execution Deep Dive"
            ),
            "Golem compute lab: requestor/provider execution Deep Dive"
        );
    }

    #[test]
    fn compact_learning_module_keeps_checkpoint_options() {
        let module = LearningModule {
            title: "CKB Cell Foundations".to_string(),
            learner_profile: "Vibecoder learning CKB".to_string(),
            outcome: "Explain cells and ship a small verifier quest.".to_string(),
            lessons: (0..5)
                .map(|index| LearningLesson {
                    id: format!("lesson-{index}"),
                    title: "Cells as state".to_string(),
                    why_it_matters: "Cells are the state a verifier trusts.".to_string(),
                    explanation: "A CKB cell is consumed and recreated, so generated code must bind witnesses to the expected cell state.".to_string(),
                    concepts: vec!["cell".to_string(), "witness".to_string()],
                    submodules: Vec::new(),
                    resources: Vec::new(),
                    evidence_map: Vec::new(),
                    quality_score: LearningQualityScore::default(),
                    checkpoint: LearningCheckpoint {
                        question: "What should the verifier bind?".to_string(),
                        options: vec![
                            LearningOption { label: "The exact cell and witness".to_string(), feedback: "Correct.".to_string() },
                            LearningOption { label: "Only the UI state".to_string(), feedback: "UI state is not proof.".to_string() },
                            LearningOption { label: "Only the reward amount".to_string(), feedback: "Amount is not identity.".to_string() },
                            LearningOption { label: "Nothing".to_string(), feedback: "That leaves replay risk.".to_string() },
                        ],
                        correct_index: index as usize % 4,
                        explanation: "The witness must match the accepted cell state.".to_string(),
                        follow_up_question: "How would a replay attack change the cell?".to_string(),
                    },
                    quest_bridge: "Build a verifier that rejects mismatched witnesses.".to_string(),
                })
                .collect(),
            capstone_quest_prompt: "Build a CKB witness verifier with a denial test.".to_string(),
            resources: vec![],
        };

        let compacted = compact_learning_module(module).unwrap();
        assert_eq!(compacted.lessons.len(), 5);
        assert_eq!(compacted.lessons[0].checkpoint.options.len(), 4);
        assert!(!compacted.resources.is_empty());
    }

    #[test]
    fn zcash_learning_request_shapes_focus_and_concepts() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("zcash-shielded-payments".to_string()),
            ecosystem_id: Some("zcash".to_string()),
            topic: Some("ZIP-321 shielded checkout".to_string()),
            learning_profile: Some("Security auditor".to_string()),
            learning_intents: vec!["Reject wrong-network payment requests".to_string()],
            interests: vec![
                "Zcash Shielded Payments".to_string(),
                "ZIP-321 Payment Requests".to_string(),
            ],
            learner_goal: "Understand Zcash shielded checkout denial cases".to_string(),
            background: "Security auditor".to_string(),
            pace: "Audit-heavy".to_string(),
        };
        let lesson = AiLearningLessonCompact {
            t: "ZIP-321 checkout safety".to_string(),
            e: "Generated Zcash checkout code must bind the ZIP-321 payment request, shielded address, zatoshi amount, memo policy, and network before it accepts payment evidence.".to_string(),
            s: "verifyZip321Request(request, { network: 'testnet' })".to_string(),
            w: "Privacy safety depends on denying malformed payment requests.".to_string(),
            j: "Build a verifier with wrong-network and memo denial tests.".to_string(),
            f: "Which field leaks privacy if it is logged?".to_string(),
            q: "Which ZIP-321 request field must the checkout verify before accepting a shielded payment?".to_string(),
            a: "The network-bound shielded recipient and amount".to_string(),
            b: Vec::new(),
            bf: Vec::new(),
            ci: 0,
        };

        assert_eq!(learning_ecosystem_label(&request), "Zcash");
        assert!(learning_focus_label(&request).contains("Zcash"));
        assert!(learning_module_capstone_prompt(&request).contains("ZIP-321"));
        assert!(learning_focus_directive(&request).contains("Zcash shielded-payment"));
        assert!(
            infer_learning_concepts("Zcash ZIP-321", &lesson)
                .iter()
                .any(|concept| concept == "ZIP-321 payment request")
        );
    }
    #[test]
    fn stacks_learning_request_shapes_focus_sources_and_rejects_ecosystem_leakage() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("stacks-bitcoin-apps".to_string()),
            ecosystem_id: Some("stacks".to_string()),
            topic: Some("Clarity wallet authorization and safe app flow".to_string()),
            learning_profile: Some("Frontend dev".to_string()),
            learning_intents: vec!["Understand the trust boundary".to_string()],
            interests: vec![
                "Stacks and Bitcoin".to_string(),
                "Clarity Smart Contracts".to_string(),
                "sBTC Basics".to_string(),
                "BNS Product Identity".to_string(),
            ],
            learner_goal: "Understand Stacks app authorization and denial tests".to_string(),
            background: "Frontend dev".to_string(),
            pace: "Focused".to_string(),
        };
        let sentence = "A Stacks app should treat the wallet signature, submitted transaction, Clarity contract call, principal, post-condition, BNS name resolution, and sBTC flow as separate pieces of evidence instead of assuming a frontend button proves completion. It must reject unsafe mismatches, replayed authorization, stale transaction status, and malformed app state before progress changes. ";
        let lesson = AiLearningLessonCompact {
            t: "Stacks Clarity authorization boundary".to_string(),
            e: format!(
                "{} Accuracy check: verify each claim against the official Stacks documentation for Clarity, transactions, wallets, sBTC, and BNS before trusting generated app logic. Submodule path: wallet connect -> Clarity public function -> post-condition review -> explorer evidence -> denial tests. Further study: read the official Stacks documentation for Clarity, transactions, wallets, sBTC, and BNS.",
                sentence.repeat(72)
            ),
            s: "(define-public (claim (owner principal))\n  ;; TODO: require the expected principal and post-condition before accepting\n  (ok owner))".to_string(),
            w: "For a frontend developer, this matters because generated Stacks UI code can confuse a connected wallet with a completed Clarity action. The learner must separate display state, submitted transaction state, principal authorization, post-conditions, and BNS or sBTC assumptions.".to_string(),
            j: "Build a Stacks authorization map and denial tests that mutate the principal, post-condition, BNS name, sBTC assumption, and transaction status before the app marks progress complete.".to_string(),
            f: "Which field would you mutate first to prove the app does not trust frontend state as protocol evidence?".to_string(),
            q: "Which Stacks transaction, Clarity contract, principal, post-condition, sBTC, BNS name, and wallet signature fields form the app authorization proof boundary?".to_string(),
            a: "The exact principal, Clarity contract call, post-condition, submitted transaction status, and relevant sBTC or BNS evidence".to_string(),
            b: vec![
                "The connected wallet button".to_string(),
                "The profile name shown by the app".to_string(),
                "Any frontend success toast".to_string(),
            ],
            bf: vec![
                "A connected wallet starts authorization UX, but it does not prove a specific Clarity call completed.".to_string(),
                "A profile label is not protocol evidence for a Stacks action.".to_string(),
                "A toast can be rendered before the transaction is confirmed or even submitted.".to_string(),
            ],
            ci: 1,
        };

        assert_eq!(learning_ecosystem_label(&request), "Stacks");
        assert!(learning_focus_label(&request).contains("Stacks"));
        assert!(learning_module_capstone_prompt(&request).contains("Stacks"));
        assert!(learning_focus_directive(&request).contains("Clarity"));
        assert!(learning_source_grounding_directive(&request).contains("docs.stacks.co"));
        assert!(
            default_learning_resources_for_focus("Stacks Clarity")
                .iter()
                .any(|resource| resource.title == "Stacks Documentation")
        );
        assert!(validate_ai_learning_lesson_compact_for_request(&request, &lesson).is_ok());
        assert!(
            infer_learning_concepts("Stacks Clarity sBTC BNS", &lesson)
                .iter()
                .any(|concept| concept == "Clarity contract")
        );

        let mut leaked = lesson;
        leaked
            .e
            .push_str(" ZIP-321 zatoshi Orchard receiver should not leak into a Stacks lesson.");
        assert!(validate_ai_learning_lesson_compact_for_request(&request, &leaked).is_err());
    }

    #[test]
    fn golem_learning_request_shapes_sources_validation_and_artifact_tags() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("golem-compute-lab".to_string()),
            ecosystem_id: Some("golem".to_string()),
            topic: Some("Golem JS SDK task execution and provider result validation".to_string()),
            learning_profile: Some("Backend dev".to_string()),
            learning_intents: vec![
                "Understand decentralized compute boundaries".to_string(),
                "Include interactive code snippets".to_string(),
            ],
            interests: vec![
                "Golem requestor/provider workflow".to_string(),
                "Yagna app key".to_string(),
                "Golem JS SDK".to_string(),
                "Task lifecycle and failure cases".to_string(),
            ],
            learner_goal: "Understand Golem compute execution with task lifecycle, provider failure, and result validation".to_string(),
            background: "Backend dev".to_string(),
            pace: "Audit-heavy".to_string(),
        };
        let sentence = "A Golem compute lesson must separate requestor intent, Yagna coordination, provider execution, market agreement, allocation budget, task command, result output, and payment boundary before accepting a decentralized compute job as complete. The learner validates provider identity, task lifecycle, expected result shape, cleanup behavior, and cost assumptions instead of treating provider output as automatically trusted. It should reject provider unavailable state, task timeout, missing result, corrupted result, wrong GVMI image, agreement mismatch, budget exceeded, Yagna disconnected, network failure, and Ray limitation assumptions. ";
        let lesson = AiLearningLessonCompact {
            t: "Golem requestor/provider task boundary".to_string(),
            e: format!(
                "{} Accuracy check: verify each claim against official Golem docs at docs.golem.network, Golem JS SDK documentation, requestor/provider interaction docs, task model docs, executing tasks examples, provider docs, and Ray on Golem limitations before trusting generated compute code. Submodule path: requestor intent -> Yagna app key -> provider agreement -> task execution -> result validation -> failure cases. Further study: read official Golem docs, JS SDK docs, task model docs, requestor/provider docs, and provider documentation.",
                sentence.repeat(72)
            ),
            s: "export function validateGolemTaskResult({ task, result, provider, budget }) {\n  if (!provider?.id) throw new Error('provider-unavailable');\n  if (task.timeoutMs && task.elapsedMs > task.timeoutMs) throw new Error('task-timeout');\n  if (!result?.stdout) throw new Error('missing-result');\n  if (budget.spent > budget.max) throw new Error('budget-exceeded');\n  // Learner edit: add a semantic validator for the expected output.\n  return { providerId: provider.id, outputReady: true };\n}".to_string(),
            w: "For a backend developer, this matters because generated Golem code can confuse local requestor state, Yagna coordination, provider execution, task output, and payment or budget assumptions. The learner must validate concrete results before accepting decentralized compute output.".to_string(),
            j: "Build a Golem task execution proof map with failure tests for provider unavailable, task timeout, missing result, corrupted output, wrong GVMI image, agreement mismatch, budget exceeded, and Yagna disconnect.".to_string(),
            f: "Which failure case would you mutate first to prove provider output is validated before completion?".to_string(),
            q: "Which Golem requestor, provider, Yagna app key, agreement, allocation budget, task command, result output, and failure-state fields form the compute proof boundary?".to_string(),
            a: "The requestor intent, Yagna/app key, provider agreement, allocation budget, executed task, validated result, and explicit failure handling".to_string(),
            b: vec![
                "The frontend job started label".to_string(),
                "Any provider output without validation".to_string(),
                "A generic decentralized cloud claim".to_string(),
            ],
            bf: vec![
                "A UI label can show intent, but it does not prove provider execution or result correctness.".to_string(),
                "Provider output must be checked against the expected task and result contract.".to_string(),
                "Marketing language is not execution evidence for a concrete Golem job.".to_string(),
            ],
            ci: 2,
        };

        assert_eq!(learning_ecosystem_label(&request), "Golem");
        assert!(learning_focus_label(&request).contains("Golem JS SDK"));
        assert!(
            learning_module_capstone_prompt(&request)
                .contains("final Golem compute execution quest")
        );
        assert!(learning_focus_directive(&request).contains("requestor/provider separation"));
        assert!(learning_source_grounding_directive(&request).contains("docs.golem.network"));
        assert!(validate_ai_learning_lesson_compact_for_request(&request, &lesson).is_ok());
        assert!(
            infer_learning_concepts("Golem JS SDK requestor provider", &lesson)
                .iter()
                .any(|concept| concept == "requestor")
        );
        let golem_resources =
            default_learning_resources_for_focus("Golem JS SDK requestor provider");
        assert!(
            golem_resources
                .iter()
                .any(|resource| resource.title == "Golem JS SDK")
        );
        assert!(
            golem_resources
                .iter()
                .any(|resource| resource.title == "Golem Requestor / Provider Interaction")
        );
        assert!(
            golem_resources
                .iter()
                .any(|resource| resource.title == "Ray on Golem Limitations")
        );

        let mut leaked = lesson.clone();
        leaked
            .e
            .push_str(" ZIP-321 zatoshi Orchard receiver, STON.fi widget, and jetton claims do not belong here.");
        assert!(validate_ai_learning_lesson_compact_for_request(&request, &leaked).is_err());

        let module = LearningModule {
            title: learning_module_title(&request),
            learner_profile: learning_module_profile(&request),
            outcome: learning_module_outcome(&request),
            lessons: vec![
                compact_ai_lesson_to_learning_lesson(0, "Backend dev", "Golem", &request, lesson)
                    .unwrap(),
            ],
            capstone_quest_prompt: learning_module_capstone_prompt(&request),
            resources: default_learning_resources_for_focus("Golem"),
        };
        let provider = AiProviderMetadata {
            provider_kind: "openai-compatible".to_string(),
            model: "test-model".to_string(),
            endpoint_origin: "https://share-ai.ckbdev.com".to_string(),
            reasoning_effort: ReasoningEffort::Minimal,
            response_storage_disabled: true,
            timeout_seconds: 90,
            configured: true,
        };
        let artifact = learning_eval_artifact(&request, &module, provider);
        assert_eq!(artifact.ecosystem_id, "golem");
        assert!(
            artifact
                .integration_tags
                .iter()
                .any(|tag| tag == "requestor-provider")
        );
        assert!(
            artifact
                .integration_tags
                .iter()
                .any(|tag| tag == "task-lifecycle")
        );
        assert!(artifact.code_mode_enabled);
        assert_eq!(
            artifact.execution_path.as_deref(),
            Some("js-sdk-task-execution")
        );
        assert!(artifact.task_lifecycle_covered);
        assert!(artifact.failure_cases_count >= 7);
        assert!(artifact.denial_tests_count >= 7);
        assert!(!artifact.final_compute_lab_ready);
        assert!(
            artifact
                .source_ids
                .iter()
                .any(|source_id| source_id == "golem-js-sdk")
        );
        assert!(
            artifact
                .source_ids
                .iter()
                .any(|source_id| source_id == "golem-requestor-provider")
        );
        assert!(
            artifact
                .source_categories
                .iter()
                .any(|category| category == "golem-js-sdk")
        );
        assert!(
            artifact
                .source_categories
                .iter()
                .any(|category| category == "golem-ray")
        );
        assert!(
            artifact
                .compute_model_coverage
                .iter()
                .any(|item| item == "requestor")
        );
    }

    #[test]
    fn golem_eval_artifact_flags_final_compute_lab_and_unsupported_claims() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("golem-compute-lab".to_string()),
            ecosystem_id: Some("golem".to_string()),
            topic: Some("Final Golem compute execution quest".to_string()),
            learning_profile: Some("Backend dev".to_string()),
            learning_intents: vec!["Include code snippets".to_string()],
            interests: vec![
                "Golem JS SDK".to_string(),
                "Python and Ray on Golem".to_string(),
                "Golem dApp deployment".to_string(),
                "Failure-state testing".to_string(),
            ],
            learner_goal: "Build and review a final Golem compute execution lab".to_string(),
            background: "Backend dev".to_string(),
            pace: "Audit-heavy".to_string(),
        };
        let resources = default_learning_resources_for_focus("Golem");
        let final_lab_text = "Final Golem compute lab: the learner builds one requestor/provider execution plan and one failure matrix. The lab treats local UI state as intent, Yagna as coordination, providers as remote execution, agreements and allocations as budget/payment boundaries, tasks as workload units, and results as evidence that must be validated instead of automatically trusted. It uses official Golem docs at docs.golem.network, JS SDK docs, Python docs, Ray on Golem limitations, dApp docs, provider overview, and requestor/provider interaction docs. The failure cases mutate provider unavailable state, provider timeout, failed task execution, missing result, corrupted result, wrong GVMI image, wrong runtime version, agreement mismatch, budget exceeded, Yagna disconnected, network failure, Ray supported-version limitation, and provider result automatically trusted. The lesson avoids claiming smart contract executes compute, free compute, unlimited GPU, or production certification.";
        let lessons = (0..5)
            .map(|index| LearningLesson {
                id: format!("module-{}-lesson-1", index + 1),
                title: if index == 4 {
                    "Final Golem Compute Quest".to_string()
                } else {
                    format!("Golem Source-Grounded Compute Boundary {}", index + 1)
                },
                why_it_matters: final_lab_text.to_string(),
                explanation: final_lab_text.to_string(),
                concepts: vec![
                    "Golem".to_string(),
                    "requestor".to_string(),
                    "provider".to_string(),
                    "Yagna".to_string(),
                    "result validation".to_string(),
                ],
                submodules: Vec::new(),
                resources: resources.clone(),
                evidence_map: Vec::new(),
                quality_score: LearningQualityScore {
                    source_coverage: 95,
                    technical_depth: 95,
                    checkpoint_quality: 95,
                    placeholder_free: true,
                    ecosystem_alignment: true,
                    passed: true,
                },
                checkpoint: LearningCheckpoint {
                    question: "Which Golem requestor/provider, Yagna, agreement, allocation, task, result, budget, and failure-state evidence makes the final compute lab trustworthy?".to_string(),
                    options: vec![
                        LearningOption {
                            label: "Requestor intent, Yagna/app key, provider agreement, allocation budget, task execution, validated result, and failure matrix".to_string(),
                            feedback: "Correct compute execution boundary.".to_string(),
                        },
                        LearningOption {
                            label: "A frontend job started label".to_string(),
                            feedback: "UI state is not remote compute evidence.".to_string(),
                        },
                        LearningOption {
                            label: "A provider output with no validation".to_string(),
                            feedback: "Provider output still needs validation or retry strategy.".to_string(),
                        },
                        LearningOption {
                            label: "A generic AI/GPU claim".to_string(),
                            feedback: "AI/GPU claims need current documented support and workload constraints.".to_string(),
                        },
                    ],
                    correct_index: 0,
                    explanation: "The answer must bind requestor intent, Yagna coordination, provider agreement, budget, task execution, result validation, and failure handling.".to_string(),
                    follow_up_question: "Which result validator would you add for this workload?".to_string(),
                },
                quest_bridge: final_lab_text.to_string(),
            })
            .collect::<Vec<_>>();
        let module = LearningModule {
            title: learning_module_title(&request),
            learner_profile: learning_module_profile(&request),
            outcome: learning_module_outcome(&request),
            lessons,
            capstone_quest_prompt: learning_module_capstone_prompt(&request),
            resources,
        };
        let provider = AiProviderMetadata {
            provider_kind: "openai-compatible".to_string(),
            model: "test-model".to_string(),
            endpoint_origin: "https://share-ai.ckbdev.com".to_string(),
            reasoning_effort: ReasoningEffort::Minimal,
            response_storage_disabled: true,
            timeout_seconds: 90,
            configured: true,
        };

        let artifact = learning_eval_artifact(&request, &module, provider);

        assert!(artifact.final_lab_ready);
        assert!(artifact.final_compute_lab_ready);
        assert!(artifact.failure_cases_count >= 9);
        assert!(artifact.denial_tests_count >= 9);
        assert!(artifact.task_lifecycle_covered);
        assert!(
            artifact
                .unsupported_claim_warnings
                .iter()
                .any(|warning| warning.contains("smart-contract execution"))
        );
        assert!(
            artifact
                .unsupported_claim_warnings
                .iter()
                .any(|warning| warning.contains("AI/GPU capability"))
        );
    }

    #[test]
    fn ton_stonfi_learning_request_shapes_sources_validation_and_artifact_tags() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("ton-stonfi-integration-lab".to_string()),
            ecosystem_id: Some("ton-stonfi".to_string()),
            topic: Some("STON.fi SDK swap quote and stale quote denial".to_string()),
            learning_profile: Some("Backend dev".to_string()),
            learning_intents: vec![
                "Understand the trust boundary".to_string(),
                "Include interactive code snippets".to_string(),
            ],
            interests: vec![
                "STON.fi SDK".to_string(),
                "TON Connect".to_string(),
                "Jetton Verification".to_string(),
                "Slippage Safety".to_string(),
            ],
            learner_goal:
                "Understand safe STON.fi swap integration with TON Connect and jetton denial tests"
                    .to_string(),
            background: "Backend dev".to_string(),
            pace: "Audit-heavy".to_string(),
        };
        let sentence = "A TON / STON.fi integration must separate the Omniston widget or STON.fi SDK quote from wallet approval and final transaction-state evidence. The builder must verify TON Connect manifest scope, jetton master address, jetton wallet contract assumptions, route, quote timestamp, slippage, min-out, referral fee disclosure, and whether the transaction is pending or confirmed before claiming completion. It should reject unsafe fake jetton metadata, stale quotes, wrong route data, missing min-out, disconnected wallet state, and REST API data treated as settlement proof. ";
        let lesson = AiLearningLessonCompact {
            t: "STON.fi quote freshness and TON Connect boundary".to_string(),
            e: format!(
                "{} Accuracy check: verify each claim against the official STON.fi documentation, docs.ston.fi DEX SDK, Omniston widget documentation, TON Connect documentation, and TON Jetton documentation before trusting generated swap code. Submodule path: TON Connect manifest -> STON.fi quote -> jetton master allowlist -> slippage min-out -> transaction state denial tests. Further study: read the official STON.fi DEX SDK docs, Omniston widget docs, TON Connect documentation, and TON Jetton documentation.",
                sentence.repeat(72)
            ),
            s: "export function validateStonfiSwapIntent({ quote, jettonMaster, minOut, walletState }) {
  if (quote.isStale || !minOut) throw new Error('stale-or-unsafe-quote');
  if (!walletState.tonConnectManifestOk) throw new Error('bad-ton-connect-manifest');
  return jettonMaster.allowlisted;
}".to_string(),
            w: "For a backend developer, this matters because generated STON.fi swap code can confuse a fresh quote, TON Connect approval, jetton metadata, and final transaction state. The learner must verify verified fields before accepting a swap as safe or complete.".to_string(),
            j: "Build a STON.fi swap verifier artifact with denial tests for stale quotes, fake jetton master addresses, missing min-out, wrong TON Connect manifest, and pending transaction state.".to_string(),
            f: "Which field would you mutate first to prove the swap flow rejects stale or spoofed evidence?".to_string(),
            q: "Which STON.fi quote, route, TON Connect manifest, jetton master address, min-out, slippage, referral fee, and transaction state fields form the swap proof boundary?".to_string(),
            a: "The source-backed quote and route, TON Connect manifest scope, verified jetton master, min-out/slippage constraints, disclosed fee, and confirmed transaction state".to_string(),
            b: vec![
                "The token symbol and frontend success toast".to_string(),
                "Any REST API response with a price".to_string(),
                "A connected wallet icon alone".to_string(),
            ],
            bf: vec![
                "Token symbol and UI state can be spoofed; the lesson requires jetton master and transaction evidence.".to_string(),
                "A REST response can inform a quote, but it is not final settlement proof.".to_string(),
                "A connected wallet starts authorization UX; it does not prove the user approved this exact swap.".to_string(),
            ],
            ci: 2,
        };

        assert_eq!(learning_ecosystem_label(&request), "TON / STON.fi");
        assert!(learning_focus_label(&request).contains("STON.fi SDK swap quote"));
        assert!(
            learning_module_capstone_prompt(&request)
                .contains("final TON / STON.fi safe-swap integration lab")
        );
        assert!(learning_focus_directive(&request).contains("STON.fi DEX SDK"));
        assert!(learning_source_grounding_directive(&request).contains("docs.ston.fi"));
        assert!(validate_ai_learning_lesson_compact_for_request(&request, &lesson).is_ok());
        assert!(
            infer_learning_concepts("TON / STON.fi", &lesson)
                .iter()
                .any(|concept| concept == "TON Connect")
        );
        let ton_resources = default_learning_resources_for_focus("TON / STON.fi STON.fi SDK");
        assert!(
            ton_resources
                .iter()
                .any(|resource| resource.title == "STON.fi DEX SDK Documentation")
        );
        assert!(
            ton_resources
                .iter()
                .any(|resource| resource.title == "STON.fi Omniston SDK")
        );
        assert!(
            ton_resources
                .iter()
                .any(|resource| resource.title == "TON Jetton Processing")
        );

        let mut leaked = lesson.clone();
        leaked.e.push_str(
            " ZIP-321 zatoshi Orchard receiver and Clarity contract claims do not belong here.",
        );
        assert!(validate_ai_learning_lesson_compact_for_request(&request, &leaked).is_err());

        let module = LearningModule {
            title: learning_module_title(&request),
            learner_profile: learning_module_profile(&request),
            outcome: learning_module_outcome(&request),
            lessons: vec![
                compact_ai_lesson_to_learning_lesson(
                    0,
                    "Backend dev",
                    "TON / STON.fi",
                    &request,
                    lesson,
                )
                .unwrap(),
            ],
            capstone_quest_prompt: learning_module_capstone_prompt(&request),
            resources: default_learning_resources_for_focus("TON / STON.fi"),
        };
        let provider = AiProviderMetadata {
            provider_kind: "openai-compatible".to_string(),
            model: "test-model".to_string(),
            endpoint_origin: "https://share-ai.ckbdev.com".to_string(),
            reasoning_effort: ReasoningEffort::Minimal,
            response_storage_disabled: true,
            timeout_seconds: 90,
            configured: true,
        };
        let artifact = learning_eval_artifact(&request, &module, provider);
        assert_eq!(artifact.ecosystem_id, "ton-stonfi");
        assert!(
            artifact
                .integration_tags
                .iter()
                .any(|tag| tag == "ton-connect")
        );
        assert!(
            artifact
                .integration_tags
                .iter()
                .any(|tag| tag == "quote-freshness")
        );
        assert!(artifact.code_mode_enabled);
        assert!(!artifact.final_lab_ready);
        assert!(artifact.denial_tests_count >= 5);
        assert!(
            artifact
                .source_ids
                .iter()
                .any(|source_id| source_id == "stonfi-dex-sdk")
        );
        assert!(
            artifact
                .source_ids
                .iter()
                .any(|source_id| source_id == "ton-connect-overview")
        );
        assert!(
            artifact
                .source_categories
                .iter()
                .any(|category| category == "stonfi-sdk")
        );
        assert!(
            artifact
                .source_categories
                .iter()
                .any(|category| category == "jetton-standard")
        );
        assert!(
            artifact.lesson_reports[0]
                .source_categories
                .iter()
                .any(|category| category == "stonfi-sdk")
        );
    }

    #[test]
    fn ton_stonfi_eval_artifact_flags_final_lab_and_unsupported_claims() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("ton-stonfi-integration-lab".to_string()),
            ecosystem_id: Some("ton-stonfi".to_string()),
            topic: Some("Final STON.fi safe-swap integration lab".to_string()),
            learning_profile: Some("Backend dev".to_string()),
            learning_intents: vec!["Include code snippets".to_string()],
            interests: vec![
                "STON.fi SDK".to_string(),
                "Omniston SDK".to_string(),
                "TON Connect".to_string(),
                "Jetton Verification".to_string(),
            ],
            learner_goal: "Build and review a safe TON / STON.fi swap learning lab".to_string(),
            background: "Backend dev".to_string(),
            pace: "Audit-heavy".to_string(),
        };
        let resources = default_learning_resources_for_focus("TON / STON.fi");
        let final_lab_text = "Final STON.fi safe-swap lab: the learner builds one transaction review path and one denial-test matrix. The lab treats STON.fi quotes and Omniston widget output as integration inputs, not proof. It verifies TON Connect manifest domain, wallet approval scope, jetton master allowlist, jetton wallet contract assumptions, route identity, quote timestamp, slippage, min-out, referral fee disclosure, confirmed transaction state, and explorer-visible settlement. The denial cases mutate fake jetton metadata, fake token symbol, changed token pair, stale quote timestamp, wrong route, missing min-out, min-out set too low, wallet disconnected state, wallet rejection, pending transaction state, wrong TON Connect manifest domain, duplicate TON Connect connector state, referral fee disclosure, and REST API response treated as settlement proof. This also catches risky claims where a widget proves settlement or a REST API proves final transaction success.";
        let lessons = (0..5)
            .map(|index| LearningLesson {
                id: format!("module-{}-lesson-1", index + 1),
                title: if index == 4 {
                    "Final STON.fi Safe-Swap Lab".to_string()
                } else {
                    format!("STON.fi Source-Grounded Boundary {}", index + 1)
                },
                why_it_matters: final_lab_text.to_string(),
                explanation: final_lab_text.to_string(),
                concepts: vec![
                    "STON.fi SDK".to_string(),
                    "TON Connect".to_string(),
                    "jetton master".to_string(),
                    "slippage".to_string(),
                    "transaction state".to_string(),
                ],
                submodules: Vec::new(),
                resources: resources.clone(),
                evidence_map: Vec::new(),
                quality_score: LearningQualityScore {
                    source_coverage: 95,
                    technical_depth: 95,
                    checkpoint_quality: 95,
                    placeholder_free: true,
                    ecosystem_alignment: true,
                    passed: true,
                },
                checkpoint: LearningCheckpoint {
                    question: "Which evidence proves the STON.fi swap completed safely?".to_string(),
                    options: vec![
                        LearningOption {
                            label: "Confirmed transaction state bound to checked route, jetton master, min-out, and disclosed fee".to_string(),
                            feedback: "Correct boundary.".to_string(),
                        },
                        LearningOption {
                            label: "The widget success label".to_string(),
                            feedback: "Widget state is not settlement proof.".to_string(),
                        },
                        LearningOption {
                            label: "A REST quote response".to_string(),
                            feedback: "A quote is not a confirmed transaction.".to_string(),
                        },
                        LearningOption {
                            label: "A token symbol".to_string(),
                            feedback: "Symbols can be spoofed.".to_string(),
                        },
                    ],
                    correct_index: 0,
                    explanation: "The answer must bind user intent to verified route, jetton, min-out, fee, and final transaction evidence.".to_string(),
                    follow_up_question: "Which denial case would you run first?".to_string(),
                },
                quest_bridge: final_lab_text.to_string(),
            })
            .collect::<Vec<_>>();
        let module = LearningModule {
            title: learning_module_title(&request),
            learner_profile: learning_module_profile(&request),
            outcome: learning_module_outcome(&request),
            lessons,
            capstone_quest_prompt: learning_module_capstone_prompt(&request),
            resources,
        };
        let provider = AiProviderMetadata {
            provider_kind: "openai-compatible".to_string(),
            model: "test-model".to_string(),
            endpoint_origin: "https://share-ai.ckbdev.com".to_string(),
            reasoning_effort: ReasoningEffort::Minimal,
            response_storage_disabled: true,
            timeout_seconds: 90,
            configured: true,
        };

        let artifact = learning_eval_artifact(&request, &module, provider);

        assert!(artifact.final_lab_ready);
        assert!(artifact.denial_tests_count >= 8);
        assert!(
            artifact
                .unsupported_claim_warnings
                .iter()
                .any(|warning| warning.contains("SDK/widget output"))
        );
        assert!(
            artifact
                .unsupported_claim_warnings
                .iter()
                .any(|warning| warning.contains("REST API response"))
        );

        let final_only_module = LearningModule {
            title: module.title.clone(),
            learner_profile: module.learner_profile.clone(),
            outcome: module.outcome.clone(),
            lessons: vec![module.lessons[4].clone()],
            capstone_quest_prompt: module.capstone_quest_prompt.clone(),
            resources: module.resources.clone(),
        };
        let provider = AiProviderMetadata {
            provider_kind: "openai-compatible".to_string(),
            model: "test-model".to_string(),
            endpoint_origin: "https://share-ai.ckbdev.com".to_string(),
            reasoning_effort: ReasoningEffort::Minimal,
            response_storage_disabled: true,
            timeout_seconds: 90,
            configured: true,
        };
        let final_only_artifact = learning_eval_artifact(&request, &final_only_module, provider);
        assert!(final_only_artifact.final_lab_ready);
        assert!(final_only_artifact.denial_tests_count >= 8);
    }

    #[test]
    fn ton_stonfi_lesson_normalization_repairs_source_anchor_before_validation() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("ton-stonfi-integration-lab".to_string()),
            ecosystem_id: Some("ton-stonfi".to_string()),
            topic: Some("Safe STON.fi swap integration with stale quote denial".to_string()),
            learning_profile: Some("Backend dev".to_string()),
            learning_intents: vec!["Understand the trust boundary".to_string()],
            interests: vec![
                "STON.fi SDK".to_string(),
                "TON Connect".to_string(),
                "Jetton Verification".to_string(),
                "Slippage Safety".to_string(),
            ],
            learner_goal: "Understand safe STON.fi swap integration with TON Connect, jetton checks, slippage, and stale quote denial tests".to_string(),
            background: "Backend dev".to_string(),
            pace: "Focused".to_string(),
        };
        let body = "A TON and STON.fi swap lesson must separate an integration UI quote from wallet approval and final transaction evidence. The learner verifies quote freshness, route identity, jetton master address, jetton wallet contract assumptions, slippage, min-out, referral fee disclosure, and confirmed transaction state before accepting completion. A safe integration should reject stale quote payloads, wrong route data, fake jetton metadata, disconnected wallet state, unsafe min-out values, and pending transaction state instead of trusting an interface success message. This matters because a REST quote or displayed button can help the user choose a swap but cannot prove settlement by itself. ";
        let mut lesson = AiLearningLessonCompact {
            t: "Quote Freshness and Min-Out as the First Trust Boundary".to_string(),
            e: body.repeat(72),
            s: "export function validateStonfiQuote({ quote, minOut, tx }) { // TODO: tune minOut for learner risk model\n  if (quote.stale || !minOut || tx.state !== 'confirmed') throw new Error('unsafe-swap'); return true; }".to_string(),
            w: "For a backend developer, this matters because generated STON.fi swap code can confuse a fresh quote, TON Connect approval, jetton metadata, and final transaction state. The learner must verify source-backed fields before accepting a swap as safe or complete.".to_string(),
            j: "Build a STON.fi quote verifier with denial tests for stale quotes, missing min-out, fake jetton master addresses, and pending transaction state.".to_string(),
            f: "Which quote field would you mutate first to prove stale data is rejected?".to_string(),
            q: "For this lesson, which evidence can prove final swap completion: STON.fi swap quote fields, integration UI config, TON Connect pending transaction state, or a confirmed TON transaction with checked route, jetton master, slippage, min-out, and referral fee?".to_string(),
            a: "A confirmed TON transaction bound to the checked STON.fi quote, route, jetton master, slippage, min-out, and disclosed referral fee".to_string(),
            b: vec![
                "The integration UI display state".to_string(),
                "A TON Connect pending transaction alone".to_string(),
                "A token symbol and estimated output".to_string(),
            ],
            bf: vec![
                "UI display state is useful context, not settlement evidence.".to_string(),
                "Pending wallet state can still fail or represent the wrong transaction.".to_string(),
                "Symbols and estimates can be spoofed or stale without jetton and transaction checks.".to_string(),
            ],
            ci: 3,
        };

        assert!(
            validate_ai_learning_lesson_compact_for_request_with_context(&request, &lesson, 0, &[])
                .is_err()
        );
        normalize_ai_learning_lesson_for_request(&request, 0, &mut lesson);
        assert!(lesson.e.contains("docs.ston.fi"));
        assert!(lesson.s.contains("Learner edit"));
        assert!(!lesson.s.to_ascii_lowercase().contains("todo"));
        assert!(
            validate_ai_learning_lesson_compact_for_request_with_context(&request, &lesson, 0, &[])
                .is_ok()
        );
    }

    #[test]
    fn ton_stonfi_final_module_normalization_adds_reviewed_denial_checklist() {
        let request = GenerateLearningModuleRequest {
            path_id: Some("ton-stonfi-integration-lab".to_string()),
            ecosystem_id: Some("ton-stonfi".to_string()),
            topic: Some("Final STON.fi safe-swap integration lab".to_string()),
            learning_profile: Some("Backend dev".to_string()),
            learning_intents: vec!["Include code snippets".to_string()],
            interests: vec![
                "STON.fi SDK".to_string(),
                "TON Connect".to_string(),
                "Jetton Verification".to_string(),
                "Slippage Safety".to_string(),
            ],
            learner_goal:
                "Review a safe STON.fi swap integration with source-grounded denial tests"
                    .to_string(),
            background: "Backend dev".to_string(),
            pace: "Audit-heavy".to_string(),
        };
        let body = "A final TON and STON.fi safe-swap lab ties a swap route, quote freshness, wallet approval, jetton master verification, min-out, slippage, referral fee disclosure, and confirmed transaction evidence into one reviewable artifact. The lesson explicitly rejects fake jetton metadata, stale quotes, wrong route data, pending transaction state, and REST API output treated as settlement evidence while using official STON.fi and TON documentation as the source grounding. Accuracy check: verify each claim against docs.ston.fi and docs.ton.org before trusting the generated integration. Submodule path: quote boundary -> wallet boundary -> jetton identity -> transaction state -> final denial matrix. Further study: read STON.fi DEX SDK, Omniston SDK, TON Connect, and TON Jetton documentation. ";
        let mut lesson = AiLearningLessonCompact {
            t: "Final STON.fi safe-swap lab".to_string(),
            e: body.repeat(56),
            s: "export function incompleteFinalLab(state) { return state.confirmed; }".to_string(),
            w: "For a backend developer, this matters because the final lab proves the learner can separate STON.fi integration inputs from confirmed TON transaction evidence before trusting generated code.".to_string(),
            j: "Build the final STON.fi swap proof map and denial matrix.".to_string(),
            f: "Which final denial case should fail before wallet approval?".to_string(),
            q: "Which STON.fi route, jetton master, min-out, TON Connect manifest, referral fee, and transaction state evidence makes the final safe-swap lab trustworthy?".to_string(),
            a: "A confirmed transaction bound to checked route, jetton master, min-out, manifest scope, disclosed fee, and denial-test coverage".to_string(),
            b: vec![
                "A widget success label".to_string(),
                "A token symbol and REST quote".to_string(),
                "A connected wallet icon".to_string(),
            ],
            bf: vec![
                "Widget labels are not settlement proof.".to_string(),
                "Symbols and REST quotes are not final proof.".to_string(),
                "Connection is not approval or settlement.".to_string(),
            ],
            ci: 0,
        };

        normalize_ai_learning_lesson_for_request(&request, 4, &mut lesson);
        let normalized_text = learning_lesson_full_text(&lesson).to_ascii_lowercase();
        assert!(normalized_text.contains("fake jetton master address"));
        assert!(normalized_text.contains("duplicate ton connect connector state"));
        assert!(lesson.s.contains("restApiResponseUsedAsSettlementProof"));

        let final_lesson = compact_ai_lesson_to_learning_lesson(
            4,
            "Backend dev",
            "TON / STON.fi",
            &request,
            lesson,
        )
        .unwrap();
        let module = LearningModule {
            title: learning_module_title(&request),
            learner_profile: learning_module_profile(&request),
            outcome: learning_module_outcome(&request),
            lessons: vec![final_lesson],
            capstone_quest_prompt: learning_module_capstone_prompt(&request),
            resources: default_learning_resources_for_focus("TON / STON.fi"),
        };
        let provider = AiProviderMetadata {
            provider_kind: "openai-compatible".to_string(),
            model: "test-model".to_string(),
            endpoint_origin: "https://share-ai.ckbdev.com".to_string(),
            reasoning_effort: ReasoningEffort::Minimal,
            response_storage_disabled: true,
            timeout_seconds: 90,
            configured: true,
        };
        let artifact = learning_eval_artifact(&request, &module, provider);
        assert!(artifact.final_lab_ready);
        assert!(artifact.denial_tests_count >= 8);
    }

    #[test]
    fn learning_only_prompt_is_not_compiled_as_code_quest() {
        assert!(is_learning_only_prompt("Teach me about CKB"));
        assert!(is_learning_only_prompt(
            "help me understand Fiber payment channels"
        ));
        assert!(!is_learning_only_prompt(
            "Build a CKB lesson quest with a verifier and denial test"
        ));
        assert!(!is_learning_only_prompt(
            "Explain the generated verifier and write a denial test"
        ));
    }

    #[test]
    fn reward_invoice_validation_rejects_missing_or_unsafe_values() {
        assert!(matches!(
            validate_reward_invoice("   "),
            Err(ApiError::MissingFiberInvoice)
        ));
        assert!(matches!(
            validate_reward_invoice("fiber invoice with spaces"),
            Err(ApiError::InvalidFiberInvoice)
        ));
        assert!(matches!(
            validate_reward_invoice("short"),
            Err(ApiError::InvalidFiberInvoice)
        ));
    }

    #[test]
    fn reward_invoice_validation_accepts_plausible_fiber_invoice() {
        validate_reward_invoice("fiber:testnet-invoice-abc123").unwrap();
    }

    #[test]
    fn server_completion_proof_rejects_unverified_runs() {
        let mut run = quest_run_fixture();
        run.progress.boss_fight_solved = false;

        assert!(matches!(
            server_completion_proof(&run),
            Err(ApiError::CompletionNotVerified)
        ));
    }

    #[test]
    fn server_completion_proof_accepts_verified_runs() {
        let run = quest_run_fixture();
        let proof = server_completion_proof(&run).unwrap();

        assert!(proof.identity_gate);
        assert!(proof.infrastructure_gate);
        assert!(proof.verification_gate);
        assert!(proof.generated_files_verified);
        assert!(proof.tests_present);
        assert!(proof.proof_present);
        assert!(proof.denial_path_present);
    }

    fn quest_run_fixture() -> QuestRunDocument {
        let quest = QuestBlueprint {
            title: "AI Fiber Receipt Run".to_string(),
            premise: "A learner validates a Fiber payment receipt against a CKB proof badge.".to_string(),
            build_objective: "Build a Fiber paid-content app with CKB proof receipts".to_string(),
            comprehension_gates: vec![
                "Explain the JoyID, Fiber receipt, and CKB proof trust boundary.".to_string(),
                "Verify that unpaid reads and replayed receipts are rejected.".to_string(),
                "Ship only after the badge and payout claim are defended.".to_string(),
            ],
            boss_fight: "A reader replays a receipt from another run. Identify the missing run binding.".to_string(),
            challenge_brief: Some(sample_challenge_brief()),
            code_explainer: QuestCodeExplainer {
                primary_invariant: "canRead must bind Fiber invoice, CKB proof hash, reader, and run before allowing paid access.".to_string(),
                denial_path: "The denial test mutates runId or reader and expects canRead to return false before reward state changes.".to_string(),
                proof_label: "Receipt proof".to_string(),
                proof_artifact: "A Fiber receipt plus CKB proof hash scoped to the active run and reader.".to_string(),
                network_label: "CKB/Fiber boundary".to_string(),
                network_boundary: "CKB anchors proof badge data while Fiber supplies invoice-bound payment evidence.".to_string(),
                risk_focus: "receipt replay across reader and run".to_string(),
                inspect_steps: vec![
                    "Trace canRead from the return statement.".to_string(),
                    "Match runId and reader checks to denial tests.".to_string(),
                    "Confirm the CKB proof hash is not only a UI label.".to_string(),
                    "Explain why Fiber payment evidence must be scoped.".to_string(),
                ],
                mentor_prompts: vec![
                    "What does canRead trust?".to_string(),
                    "Which denial test matters most?".to_string(),
                    "What proof comes from CKB?".to_string(),
                    "How would a replay attack work?".to_string(),
                ],
                resources: default_learning_resources().into_iter().take(2).collect(),
            },
            reward_logic: "CKB stores the proof badge and Fiber invoice-bound reward claim.".to_string(),
            ckb_fiber_hooks: vec![
                "CKB proof hash binds the quest receipt.".to_string(),
                "Fiber invoice binds the payout claim.".to_string(),
            ],
            workbench_files: vec![
                WorkbenchFile {
                    path: "src/receiptVerifier.ts".to_string(),
                    language: "ts".to_string(),
                    content: "export function canRead(receipt?: { runId: string; fiber: string; ckbProof: string }) { return Boolean(receipt && receipt.runId === 'vq-test' && receipt.fiber && receipt.ckbProof.startsWith('0x')); }".to_string(),
                },
                WorkbenchFile {
                    path: "tests/receiptVerifier.test.ts".to_string(),
                    language: "test.ts".to_string(),
                    content: "test('blocks unpaid reads', () => expect(canRead()).toBe(false)); test('rejects replayed receipts', () => expect(canRead({ runId: 'old', fiber: 'preimage', ckbProof: '0xabc' })).toBe(false));".to_string(),
                },
            ],
        };
        let now = BsonDateTime::now();
        QuestRunDocument {
            run_id: Uuid::nil().to_string(),
            user_address: "ckt1qjoyidvibequestwalletproof000000000000000000000000000".to_string(),
            build_prompt: "Build a Fiber paid-content app with CKB proof receipts".to_string(),
            skill_track: "Fiber Builder".to_string(),
            difficulty: "builder".to_string(),
            learning_context: None,
            source: QuestSource::OpenAi,
            wallet: WalletBinding {
                address: "ckt1qjoyidvibequestwalletproof000000000000000000000000000".to_string(),
                identity: "identity".to_string(),
                sign_type: "JoyId".to_string(),
                message: "VibeQuest wallet proof".to_string(),
            },
            quest,
            ship_requirements: ShipRequirements {
                ckb_rpc_ready: true,
                fiber_rpc_ready: true,
                can_claim_rewards: true,
            },
            progress: QuestProgress {
                gates: vec![
                    StoredGateProgress {
                        id: "identity".to_string(),
                        name: "Wallet Proof".to_string(),
                        description: "signed".to_string(),
                        is_completed: true,
                    },
                    StoredGateProgress {
                        id: "infrastructure".to_string(),
                        name: "Backend Readiness".to_string(),
                        description: "ready".to_string(),
                        is_completed: true,
                    },
                    StoredGateProgress {
                        id: "verification".to_string(),
                        name: "Generated Workspace Checks".to_string(),
                        description: "verified".to_string(),
                        is_completed: true,
                    },
                ],
                boss_fight_solved: true,
                shipped: false,
            },
            boss_attempts: Vec::new(),
            code_tutor_messages: Vec::new(),
            status: QuestRunStatus::InProgress,
            created_at: now,
            updated_at: now,
            completed_at: None,
            reward: RewardSnapshot {
                amount_shannons: "400".to_string(),
                currency: "Fibd".to_string(),
                sponsor: "vibequest-core".to_string(),
            },
        }
    }

    fn sample_challenge_brief() -> QuestChallengeBrief {
        QuestChallengeBrief {
            question: "Which proof makes the generated receipt verifier safe to ship?".to_string(),
            correct_answer: "Bind the Fiber invoice, CKB proof hash, reader, and run before allowing the paid read.".to_string(),
            wrong_answers: vec![
                ChallengeWrongAnswer {
                    label: "Trust the happy path fixture.".to_string(),
                    feedback: "The happy path does not attack replay.".to_string(),
                },
                ChallengeWrongAnswer {
                    label: "Only check that a wallet is connected.".to_string(),
                    feedback: "Wallet connection does not prove the receipt belongs to this read.".to_string(),
                },
                ChallengeWrongAnswer {
                    label: "Ship when the reward amount exists.".to_string(),
                    feedback: "Reward metadata is not code safety evidence.".to_string(),
                },
            ],
            invariant: "The receipt must bind run, reader, content, Fiber preimage, and CKB proof hash.".to_string(),
            attack_scenario: "A user replays another run's receipt against the active paid-content read.".to_string(),
            code_focus: "Inspect canReadPaidContent and every equality check.".to_string(),
            test_focus: "Mutate runId or reader in the denial test.".to_string(),
            hint: "Start with the field an attacker can copy, then prove the verifier rejects it.".to_string(),
            follow_up_question: "Which trusted receipt field would you mutate first to prove replay is blocked?".to_string(),
            resources: default_learning_resources().into_iter().take(2).collect(),
        }
    }
}
