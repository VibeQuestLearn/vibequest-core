use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use mongodb::{
    Client as MongoClient, Database, IndexModel,
    bson::{DateTime as BsonDateTime, Document, doc},
    options::{ClientOptions, IndexOptions},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::sync::OnceCell;

pub const SCHEMA_VERSION: u16 = 3;
pub const DEFAULT_V3_DATABASE: &str = "vibequestlearn_v3";
pub const BASICS_ECOSYSTEM_ID: &str = "basics";
pub const CKB_ECOSYSTEM_ID: &str = "ckb";
pub const FIBER_ECOSYSTEM_ID: &str = "fiber";
pub const STACKS_ECOSYSTEM_ID: &str = "stacks";
pub const TON_STONFI_ECOSYSTEM_ID: &str = "ton-stonfi";
pub const GOLEM_ECOSYSTEM_ID: &str = "golem";
pub const AIBTC_ECOSYSTEM_ID: &str = "aibtc";
pub const ZCASH_ECOSYSTEM_ID: &str = "zcash";
pub const SHIELDED_PAYMENTS_TRACK_ID: &str = "shielded-payments-safety";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CatalogResponse {
    pub schema_version: u16,
    pub ecosystems: Vec<EcosystemRegistration>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct EcosystemRegistration {
    pub ecosystem_id: String,
    pub name: String,
    pub summary: String,
    pub enabled: bool,
    pub configuration: EcosystemConfiguration,
    pub tracks: Vec<TrackRegistration>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "configuration", rename_all = "kebab-case")]
pub enum EcosystemConfiguration {
    Basics(GenericEcosystemRegistration),
    Ckb(GenericEcosystemRegistration),
    Fiber(GenericEcosystemRegistration),
    Stacks(GenericEcosystemRegistration),
    TonStonfi(GenericEcosystemRegistration),
    Golem(GenericEcosystemRegistration),
    Aibtc(GenericEcosystemRegistration),
    Zcash(ZcashRegistration),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct GenericEcosystemRegistration {
    pub source_pack_version: String,
    pub primary_sources: Vec<String>,
    pub learning_focus: Vec<String>,
    pub validation_mode: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ZcashRegistration {
    pub network: String,
    pub address_standard: String,
    pub payment_request_standard: String,
    pub custody_mode: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct TrackRegistration {
    pub track_id: String,
    pub title: String,
    pub summary: String,
    pub enabled: bool,
    pub status: TrackStatus,
    pub track_version: String,
    pub content_version: String,
    pub source_manifest_version: String,
    pub runner_manifest_version: String,
    pub runner_version: String,
    pub runner_status: RunnerStatus,
    pub lesson_count: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_proof: Option<TrackReviewProof>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct TrackReviewProof {
    pub proof_label: String,
    pub sample_topic: String,
    pub sample_modules: Vec<String>,
    pub required_artifacts: Vec<String>,
    pub reviewer_demo_steps: Vec<String>,
    pub source_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerStatus {
    ReviewRequired,
    Enabled,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrackStatus {
    Building,
    Enabled,
    Retired,
}

fn generic_registration<const S: usize, const F: usize>(
    source_pack_version: &str,
    primary_sources: [&str; S],
    learning_focus: [&str; F],
) -> GenericEcosystemRegistration {
    GenericEcosystemRegistration {
        source_pack_version: source_pack_version.to_string(),
        primary_sources: primary_sources.into_iter().map(str::to_string).collect(),
        learning_focus: learning_focus.into_iter().map(str::to_string).collect(),
        validation_mode: "ai-generated-source-grounded".to_string(),
    }
}

fn generic_ecosystem(
    ecosystem_id: &str,
    name: &str,
    summary: &str,
    configuration: EcosystemConfiguration,
) -> EcosystemRegistration {
    EcosystemRegistration {
        ecosystem_id: ecosystem_id.to_string(),
        name: name.to_string(),
        summary: summary.to_string(),
        enabled: true,
        configuration,
        tracks: vec![TrackRegistration {
            track_id: format!("{}-learning-track", ecosystem_id),
            title: format!("{} Learning Track", name),
            summary: "AI-generated lessons must pass source grounding, depth, checkpoint, and placeholder validation before display.".to_string(),
            enabled: true,
            status: TrackStatus::Enabled,
            track_version: "1.0.0".to_string(),
            content_version: "2026-07-26.1".to_string(),
            source_manifest_version: format!("{}-source-pack-1.0.0", ecosystem_id),
            runner_manifest_version: crate::runner::RUNNER_MANIFEST_VERSION.to_string(),
            runner_version: crate::runner::RUNNER_VERSION.to_string(),
            runner_status: RunnerStatus::ReviewRequired,
            lesson_count: 5,
            review_proof: None,
        }],
    }
}

fn golem_review_proof() -> TrackReviewProof {
    TrackReviewProof {
        proof_label: "Golem grant proof sample".to_string(),
        sample_topic: "Golem compute lab: requestor/provider execution, JS SDK task run, result validation, and failure matrix".to_string(),
        sample_modules: vec![
            "Compute mental model: requestor intent, provider execution, Yagna coordination, agreements, allocations, results, and payments".to_string(),
            "JS SDK execution lab: define a task, negotiate compute, collect output, validate result shape, and clean up".to_string(),
            "Python and Ray pathing: choose the right workload path, split work, respect Ray limitations, and verify outputs".to_string(),
            "dApp lifecycle: GVMI images, descriptors, services, logs, proxies, health checks, and deployment failure states".to_string(),
            "Final compute quest: build a proof map, run denial cases, and explain why provider output is not automatically trusted".to_string(),
        ],
        required_artifacts: vec![
            "source-grounded generated course".to_string(),
            "validation artifact with source IDs and categories".to_string(),
            "code-mode sample inspected or copied".to_string(),
            "checkpoint attempt and pass evidence".to_string(),
            "final compute quest visibility".to_string(),
            "usage metrics for starts, completions, tutor use, and code-copy events".to_string(),
        ],
        reviewer_demo_steps: vec![
            "Open VibeQuest and sign in with Google".to_string(),
            "Select Learn, choose Golem, and use the grant-proof sample topic".to_string(),
            "Enable interactive code samples and generate the course".to_string(),
            "Open module 1 as soon as it appears while modules 2-5 continue generating".to_string(),
            "Inspect validation gates, source categories, execution path, failure cases, and code sample".to_string(),
            "Answer a checkpoint and open the final compute quest path".to_string(),
        ],
        source_ids: vec![
            "golem-ecosystem-fund".to_string(),
            "golem-docs".to_string(),
            "golem-quickstarts".to_string(),
            "golem-js-sdk".to_string(),
            "golem-js-task-model".to_string(),
            "golem-js-executing-tasks".to_string(),
            "golem-requestor-provider".to_string(),
            "golem-python-quickstart".to_string(),
            "golem-python-fundamentals".to_string(),
            "golem-ray".to_string(),
            "golem-ray-limitations".to_string(),
            "golem-dapp-hello-world".to_string(),
            "golem-dapp-creation".to_string(),
            "golem-provider-overview".to_string(),
            "golem-provider-architecture".to_string(),
        ],
    }
}

fn aibtc_review_proof() -> TrackReviewProof {
    TrackReviewProof {
        proof_label: "AIBTC agent lab proof sample".to_string(),
        sample_topic: "AIBTC agent lab: signed agent actions, bounty workflow, sBTC payment proof, and reputation evidence".to_string(),
        sample_modules: vec![
            "Agent economy mental model: agent identity, public work history, signed actions, and UI claim boundaries".to_string(),
            "Agent registration and signed actions: BTC/STX signatures, request scope, replay risk, and verification fields".to_string(),
            "Bounty workflow: bounty fields, submission window, fixed-winner assumptions, proof artifact, and review state".to_string(),
            "sBTC payment proof: transfer evidence, BNTY memo binding, confirmation state, and reputation trail".to_string(),
            "Final agent quest: design a safe bounty flow, validate signed payloads, and reject unsafe payment or autonomy claims".to_string(),
        ],
        required_artifacts: vec![
            "source-grounded generated AIBTC course".to_string(),
            "validation artifact with AIBTC source IDs and signed-action coverage".to_string(),
            "code-mode sample for safe payload or proof validation".to_string(),
            "checkpoint attempt and pass evidence".to_string(),
            "final agent quest visibility".to_string(),
            "usage metrics for starts, completions, tutor use, and code-copy events".to_string(),
        ],
        reviewer_demo_steps: vec![
            "Open VibeQuest and sign in with Google".to_string(),
            "Select Learn, choose AIBTC / Stacks Agents, and use the sample topic".to_string(),
            "Enable optional code samples and generate the course".to_string(),
            "Inspect source categories, signed-action coverage, bounty coverage, and payment-proof coverage".to_string(),
            "Answer a checkpoint and confirm the final agent quest path is visible".to_string(),
        ],
        source_ids: vec![
            "aibtc-home".to_string(),
            "aibtc-llms".to_string(),
            "aibtc-bounties".to_string(),
            "aibtc-bounty-new".to_string(),
            "aibtc-bounties-docs".to_string(),
            "aibtc-openapi".to_string(),
            "stacks-docs".to_string(),
        ],
    }
}

fn aibtc_ecosystem() -> EcosystemRegistration {
    let mut ecosystem = generic_ecosystem(
        AIBTC_ECOSYSTEM_ID,
        "AIBTC / Stacks Agents",
        "Agent-economy learning paths for signed AIBTC actions, bounty workflows, sBTC payment proof, Stacks identity, and public work reputation.",
        EcosystemConfiguration::Aibtc(generic_registration(
            "aibtc-source-pack-1.0.0",
            [
                "AIBTC home",
                "AIBTC LLM source map",
                "AIBTC bounties",
                "AIBTC bounty creation",
                "AIBTC bounty workflow documentation",
                "AIBTC OpenAPI schema",
                "Stacks documentation",
            ],
            [
                "agent identity and registration",
                "BTC and STX signed actions",
                "AIBTC bounty creation",
                "AIBTC bounty submission",
                "sBTC payment proof and BNTY memo evidence",
                "x402 and paid agent interactions",
                "public work reputation",
                "unsafe autonomy and wallet boundary denial tests",
            ],
        )),
    );

    if let Some(track) = ecosystem.tracks.first_mut() {
        track.track_id = "aibtc-agent-lab".to_string();
        track.title = "AIBTC Agent Lab: Sign, Submit, Prove".to_string();
        track.summary = "A source-grounded agent-economy onboarding track where builders learn signed AIBTC actions, bounty workflows, sBTC payment proof, and reputation evidence without exposing wallet secrets.".to_string();
        track.content_version = "2026-08-07.1".to_string();
        track.review_proof = Some(aibtc_review_proof());
    }

    ecosystem
}

fn golem_ecosystem() -> EcosystemRegistration {
    let mut ecosystem = generic_ecosystem(
        GOLEM_ECOSYSTEM_ID,
        "Golem",
        "Decentralized compute labs for builders learning requestor/provider workflows, SDK task execution, Ray workloads, dApp deployment, and failure-state handling.",
        EcosystemConfiguration::Golem(generic_registration(
            "golem-source-pack-1.0.0",
            [
                "Golem Ecosystem Fund",
                "Golem Docs",
                "Golem Quickstarts",
                "Golem JS SDK documentation",
                "Golem JS task model",
                "Golem JS task execution examples",
                "Golem requestor/provider interaction documentation",
                "Golem Python quickstart",
                "Golem Python application fundamentals",
                "Ray on Golem documentation",
                "Ray on Golem limitations",
                "Golem dApp deployment documentation",
                "Golem provider overview",
                "Golem provider architecture",
            ],
            [
                "requestor and provider mental model",
                "Yagna setup and app keys",
                "JS SDK task execution",
                "Python SDK task execution",
                "Ray on Golem workloads",
                "dApp deployment lifecycle",
                "provider selection and pricing awareness",
                "task lifecycle and result handling",
                "failure-state denial tests",
            ],
        )),
    );

    if let Some(track) = ecosystem.tracks.first_mut() {
        track.track_id = "golem-compute-lab".to_string();
        track.title = "Golem Compute Lab: Learn, Run, Validate".to_string();
        track.summary = "A source-grounded compute onboarding track where builders learn Golem requestor/provider execution, inspect SDK paths, and prove failure handling before shipping.".to_string();
        track.content_version = "2026-08-05.1".to_string();
        track.review_proof = Some(golem_review_proof());
    }

    ecosystem
}

fn zcash_ecosystem() -> EcosystemRegistration {
    EcosystemRegistration {
        ecosystem_id: ZCASH_ECOSYSTEM_ID.to_string(),
        name: "Zcash".to_string(),
        summary: "Shielded payment integration labs for learners and builders.".to_string(),
        enabled: true,
        configuration: EcosystemConfiguration::Zcash(ZcashRegistration {
            network: "testnet".to_string(),
            address_standard: "ZIP-316".to_string(),
            payment_request_standard: "ZIP-321".to_string(),
            custody_mode: "non-custodial-labs".to_string(),
        }),
        tracks: vec![TrackRegistration {
            track_id: SHIELDED_PAYMENTS_TRACK_ID.to_string(),
            title: "Shielded Payments: Accept, Detect, and Defend".to_string(),
            summary: "Implement one safe shielded checkout boundary and prove denial behavior."
                .to_string(),
            enabled: false,
            status: TrackStatus::Building,
            track_version: "1.0.0".to_string(),
            content_version: "2026-07-21.1".to_string(),
            source_manifest_version: crate::zcash::SOURCE_MANIFEST_VERSION.to_string(),
            runner_manifest_version: crate::runner::RUNNER_MANIFEST_VERSION.to_string(),
            runner_version: crate::runner::RUNNER_VERSION.to_string(),
            runner_status: RunnerStatus::ReviewRequired,
            lesson_count: 5,
            review_proof: None,
        }],
    }
}

#[derive(Clone, Debug)]
pub struct EcosystemRegistry {
    catalog: CatalogResponse,
    ecosystem_positions: BTreeMap<String, usize>,
    track_positions: BTreeMap<(String, String), usize>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("registry must contain at least one ecosystem")]
    Empty,
    #[error("ecosystem id is empty")]
    EmptyEcosystemId,
    #[error("duplicate ecosystem id: {0}")]
    DuplicateEcosystem(String),
    #[error("track id is empty for ecosystem: {0}")]
    EmptyTrackId(String),
    #[error("duplicate track id {track_id} for ecosystem {ecosystem_id}")]
    DuplicateTrack {
        ecosystem_id: String,
        track_id: String,
    },
    #[error("unknown ecosystem: {0}")]
    UnknownEcosystem(String),
    #[error("ecosystem is disabled: {0}")]
    EcosystemDisabled(String),
    #[error("unknown track {track_id} for ecosystem {ecosystem_id}")]
    UnknownTrack {
        ecosystem_id: String,
        track_id: String,
    },
    #[error("track is disabled: {track_id}")]
    TrackDisabled { track_id: String },
}

impl EcosystemRegistry {
    pub fn new(ecosystems: Vec<EcosystemRegistration>) -> Result<Self, RegistryError> {
        if ecosystems.is_empty() {
            return Err(RegistryError::Empty);
        }

        let mut ecosystem_positions = BTreeMap::new();
        let mut track_positions = BTreeMap::new();

        for (ecosystem_index, ecosystem) in ecosystems.iter().enumerate() {
            if ecosystem.ecosystem_id.trim().is_empty() {
                return Err(RegistryError::EmptyEcosystemId);
            }
            if ecosystem_positions
                .insert(ecosystem.ecosystem_id.clone(), ecosystem_index)
                .is_some()
            {
                return Err(RegistryError::DuplicateEcosystem(
                    ecosystem.ecosystem_id.clone(),
                ));
            }

            for (track_index, track) in ecosystem.tracks.iter().enumerate() {
                if track.track_id.trim().is_empty() {
                    return Err(RegistryError::EmptyTrackId(ecosystem.ecosystem_id.clone()));
                }
                let key = (ecosystem.ecosystem_id.clone(), track.track_id.clone());
                if track_positions.insert(key, track_index).is_some() {
                    return Err(RegistryError::DuplicateTrack {
                        ecosystem_id: ecosystem.ecosystem_id.clone(),
                        track_id: track.track_id.clone(),
                    });
                }
            }
        }

        Ok(Self {
            catalog: CatalogResponse {
                schema_version: SCHEMA_VERSION,
                ecosystems,
            },
            ecosystem_positions,
            track_positions,
        })
    }

    pub fn built_in() -> Result<Self, RegistryError> {
        Self::new(vec![
            generic_ecosystem(
                BASICS_ECOSYSTEM_ID,
                "Web3 + Blockchain Basics",
                "Beginner Web3 foundations with wallets, transactions, networks, explorers, and safety checks.",
                EcosystemConfiguration::Basics(generic_registration(
                    "basics-source-pack-1.0.0",
                    [
                        "Ethereum developer docs",
                        "MDN Web Docs",
                        "Bitcoin developer reference",
                    ],
                    [
                        "wallet mental models",
                        "transaction lifecycle",
                        "network and confirmation safety",
                    ],
                )),
            ),
            generic_ecosystem(
                CKB_ECOSYSTEM_ID,
                "CKB",
                "Cell-model learning paths for scripts, witnesses, transactions, and verifier boundaries.",
                EcosystemConfiguration::Ckb(generic_registration(
                    "ckb-source-pack-1.0.0",
                    ["CKB Docs", "Nervos RFCs"],
                    [
                        "cell model",
                        "scripts",
                        "witnesses",
                        "transaction proof boundaries",
                    ],
                )),
            ),
            generic_ecosystem(
                FIBER_ECOSYSTEM_ID,
                "Fiber",
                "Payment-channel learning paths for invoices, PTLCs, routing, receipts, and replay defense.",
                EcosystemConfiguration::Fiber(generic_registration(
                    "fiber-source-pack-1.0.0",
                    ["Fiber Network repository", "CKB Docs"],
                    [
                        "channels",
                        "invoices",
                        "PTLC proof boundaries",
                        "receipt replay defense",
                    ],
                )),
            ),
            zcash_ecosystem(),
            generic_ecosystem(
                STACKS_ECOSYSTEM_ID,
                "Stacks",
                "Bitcoin-secured app learning paths for Clarity, sBTC, BNS, wallets, and safe authorization flows.",
                EcosystemConfiguration::Stacks(generic_registration(
                    "stacks-source-pack-1.0.0",
                    [
                        "Stacks Docs",
                        "Clarity documentation",
                        "sBTC documentation",
                        "BNS documentation",
                    ],
                    [
                        "Stacks and Bitcoin",
                        "Clarity",
                        "sBTC",
                        "BNS",
                        "wallet authorization",
                    ],
                )),
            ),
            generic_ecosystem(
                TON_STONFI_ECOSYSTEM_ID,
                "TON / STON.fi",
                "STON.fi integration labs for TON builders implementing swaps, widget flows, jetton checks, slippage safety, and transaction-state handling.",
                EcosystemConfiguration::TonStonfi(generic_registration(
                    "ton-stonfi-source-pack-1.0.0",
                    [
                        "STON.fi DEX overview",
                        "STON.fi DEX SDK documentation",
                        "STON.fi DEX smart contracts documentation",
                        "STON.fi Omniston widget documentation",
                        "STON.fi Omniston SDK documentation",
                        "STON.fi REST API documentation",
                        "TON Connect documentation",
                        "TON Connect UI documentation",
                        "TON token standards documentation",
                        "TON jetton processing documentation",
                        "TON jetton interface documentation",
                        "TON jetton architecture documentation",
                    ],
                    [
                        "STON.fi swap quotes",
                        "STON.fi router and pool boundaries",
                        "Omniston widget integration",
                        "Omniston SDK integration",
                        "TON Connect wallet boundaries",
                        "jetton master and wallet verification",
                        "slippage and stale quote denial tests",
                        "referral-fee disclosure",
                        "transaction-state evidence",
                    ],
                )),
            ),
            golem_ecosystem(),
            aibtc_ecosystem(),
        ])
    }

    pub fn zcash_only() -> Result<Self, RegistryError> {
        Self::new(vec![zcash_ecosystem()])
    }

    pub fn catalog(&self) -> CatalogResponse {
        self.catalog.clone()
    }

    pub fn resolve_track(
        &self,
        ecosystem_id: &str,
        track_id: &str,
    ) -> Result<TrackRegistration, RegistryError> {
        let track = self.registered_track(ecosystem_id, track_id)?;
        if !track.enabled {
            return Err(RegistryError::TrackDisabled {
                track_id: track_id.to_string(),
            });
        }
        Ok(track)
    }

    pub fn registered_track(
        &self,
        ecosystem_id: &str,
        track_id: &str,
    ) -> Result<TrackRegistration, RegistryError> {
        let ecosystem_index = self
            .ecosystem_positions
            .get(ecosystem_id)
            .copied()
            .ok_or_else(|| RegistryError::UnknownEcosystem(ecosystem_id.to_string()))?;
        let ecosystem = &self.catalog.ecosystems[ecosystem_index];

        if !ecosystem.enabled {
            return Err(RegistryError::EcosystemDisabled(ecosystem_id.to_string()));
        }

        let track_index = self
            .track_positions
            .get(&(ecosystem_id.to_string(), track_id.to_string()))
            .copied()
            .ok_or_else(|| RegistryError::UnknownTrack {
                ecosystem_id: ecosystem_id.to_string(),
                track_id: track_id.to_string(),
            })?;
        let track = &ecosystem.tracks[track_index];
        Ok(track.clone())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RecordNamespace {
    pub schema_version: u16,
    pub ecosystem_id: String,
    pub track_id: String,
    pub track_version: String,
    pub content_version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LearningSessionStatus {
    Active,
    Completed,
    Archived,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct LearningSessionV3 {
    pub session_id: String,
    pub user_id: String,
    pub namespace: RecordNamespace,
    pub current_lesson_id: Option<String>,
    pub status: LearningSessionStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct VerifierReference {
    pub verifier_id: String,
    pub verifier_version: String,
    pub fixture_digest: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ScenarioV3 {
    pub scenario_id: String,
    pub namespace: RecordNamespace,
    pub title: String,
    pub verifier: VerifierReference,
    pub source_references: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubmissionStatus {
    Queued,
    Running,
    Passed,
    Failed,
    Error,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct EvidenceReference {
    pub runner_version: String,
    pub source_digest: String,
    pub result_digest: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SubmissionV3 {
    pub submission_id: String,
    pub user_id: String,
    pub scenario_id: String,
    pub namespace: RecordNamespace,
    pub status: SubmissionStatus,
    pub evidence: Option<EvidenceReference>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CompletionReceiptV3 {
    pub receipt_id: String,
    pub user_id: String,
    pub namespace: RecordNamespace,
    pub submission_id: String,
    pub evidence: EvidenceReference,
    pub completed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountExport {
    pub schema_version: u16,
    pub generated_at: DateTime<Utc>,
    pub persistence_enabled: bool,
    pub profile: Option<Document>,
    pub learning_sessions: Vec<Document>,
    pub submissions: Vec<Document>,
    pub completion_receipts: Vec<Document>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountDeletion {
    pub persistence_enabled: bool,
    pub profiles_deleted: u64,
    pub learning_sessions_deleted: u64,
    pub submissions_deleted: u64,
    pub completion_receipts_deleted: u64,
}

#[derive(Clone)]
pub struct PlatformStore {
    uri: Option<String>,
    database_name: String,
    client: Arc<OnceCell<MongoClient>>,
}

impl PlatformStore {
    pub fn new(uri: Option<String>, database_name: String) -> Self {
        Self {
            uri,
            database_name,
            client: Arc::new(OnceCell::new()),
        }
    }

    pub async fn ensure_indexes(&self) -> Result<(), mongodb::error::Error> {
        let Some(database) = self.database().await? else {
            return Ok(());
        };

        database
            .collection::<Document>("users")
            .create_index(unique_index(
                doc! { "provider": 1, "provider_subject": 1 },
                "identity_provider_subject_unique",
            ))
            .await?;

        database
            .collection::<Document>("learning_sessions")
            .create_indexes([
                unique_index(doc! { "session_id": 1 }, "session_id_unique"),
                named_index(
                    doc! {
                        "user_id": 1,
                        "namespace.ecosystem_id": 1,
                        "namespace.track_id": 1,
                        "updated_at": -1,
                    },
                    "learner_track_recency",
                ),
            ])
            .await?;

        database
            .collection::<Document>("scenarios")
            .create_index(unique_index(
                doc! {
                    "scenario_id": 1,
                    "namespace.track_version": 1,
                    "namespace.content_version": 1,
                },
                "scenario_version_unique",
            ))
            .await?;

        database
            .collection::<Document>("submissions")
            .create_indexes([
                unique_index(doc! { "submission_id": 1 }, "submission_id_unique"),
                named_index(
                    doc! {
                        "user_id": 1,
                        "namespace.ecosystem_id": 1,
                        "namespace.track_id": 1,
                        "created_at": -1,
                    },
                    "learner_submission_recency",
                ),
            ])
            .await?;

        database
            .collection::<Document>("completion_receipts")
            .create_indexes([
                unique_index(doc! { "receipt_id": 1 }, "receipt_id_unique"),
                unique_index(
                    doc! {
                        "user_id": 1,
                        "namespace.ecosystem_id": 1,
                        "namespace.track_id": 1,
                        "namespace.track_version": 1,
                    },
                    "learner_track_completion_unique",
                ),
            ])
            .await?;

        Ok(())
    }

    pub async fn upsert_identity(
        &self,
        user_id: &str,
        provider: &str,
        provider_subject: &str,
        email: Option<&str>,
        name: Option<&str>,
    ) -> Result<bool, mongodb::error::Error> {
        let Some(database) = self.database().await? else {
            return Ok(false);
        };
        let now = BsonDateTime::now();
        let mut current_fields = doc! { "last_seen_at": now };
        if let Some(email) = email {
            current_fields.insert("email", email);
        }
        if let Some(name) = name {
            current_fields.insert("name", name);
        }

        database
            .collection::<Document>("users")
            .update_one(
                doc! { "provider": provider, "provider_subject": provider_subject },
                doc! {
                    "$set": current_fields,
                    "$setOnInsert": {
                        "_id": user_id,
                        "user_id": user_id,
                        "provider": provider,
                        "provider_subject": provider_subject,
                        "created_at": now,
                    },
                },
            )
            .upsert(true)
            .await?;

        Ok(true)
    }

    pub async fn export_account(
        &self,
        user_id: &str,
    ) -> Result<AccountExport, mongodb::error::Error> {
        let Some(database) = self.database().await? else {
            return Ok(AccountExport {
                schema_version: SCHEMA_VERSION,
                generated_at: Utc::now(),
                persistence_enabled: false,
                profile: None,
                learning_sessions: Vec::new(),
                submissions: Vec::new(),
                completion_receipts: Vec::new(),
            });
        };

        let profile = database
            .collection::<Document>("users")
            .find_one(doc! { "_id": user_id })
            .await?;
        let learning_sessions = database
            .collection::<Document>("learning_sessions")
            .find(doc! { "user_id": user_id })
            .await?
            .try_collect()
            .await?;
        let submissions = database
            .collection::<Document>("submissions")
            .find(doc! { "user_id": user_id })
            .await?
            .try_collect()
            .await?;
        let completion_receipts = database
            .collection::<Document>("completion_receipts")
            .find(doc! { "user_id": user_id })
            .await?
            .try_collect()
            .await?;

        Ok(AccountExport {
            schema_version: SCHEMA_VERSION,
            generated_at: Utc::now(),
            persistence_enabled: true,
            profile,
            learning_sessions,
            submissions,
            completion_receipts,
        })
    }

    pub async fn delete_account(
        &self,
        user_id: &str,
    ) -> Result<AccountDeletion, mongodb::error::Error> {
        let Some(database) = self.database().await? else {
            return Ok(AccountDeletion {
                persistence_enabled: false,
                profiles_deleted: 0,
                learning_sessions_deleted: 0,
                submissions_deleted: 0,
                completion_receipts_deleted: 0,
            });
        };

        let learning_sessions_deleted = database
            .collection::<Document>("learning_sessions")
            .delete_many(doc! { "user_id": user_id })
            .await?
            .deleted_count;
        let submissions_deleted = database
            .collection::<Document>("submissions")
            .delete_many(doc! { "user_id": user_id })
            .await?
            .deleted_count;
        let completion_receipts_deleted = database
            .collection::<Document>("completion_receipts")
            .delete_many(doc! { "user_id": user_id })
            .await?
            .deleted_count;
        let profiles_deleted = database
            .collection::<Document>("users")
            .delete_one(doc! { "_id": user_id })
            .await?
            .deleted_count;

        Ok(AccountDeletion {
            persistence_enabled: true,
            profiles_deleted,
            learning_sessions_deleted,
            submissions_deleted,
            completion_receipts_deleted,
        })
    }

    async fn database(&self) -> Result<Option<Database>, mongodb::error::Error> {
        let Some(uri) = self.uri.clone() else {
            return Ok(None);
        };
        let database_name = self.database_name.clone();
        let client = self
            .client
            .get_or_try_init(|| async move {
                let mut options = ClientOptions::parse(uri).await?;
                options.app_name = Some("vibequestlearn-v3".to_string());
                options.server_selection_timeout = Some(Duration::from_secs(3));
                options.connect_timeout = Some(Duration::from_secs(3));
                MongoClient::with_options(options)
            })
            .await?;

        Ok(Some(client.database(&database_name)))
    }
}

fn named_index(keys: Document, name: &str) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(IndexOptions::builder().name(name.to_string()).build())
        .build()
}

fn unique_index(keys: Document, name: &str) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_string())
                .unique(true)
                .build(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn built_in_registry_exposes_multi_ecosystem_tracks() {
        let registry = EcosystemRegistry::built_in().expect("valid static registry");
        let catalog = registry.catalog();

        assert_eq!(catalog.schema_version, SCHEMA_VERSION);
        assert_eq!(catalog.ecosystems.len(), 8);
        assert!(
            catalog
                .ecosystems
                .iter()
                .any(|ecosystem| ecosystem.ecosystem_id == BASICS_ECOSYSTEM_ID)
        );
        assert!(
            catalog
                .ecosystems
                .iter()
                .any(|ecosystem| ecosystem.ecosystem_id == CKB_ECOSYSTEM_ID)
        );
        assert!(
            catalog
                .ecosystems
                .iter()
                .any(|ecosystem| ecosystem.ecosystem_id == FIBER_ECOSYSTEM_ID)
        );
        assert!(
            catalog
                .ecosystems
                .iter()
                .any(|ecosystem| ecosystem.ecosystem_id == STACKS_ECOSYSTEM_ID)
        );
        assert!(
            catalog
                .ecosystems
                .iter()
                .any(|ecosystem| ecosystem.ecosystem_id == TON_STONFI_ECOSYSTEM_ID)
        );
        let golem = catalog
            .ecosystems
            .iter()
            .find(|ecosystem| ecosystem.ecosystem_id == GOLEM_ECOSYSTEM_ID)
            .expect("Golem compute lab is registered");
        assert!(golem.summary.contains("Decentralized compute"));
        assert!(matches!(
            golem.configuration,
            EcosystemConfiguration::Golem(_)
        ));
        assert_eq!(golem.tracks[0].track_id, "golem-compute-lab");
        assert!(golem.tracks[0].summary.contains("source-grounded compute"));
        let proof = golem.tracks[0]
            .review_proof
            .as_ref()
            .expect("Golem track exposes grant-review proof metadata");
        assert_eq!(proof.sample_modules.len(), 5);
        assert!(proof.sample_topic.contains("requestor/provider"));
        assert!(
            proof
                .required_artifacts
                .iter()
                .any(|item| item.contains("validation artifact"))
        );
        assert!(
            proof
                .reviewer_demo_steps
                .iter()
                .any(|step| step.contains("module 1"))
        );
        assert!(
            proof
                .source_ids
                .iter()
                .any(|source_id| source_id == "golem-js-sdk")
        );
        assert!(
            proof
                .source_ids
                .iter()
                .any(|source_id| source_id == "golem-ray-limitations")
        );
        let aibtc = catalog
            .ecosystems
            .iter()
            .find(|ecosystem| ecosystem.ecosystem_id == AIBTC_ECOSYSTEM_ID)
            .expect("AIBTC agent lab is registered");
        assert!(matches!(
            aibtc.configuration,
            EcosystemConfiguration::Aibtc(_)
        ));
        assert_eq!(aibtc.tracks[0].track_id, "aibtc-agent-lab");
        let aibtc_proof = aibtc.tracks[0]
            .review_proof
            .as_ref()
            .expect("AIBTC track exposes review proof metadata");
        assert!(aibtc_proof.sample_topic.contains("signed agent actions"));
        assert_eq!(aibtc_proof.sample_modules.len(), 5);
        assert!(
            aibtc_proof
                .source_ids
                .iter()
                .any(|source_id| source_id == "aibtc-bounties-docs")
        );

        let zcash = catalog
            .ecosystems
            .iter()
            .find(|ecosystem| ecosystem.ecosystem_id == ZCASH_ECOSYSTEM_ID)
            .expect("Zcash remains registered");
        assert_eq!(zcash.tracks.len(), 1);
        assert!(!zcash.tracks[0].enabled);
        assert_eq!(
            zcash.tracks[0].source_manifest_version,
            crate::zcash::SOURCE_MANIFEST_VERSION
        );
    }

    #[test]
    fn registry_fails_closed_for_unknown_and_disabled_tracks() {
        let registry = EcosystemRegistry::built_in().expect("valid static registry");

        assert_eq!(
            registry.resolve_track("unknown", SHIELDED_PAYMENTS_TRACK_ID),
            Err(RegistryError::UnknownEcosystem("unknown".to_string()))
        );
        assert_eq!(
            registry.resolve_track(ZCASH_ECOSYSTEM_ID, "unknown"),
            Err(RegistryError::UnknownTrack {
                ecosystem_id: ZCASH_ECOSYSTEM_ID.to_string(),
                track_id: "unknown".to_string(),
            })
        );
        assert_eq!(
            registry.resolve_track(ZCASH_ECOSYSTEM_ID, SHIELDED_PAYMENTS_TRACK_ID),
            Err(RegistryError::TrackDisabled {
                track_id: SHIELDED_PAYMENTS_TRACK_ID.to_string(),
            })
        );
    }

    #[test]
    fn v3_records_serialize_without_wallet_or_reward_fields() {
        let now = Utc::now();
        let session = LearningSessionV3 {
            session_id: "session_01".to_string(),
            user_id: "user_01".to_string(),
            namespace: RecordNamespace {
                schema_version: SCHEMA_VERSION,
                ecosystem_id: ZCASH_ECOSYSTEM_ID.to_string(),
                track_id: SHIELDED_PAYMENTS_TRACK_ID.to_string(),
                track_version: "1.0.0".to_string(),
                content_version: "2026-07-21.1".to_string(),
            },
            current_lesson_id: None,
            status: LearningSessionStatus::Active,
            created_at: now,
            updated_at: now,
        };
        let value = serde_json::to_value(session).expect("serializable record");

        assert_eq!(value["user_id"], json!("user_01"));
        assert_eq!(value["namespace"]["schema_version"], json!(SCHEMA_VERSION));
        assert!(value.get("wallet").is_none());
        assert!(value.get("reward").is_none());
        assert!(value.get("fiber_invoice").is_none());
    }
}
