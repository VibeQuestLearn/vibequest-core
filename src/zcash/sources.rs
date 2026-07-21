use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub const SOURCE_MANIFEST_VERSION: &str = "zcash-sources-2026-07-21.2";
pub const LIBRUSTZCASH_REVISION: &str = "d47691c6b620e9c1fa3574a5a63deb4da544da2e";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SourceManifest {
    pub manifest_version: String,
    pub retrieved_at: String,
    pub supported_scope: String,
    pub sources: Vec<SourceReference>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SourceReference {
    pub source_id: String,
    pub title: String,
    pub version: String,
    pub url: String,
    pub revision: String,
    pub license: String,
    pub notes: String,
}

pub fn source_manifest() -> &'static SourceManifest {
    static MANIFEST: OnceLock<SourceManifest> = OnceLock::new();

    MANIFEST.get_or_init(|| {
        let manifest: SourceManifest =
            serde_json::from_str(include_str!("../../fixtures/zcash/v1/source-manifest.json"))
                .expect("the reviewed Zcash source manifest must be valid JSON");
        assert_eq!(manifest.manifest_version, SOURCE_MANIFEST_VERSION);
        manifest
    })
}
