#![recursion_limit = "256"]
#![allow(
    dead_code,
    reason = "legacy CKB implementation is intentionally unrouted during the v3 migration"
)]

pub mod auth;

pub mod platform;

use auth::{AuthVerifier, AuthenticatedPrincipal};
use axum::{
    Json, Router,
    extract::{Extension, Path, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
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
use reqwest::{Client, StatusCode as ReqwestStatusCode};
use ring::signature::{self, RsaPublicKeyComponents};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{env, error::Error, sync::Arc, time::Duration};
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
const QUICK_QUEST_OUTPUT_TOKENS: u16 = 5200;
const LEARNING_LESSON_OUTPUT_TOKENS: u16 = 1550;
const TUTOR_OUTPUT_TOKENS: u16 = 780;

#[derive(Clone)]
pub struct AppState {
    auth: AuthVerifier,
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
    interests: Vec<String>,
    learner_goal: String,
    background: String,
    pace: String,
}

#[derive(Debug, Deserialize)]
struct GenerateLearningLessonRequest {
    #[serde(default)]
    path_id: Option<String>,
    interests: Vec<String>,
    learner_goal: String,
    background: String,
    pace: String,
    lesson_index: usize,
}

impl GenerateLearningLessonRequest {
    fn module_request(&self) -> GenerateLearningModuleRequest {
        GenerateLearningModuleRequest {
            path_id: self.path_id.clone(),
            interests: self.interests.clone(),
            learner_goal: self.learner_goal.clone(),
            background: self.background.clone(),
            pace: self.pace.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct GenerateLearningModuleResponse {
    module_id: Uuid,
    source: QuestSource,
    module: LearningModule,
    warning: Option<String>,
}

#[derive(Debug, Serialize)]
struct GenerateLearningLessonResponse {
    source: QuestSource,
    module_title: String,
    learner_profile: String,
    outcome: String,
    capstone_quest_prompt: String,
    resources: Vec<LearningResource>,
    lesson: LearningLesson,
    lesson_index: usize,
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
    checkpoint: LearningCheckpoint,
    quest_bridge: String,
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

#[derive(Debug, Deserialize)]
struct AiLearningModuleCompact {
    t: String,
    l: Vec<AiLearningLessonCompact>,
}

#[derive(Debug, Deserialize)]
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
    user_address: String,
    wallet: WalletBinding,
    source: QuestSource,
    module: LearningModule,
    selected_interests: Vec<String>,
    learner_goal: String,
    background: String,
    pace: String,
    active_lesson_index: i64,
    checkpoint_answers: Document,
    tutor_messages: Vec<LearningTutorMessage>,
    created_at: BsonDateTime,
    updated_at: BsonDateTime,
}

#[derive(Debug, Deserialize)]
struct SaveLearningSessionRequest {
    wallet: WalletProof,
    module_id: Option<String>,
    source: Option<QuestSource>,
    module: LearningModule,
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
}

#[derive(Clone, Debug, Serialize)]
struct LearningSessionRecord {
    module_id: String,
    user_address: String,
    source: QuestSource,
    module: LearningModule,
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
    wallet: WalletProof,
    module_title: String,
    lesson_title: String,
    lesson_context: String,
    question: String,
}

#[derive(Debug, Serialize)]
struct SavedTutorExchangeResponse {
    answer: LearningTutorResponse,
    session: Option<LearningSessionRecord>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
        address: &str,
    ) -> Result<Option<LearningSessionRecord>, ApiError> {
        let address = address.trim();
        if address.is_empty() {
            return Err(ApiError::MissingWalletAddress);
        }

        let document = self
            .learning_sessions()
            .await?
            .find_one(doc! { "user_address": address })
            .await?;

        Ok(document.map(LearningSessionRecord::from))
    }

    async fn save_learning_session(
        &self,
        address: &str,
        request: SaveLearningSessionRequest,
    ) -> Result<LearningSessionRecord, ApiError> {
        validate_wallet_proof(&request.wallet)?;
        let address = address.trim();
        if address.is_empty() {
            return Err(ApiError::MissingWalletAddress);
        }
        if request.wallet.address.trim() != address {
            return Err(ApiError::WalletMismatch);
        }

        let wallet = wallet_binding_from_proof(&request.wallet);
        self.upsert_user(&wallet).await?;

        let module = compact_learning_module(request.module)?;
        let sessions = self.learning_sessions().await?;
        let existing = sessions.find_one(doc! { "user_address": address }).await?;
        let now = BsonDateTime::now();
        let id = request
            .module_id
            .filter(|module_id| !module_id.trim().is_empty())
            .or_else(|| existing.as_ref().map(|session| session.id.clone()))
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let created_at = existing
            .as_ref()
            .map(|session| session.created_at)
            .unwrap_or(now);
        let document = LearningSessionDocument {
            id: id.clone(),
            user_address: address.to_string(),
            wallet,
            source: request.source.unwrap_or(QuestSource::OpenAi),
            module,
            selected_interests: compact_string_list(request.selected_interests, 8, 80),
            learner_goal: clamp_text(request.learner_goal, 360),
            background: clamp_text(request.background, 80),
            pace: clamp_text(request.pace, 80),
            active_lesson_index: request.active_lesson_index.min(20) as i64,
            checkpoint_answers: checkpoint_answers_document(request.checkpoint_answers),
            tutor_messages: compact_tutor_messages(request.tutor_messages),
            created_at,
            updated_at: now,
        };

        sessions
            .replace_one(doc! { "user_address": address }, &document)
            .upsert(true)
            .await?;

        Ok(document.into())
    }

    async fn append_tutor_exchange(
        &self,
        address: &str,
        request: &SaveTutorExchangeRequest,
        answer: &LearningTutorResponse,
    ) -> Result<Option<LearningSessionRecord>, ApiError> {
        if !self.is_configured() {
            return Ok(None);
        }

        let address = address.trim();
        let mut session = match self
            .learning_sessions()
            .await?
            .find_one(doc! { "user_address": address })
            .await?
        {
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
            .replace_one(doc! { "_id": &session.id }, &session)
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

impl From<LearningSessionDocument> for LearningSessionRecord {
    fn from(session: LearningSessionDocument) -> Self {
        Self {
            module_id: session.id,
            user_address: session.user_address,
            source: session.source,
            module: session.module,
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
        EcosystemRegistry::zcash_only().expect("the built-in ecosystem registry must be valid");
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
        .route("/v3/me", get(v3_me).delete(v3_delete_account))
        .route("/v3/me/export", get(v3_export_account))
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
            .min(DEFAULT_OPENAI_TIMEOUT_SECONDS);

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
        for lesson_index in 0..5 {
            lessons.push(self.generate_learning_lesson(request, lesson_index).await?);
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
    ) -> Result<LearningLesson, ApiError> {
        let compact = self.generate_learning_lesson(request, lesson_index).await?;
        compact_ai_lesson_to_learning_lesson(
            lesson_index,
            &learning_background_label(request),
            &learning_focus_label(request),
            compact,
        )
    }

    async fn generate_learning_lesson(
        &self,
        request: &GenerateLearningModuleRequest,
        lesson_index: usize,
    ) -> Result<AiLearningLessonCompact, ApiError> {
        match self
            .request_learning_lesson(request, lesson_index, false)
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
                        warn!(lesson_index, "AI lesson response failed validation");
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
    ) -> Result<AiLearningLessonCompact, ApiError> {
        let prompt = learning_lesson_prompt(request, lesson_index, repair);
        let lesson_timeout = if self.timeout > Duration::from_secs(45) {
            Duration::from_secs(45)
        } else {
            self.timeout
        };
        let lesson = self
            .post_openai_json::<AiLearningLessonCompact>(
                prompt,
                LEARNING_LESSON_OUTPUT_TOKENS,
                ReasoningEffort::None,
                lesson_timeout,
            )
            .await?;
        if let Err(error) = validate_ai_learning_lesson_compact(&lesson) {
            warn!(
                lesson_index,
                title = %clamp_text(lesson.t.clone(), 120),
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

async fn generate_learning_module(
    State(state): State<Arc<AppState>>,
    Json(request): Json<GenerateLearningModuleRequest>,
) -> Result<Json<GenerateLearningModuleResponse>, ApiError> {
    if request.learner_goal.trim().chars().count() < 8 && request.interests.is_empty() {
        return Err(ApiError::InvalidPrompt);
    }

    let module = state.openai.generate_learning_module(&request).await?;
    let source = QuestSource::OpenAi;

    Ok(Json(GenerateLearningModuleResponse {
        module_id: Uuid::new_v4(),
        source,
        module,
        warning: None,
    }))
}

async fn generate_learning_lesson_endpoint(
    State(state): State<Arc<AppState>>,
    Json(request): Json<GenerateLearningLessonRequest>,
) -> Result<Json<GenerateLearningLessonResponse>, ApiError> {
    let module_request = request.module_request();
    if module_request.learner_goal.trim().chars().count() < 8 && module_request.interests.is_empty()
    {
        return Err(ApiError::InvalidPrompt);
    }
    if request.lesson_index >= 5 {
        return Err(ApiError::InvalidPrompt);
    }

    let lesson = state
        .openai
        .generate_learning_lesson_item(&module_request, request.lesson_index)
        .await?;

    Ok(Json(GenerateLearningLessonResponse {
        source: QuestSource::OpenAi,
        module_title: learning_module_title(&module_request),
        learner_profile: learning_module_profile(&module_request),
        outcome: learning_module_outcome(&module_request),
        capstone_quest_prompt: learning_module_capstone_prompt(&module_request),
        resources: default_learning_resources(),
        lesson,
        lesson_index: request.lesson_index,
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
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> Result<Json<LearningSessionResponse>, ApiError> {
    Ok(Json(LearningSessionResponse {
        session: state.store.get_learning_session(&address).await?,
    }))
}

async fn api_save_learning_session(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
    Json(request): Json<SaveLearningSessionRequest>,
) -> Result<Json<LearningSessionRecord>, ApiError> {
    Ok(Json(
        state.store.save_learning_session(&address, request).await?,
    ))
}

async fn api_save_learning_tutor_exchange(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
    Json(request): Json<SaveTutorExchangeRequest>,
) -> Result<Json<SavedTutorExchangeResponse>, ApiError> {
    validate_wallet_proof(&request.wallet)?;
    if request.wallet.address.trim() != address.trim() {
        return Err(ApiError::WalletMismatch);
    }
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
    let session = state
        .store
        .append_tutor_exchange(&address, &request, &answer)
        .await?;

    Ok(Json(SavedTutorExchangeResponse { answer, session }))
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
        "ckb", "cell", "witness", "script", "xudt", "fiber", "invoice", "ptlc", "htlc", "channel",
        "proof", "receipt", "payout", "runid",
    ]
    .iter()
    .any(|term| lower.contains(term));
    let has_denial_signal = [
        "reject", "block", "false", "throw", "invalid", "unpaid", "mismatch", "replay",
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
        "ckb", "cell", "witness", "script", "xudt", "fiber", "invoice", "ptlc", "htlc", "channel",
        "proof", "receipt", "payout",
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
        "cell", "witness", "script", "xudt", "fiber", "invoice", "ptlc", "htlc", "channel",
        "proof", "receipt", "payout", "reader", "run", "content", "outpoint", "nonce",
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
    let generic_output = [
        "generic template",
        "generic challenge",
        "placeholder",
        "stock variable",
        "sample quest",
        "paywall reactor",
        "fiber proof run",
    ]
    .iter()
    .any(|phrase| haystack.contains(phrase));

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
    module.title = clamp_text(module.title, 80);
    module.learner_profile = clamp_text(module.learner_profile, 180);
    module.outcome = clamp_text(module.outcome, 220);
    module.capstone_quest_prompt = clamp_text(module.capstone_quest_prompt, 360);

    if module.lessons.len() > 5 {
        module.lessons.truncate(5);
    }

    if module.lessons.len() < 5 {
        return Err(ApiError::InvalidAiResponse);
    }

    for (index, lesson) in module.lessons.iter_mut().enumerate() {
        if lesson.id.trim().is_empty() {
            lesson.id = format!("lesson-{}", index + 1);
        }
        lesson.title = clamp_text(lesson.title.clone(), 80);
        lesson.why_it_matters = clamp_text(lesson.why_it_matters.clone(), 620);
        lesson.explanation = clamp_text(lesson.explanation.clone(), 3200);
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
            lesson.concepts.push("CKB/Fiber trust boundary".to_string());
        }

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

fn compact_learning_resources(resources: Vec<LearningResource>) -> Vec<LearningResource> {
    let mut compacted = resources
        .into_iter()
        .filter(|resource| resource.title.trim().len() > 1 && resource.url.starts_with("https://"))
        .map(|resource| LearningResource {
            title: clamp_text(resource.title, 80),
            url: clamp_text(resource.url, 160),
            reason: clamp_text(resource.reason, 180),
        })
        .take(4)
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
            reason: "Reference cells, scripts, witnesses, transactions, and token state."
                .to_string(),
        },
        LearningResource {
            title: "Fiber Network Repository".to_string(),
            url: "https://github.com/nervosnetwork/fiber".to_string(),
            reason: "Reference Fiber payment channels, invoices, PTLC-based security, routing, and node behavior."
                .to_string(),
        },
        LearningResource {
            title: "JoyID Documentation".to_string(),
            url: "https://docs.joyid.dev/".to_string(),
            reason: "Reference passkey wallet flows and signer identity assumptions.".to_string(),
        },
    ]
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
    if compact.l.len() < 5 {
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

    let lessons = compact
        .l
        .into_iter()
        .take(5)
        .enumerate()
        .map(|(index, lesson)| {
            compact_ai_lesson_to_learning_lesson(index, &background, &focus, lesson)
        })
        .collect::<Result<Vec<_>, _>>()?;

    compact_learning_module(LearningModule {
        title: non_empty_or(
            compact.t,
            &format!("VibeQuest: {} Deep Dive", clamp_text(focus.clone(), 52)),
        ),
        learner_profile: learning_module_profile(request),
        outcome: learning_module_outcome(request),
        lessons,
        capstone_quest_prompt: learning_module_capstone_prompt(request),
        resources: default_learning_resources(),
    })
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

    if lesson.t.trim().is_empty()
        || explainer_words < 300
        || lesson.s.trim().is_empty()
        || why_words < 35
        || bridge_words < 22
        || lesson.f.trim().is_empty()
        || lesson.q.trim().is_empty()
        || generic_learning_checkpoint_question(&lesson.q)
        || lesson.a.trim().is_empty()
        || wrong_answer_count != 3
        || wrong_feedback_count != 3
    {
        return Err(ApiError::InvalidAiResponse);
    }

    Ok(())
}

fn generic_learning_checkpoint_question(question: &str) -> bool {
    let lower = question.trim().to_ascii_lowercase();
    let names_domain_term = [
        "cell", "outpoint", "witness", "script", "channel", "invoice", "nonce", "ptlc", "joyid",
        "xudt", "fiber", "receipt", "capacity", "lock", "type",
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
    lesson: AiLearningLessonCompact,
) -> Result<LearningLesson, ApiError> {
    validate_ai_learning_lesson_compact(&lesson)?;

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
        title,
        why_it_matters,
        explanation: expanded_learning_explanation(&lesson),
        concepts: concepts.clone(),
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

fn learning_module_title(request: &GenerateLearningModuleRequest) -> String {
    format!(
        "VibeQuest: {} Deep Dive",
        clamp_text(learning_focus_label(request), 52)
    )
}

fn learning_focus_label(request: &GenerateLearningModuleRequest) -> String {
    let interests = request
        .interests
        .iter()
        .map(|interest| interest.trim())
        .filter(|interest| !interest.is_empty())
        .take(4)
        .collect::<Vec<_>>();

    if interests.is_empty() {
        "CKB/Fiber".to_string()
    } else {
        interests.join(" + ")
    }
}

fn learning_background_label(request: &GenerateLearningModuleRequest) -> String {
    let background = request.background.trim();
    if background.is_empty() {
        "learner".to_string()
    } else {
        background.to_string()
    }
}

fn learning_module_profile(request: &GenerateLearningModuleRequest) -> String {
    format!(
        "A {} learning {} through live AI-authored deep modules, code snippets, checkpoints, tutor support, and practical quest handoffs.",
        learning_background_label(request),
        learning_focus_label(request)
    )
}

fn learning_module_outcome(request: &GenerateLearningModuleRequest) -> String {
    format!(
        "Explain {} trust boundaries, read generated verifier code, answer code-aware checkpoints, and turn passed lessons into quests.",
        learning_focus_label(request)
    )
}

fn learning_module_capstone_prompt(request: &GenerateLearningModuleRequest) -> String {
    format!(
        "Generate a {} verifier quest with proof binding, denial tests, a boss question, and a reward-safe ship gate.",
        learning_focus_label(request)
    )
}

fn learning_lesson_role(path_id: Option<&str>, lesson_index: usize) -> &'static str {
    let roles = match path_id.unwrap_or_default().to_ascii_lowercase().as_str() {
        "ckb-cells" => [
            "state model and live-cell evidence",
            "OutPoint lineage, inputs, outputs, and transaction scope",
            "lock scripts, type scripts, witnesses, and local verifier trust",
            "denial testing for copied cell data, stale witnesses, and fake frontend payloads",
            "turning CKB cell understanding into a generated verifier quest",
        ],
        "fiber-payments" => [
            "payment channel mental model and CKB-backed settlement assumptions",
            "invoice, PTLC, route, and channel-state proof boundaries",
            "paid-content receipt verification and replay-resistant access control",
            "xUDT amount, balance transition, and payout integrity checks",
            "turning Fiber payment understanding into a generated verifier quest",
        ],
        "security-audits" => [
            "threat modeling CKB/Fiber flows before trusting generated code",
            "replay, mismatch, stale state, and witness-substitution attacks",
            "JoyID signer scope, domain binding, nonce freshness, and action intent",
            "denial tests that mutate the exact proof boundary under review",
            "turning audit findings into a generated fix-and-defend quest",
        ],
        _ => [
            "core mental model for the selected CKB/Fiber topic",
            "proof boundary and generated-code reading habit",
            "wallet, payment, state, or verifier integration risk",
            "attack case and denial test design",
            "turning the lesson into a practical generated quest",
        ],
    };

    roles[lesson_index.min(4)]
}

fn learning_speciality_directive(background: &str) -> &'static str {
    let lower = background.to_ascii_lowercase();
    if lower.contains("vibecoder") {
        "The learner uses AI to generate code quickly. Teach them how to slow down at the proof boundary, read generated code, name trusted fields, and design denial tests before believing the AI output."
    } else if lower.contains("backend") {
        "The learner writes backend services. Emphasize verifier placement, database versus chain evidence, request tampering, authorization scope, replay prevention, and tests that run outside the frontend."
    } else if lower.contains("frontend") {
        "The learner builds interfaces. Explain what the UI may display versus what the backend or chain must verify, how wallet UX can mislead, and how to surface proof state without trusting client labels."
    } else if lower.contains("security") || lower.contains("auditor") {
        "The learner reviews systems for risk. Emphasize threat models, attacker-controlled fields, stale proofs, replay paths, denial tests, and how to prove generated code rejects the intended attack."
    } else if lower.contains("product") || lower.contains("community") {
        "The learner may not write every line of code. Explain value, risk, trust boundaries, user stories, and plain-language failure cases while still pointing to the technical evidence that matters."
    } else {
        "Teach through concrete CKB/Fiber examples, generated-code reading habits, proof boundaries, and denial tests that fit the learner's stated background."
    }
}

fn learning_focus_directive(path_id: Option<&str>) -> &'static str {
    match path_id.unwrap_or_default().to_ascii_lowercase().as_str() {
        "ckb-cells" => {
            "Ground the lesson in CKB cells, capacity, cell data, OutPoint lineage, inputs and outputs, lock/type scripts, witnesses, transaction evidence, and local verifier boundaries."
        }
        "fiber-payments" => {
            "Ground the lesson in Fiber payment channels, invoices, PTLC-based security, routing, off-chain channel state, CKB settlement assumptions, and paid-access receipt verification."
        }
        "security-audits" => {
            "Ground the lesson in replay defense, witness mismatch, stale Fiber state, JoyID signer scope, xUDT payout integrity, denial tests, and reward-safe ship gates."
        }
        _ => {
            "Ground the lesson in the selected CKB/Fiber/JoyID interests and make the proof boundary clear enough to become a practical quest."
        }
    }
}

fn learning_lesson_prompt(
    request: &GenerateLearningModuleRequest,
    lesson_index: usize,
    repair: bool,
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
        "CKB foundations, Fiber payments, JoyID wallet UX".to_string()
    } else {
        interests
    };
    let background = request.background.trim();
    let background = if background.is_empty() {
        "learner"
    } else {
        background
    };
    let repair_directive = if repair {
        "The previous lesson was rejected because it was short, generic, or incomplete. Return a complete lesson this time: the e field alone must be 335-365 words and must teach, not summarize."
    } else {
        ""
    };

    format!(
        r#"Return minified JSON only with keys exactly: t,e,s,w,j,f,q,a,b,bf,ci. No markdown or prose outside JSON.

VibeQuest module {module_number}/5. Role: {module_role}. Interests: {interests}. Learner goal: {goal}. Speciality: {background}. Pace: {pace}. Focus: {focus_directive}. Speciality lens: {speciality_directive}. {repair_directive}

Ground facts in official sources without quoting them: CKB docs https://docs.nervos.org/ for cells/scripts/witnesses/transactions, Fiber repo https://github.com/nervosnetwork/fiber for channels/invoices/PTLC/routing/node behavior, JoyID docs https://docs.joyid.dev/ for passkey signer UX.

e must be 335-365 words of real teaching prose with paragraphs. Define the key terms, explain how the idea appears in generated TypeScript or Rust, name one realistic builder mistake, and describe one denial-test idea. s is one matching TypeScript/Rust code lens line. w is 35-60 words on why it matters to this speciality. j is 22-45 words naming the practice quest artifact and denial test. f is one follow-up reasoning question. q is one checkpoint about this lesson's exact proof boundary and must name concrete fields or concepts such as cell, OutPoint, witness, script, channel, invoice, nonce, PTLC, JoyID challenge, or xUDT split. Do not ask generic questions like "What is the exact proof boundary for this lesson?". a is the specific correct answer. b has exactly 3 plausible wrong answer labels. bf has exactly 3 matching feedback strings. ci is 0-3 and must vary. Avoid meta labels such as old fallback wording. Seed: {nonce}."#,
        module_number = lesson_index + 1,
        module_role = learning_lesson_role(request.path_id.as_deref(), lesson_index),
        goal = request.learner_goal.trim(),
        background = background,
        pace = request.pace.trim(),
        focus_directive = learning_focus_directive(request.path_id.as_deref()),
        speciality_directive = learning_speciality_directive(background),
    )
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
- Teach the CKB/Fiber concept behind the code, then explain the vibecoding mistake this prevents.
- If the learner asks for a patch, describe the change and the denial test to add.
- code_walkthrough: 3-5 short bullets, each tied to a concrete line/function/field in the generated files.
- common_misunderstanding: name the likely wrong mental model and correct it.
- follow_up_question: ask one question that checks whether the learner truly understood this code.
- references: 2-3 authoritative links with title,url,reason. Prefer CKB Docs, Fiber repo, JoyID docs when relevant.
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
- references: 2-3 authoritative links with title,url,reason. Prefer CKB Docs, Fiber repo, JoyID docs when relevant.
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
- Every field must be authored for this exact request or lesson context. Do not use a generic paywall, generic quiz, stock variable names, or placeholder prose.
- For lesson-derived quests, invent names from the lesson. Do not use cellVerifier, verifyCkbCellProof, verifyGeneratedReceipt, src/quest.ts, test/quest.test.ts, ACTIVE_RUN_ID, LESSON_INVARIANT, Fiber Proof Run, Paywall Reactor, or titles ending in Practice Quest.
- title: specific to the generated quest, max 80 chars.
- premise: 2 concise sentences explaining the concrete CKB/Fiber risk the learner is practicing.
- build_objective: concrete objective from the request/lesson, max 420 chars.
- comprehension_gates: exactly 3 specific gates. Gate 1 explains the invariant. Gate 2 runs or reads the denial test. Gate 3 ships only after defending the generated diff.
- boss_fight: code-specific challenge tied to the verifier function and denial test.
- reward_logic: explain when XP/badge/reward claim unlocks, no fake payout promises.
- ckb_fiber_hooks: exactly 2 concrete hooks, one CKB-side and one Fiber-side.
- workbench_files: exactly 2 files. One implementation file and one test file. Each has path, language, content. File paths must be specific to the lesson scenario.
- implementation content: TypeScript or Rust, 45-95 lines max. Export types and one verifier/settlement function whose name is specific to the lesson. It must mention concrete CKB/Fiber terms such as cell, OutPoint, witness, script, xUDT, invoice, PTLC/HTLC, channel state, nonce, JoyID challenge, receipt, or payout.
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
            registry: EcosystemRegistry::zcash_only().expect("valid test registry"),
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
    fn missing_integrations_include_ckb_and_fiber() {
        let state = AppState {
            auth: AuthVerifier::disabled(),
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
            registry: EcosystemRegistry::zcash_only().expect("valid test registry"),
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
            interests: vec!["CKB Foundations".to_string(), "Fiber Payments".to_string()],
            learner_goal: "Understand generated CKB/Fiber verifier code".to_string(),
            background: "Backend dev".to_string(),
            pace: "Deep dive".to_string(),
        };
        let lesson_body = "Backend checks should treat a JoyID-signed payload as evidence for one exact action, not as a reusable login token. In a CKB/Fiber flow, the learner should trace how the wallet address, challenge nonce, domain, action name, CKB cell reference, and Fiber channel request are assembled before signature verification. The important detail is that JoyID makes the signing experience feel simple, but the verifier still has to decide what the user actually approved. A passkey signature proves control of a signer; it does not automatically prove that the same signer approved this run, this route, this invoice, or this generated code path. CKB adds another boundary because cells, scripts, witnesses, and transaction evidence describe state in a way a frontend payload cannot replace. Fiber adds a payment boundary because channel state and invoice proof must be scoped to the content, amount, and settlement expectation. The failure mode is subtle: a vibe-coded route may check that a signature exists while letting the same signed text approve another payout, channel action, or generated quest run. A stronger verifier binds the challenge to the current run, refuses stale nonces, records which proof was consumed, and checks that the CKB or Fiber evidence being trusted is the evidence named in the signed message. The denial test should copy a valid signature into a request with a different run id, CKB cell, Fiber invoice, or action name and prove the backend rejects it before any badge or reward state changes. That test teaches the learner to read generated code by asking what exact proof boundary the code defends, not whether the UI looks authenticated. The practical review habit is to read the generated function from the accepting return statement backward. Every value that makes the function return true should come from either the signed JoyID message, the CKB transaction evidence, or the Fiber payment state. If a value appears only in request JSON, the learner should treat it as attacker-controlled until a denial test proves otherwise.";
        let compact = AiLearningModuleCompact {
            t: "CKB Fiber JoyID from vibecoded code".to_string(),
            l: (0..5)
                .map(|index| AiLearningLessonCompact {
                    t: format!("Module {index} Verify CKB/Fiber proof"),
                    e: lesson_body.to_string(),
                    s: "const ok = await verifyJoyIDSignature({ message, signature, address });".to_string(),
                    w: "For a backend developer, this matters because generated wallet code often confuses signer presence with authorization. The lesson forces the learner to connect JoyID proof, CKB state evidence, Fiber payment context, and denial tests before trusting a generated verifier.".to_string(),
                    j: "Generate a verifier quest that binds JoyID challenge fields to CKB cell evidence and Fiber invoice context, then rejects replayed run ids or mismatched payment state.".to_string(),
                    f: "Which signed field would you mutate first to prove the authorization cannot be replayed?".to_string(),
                    q: "What must the backend verify before trusting a JoyID-authorized Fiber request?".to_string(),
                    a: "JoyID signature over the exact challenge payload and action context".to_string(),
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

        let module = build_learning_module_from_compact_ai(&request, compact).unwrap();

        assert_eq!(module.lessons.len(), 5);
        assert!(
            module.lessons[0]
                .explanation
                .contains("JoyID-signed payload")
        );
        assert!(module.lessons[0].explanation.split_whitespace().count() >= 300);
        assert!(module.lessons[0].explanation.contains("Code lens:"));
        assert!(
            module.lessons[0]
                .explanation
                .contains("verifyJoyIDSignature")
        );
        assert_eq!(module.lessons[0].checkpoint.options.len(), 4);
        assert!(
            module.lessons[0]
                .why_it_matters
                .to_lowercase()
                .contains("backend")
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
