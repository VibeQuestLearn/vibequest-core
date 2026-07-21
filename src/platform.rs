use chrono::{DateTime, Utc};
use mongodb::{
    Client as MongoClient, Database, IndexModel,
    bson::{Document, doc},
    options::{ClientOptions, IndexOptions},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use thiserror::Error;
use tokio::sync::OnceCell;

pub const SCHEMA_VERSION: u16 = 3;
pub const DEFAULT_V3_DATABASE: &str = "vibequestlearn_v3";
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
    Zcash(ZcashRegistration),
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
    pub lesson_count: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrackStatus {
    Building,
    Enabled,
    Retired,
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

    pub fn zcash_only() -> Result<Self, RegistryError> {
        Self::new(vec![EcosystemRegistration {
            ecosystem_id: ZCASH_ECOSYSTEM_ID.to_string(),
            name: "Zcash".to_string(),
            summary: "Shielded payment integration labs for web and backend developers."
                .to_string(),
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
                track_version: "0.1.0".to_string(),
                content_version: "2026-07-21".to_string(),
                lesson_count: 5,
            }],
        }])
    }

    pub fn catalog(&self) -> CatalogResponse {
        self.catalog.clone()
    }

    pub fn resolve_track(
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

        if !track.enabled {
            return Err(RegistryError::TrackDisabled {
                track_id: track_id.to_string(),
            });
        }

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
    fn zcash_registry_exposes_only_the_building_track() {
        let registry = EcosystemRegistry::zcash_only().expect("valid static registry");
        let catalog = registry.catalog();

        assert_eq!(catalog.schema_version, SCHEMA_VERSION);
        assert_eq!(catalog.ecosystems.len(), 1);
        assert_eq!(catalog.ecosystems[0].ecosystem_id, ZCASH_ECOSYSTEM_ID);
        assert_eq!(catalog.ecosystems[0].tracks.len(), 1);
        assert!(!catalog.ecosystems[0].tracks[0].enabled);
    }

    #[test]
    fn registry_fails_closed_for_unknown_and_disabled_tracks() {
        let registry = EcosystemRegistry::zcash_only().expect("valid static registry");

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
                track_version: "0.1.0".to_string(),
                content_version: "2026-07-21".to_string(),
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
