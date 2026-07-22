use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use thiserror::Error;

use super::{
    EDUCATIONAL_PUBLIC_TESTS_SOURCE, HARNESS_SOURCE, HIDDEN_TEST_SPEC_SOURCE, HIDDEN_TESTS_SOURCE,
    PUBLIC_TESTS_SOURCE, SOLUTION_SOURCE, STARTER_SOURCE,
};

pub const RUNNER_MANIFEST_VERSION: &str = "shielded-checkout-runner-1.0.0";
pub const RUNNER_VERSION: &str = "vibequest-runner-1.0.0";
pub const RUNNER_PROTOCOL_VERSION: &str = "vibequest-runner-protocol-1.0.0";
pub const APPROVED_NODE_IMAGE: &str =
    "node:22-alpine@sha256:16e22a550f3863206a3f701448c45f7912c6896a62de43add43bb9c86130c3e2";

#[derive(Clone, Debug, Deserialize)]
pub struct RunnerManifest {
    pub runner_manifest_version: String,
    pub runner_version: String,
    pub protocol_version: String,
    pub scenario_manifest_version: String,
    pub scenario_id: String,
    pub language: String,
    pub production_enabled: bool,
    pub toolchain: RunnerToolchain,
    pub source: RunnerSource,
    pub tests: RunnerTests,
    pub scenario_package_sha256: String,
    pub limits: RunnerLimits,
    pub isolation: RunnerIsolation,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunnerToolchain {
    pub image: String,
    pub runtime: String,
    pub runtime_version: String,
    pub dependency_allowlist: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunnerSource {
    pub path: String,
    pub max_bytes: usize,
    pub starter_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunnerTests {
    pub harness_path: String,
    pub harness_sha256: String,
    pub public_path: String,
    pub public_sha256: String,
    pub hidden_path: String,
    pub hidden_sha256: String,
    pub bundle_sha256: String,
    pub public_case_count: usize,
    pub hidden_case_count: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunnerLimits {
    pub queue_capacity: usize,
    pub max_concurrency: usize,
    pub max_retries: u8,
    pub wall_clock_ms: u64,
    pub vm_module_ms: u64,
    pub cpu_cores: f64,
    pub memory_bytes: u64,
    pub pids: u32,
    pub max_output_bytes: usize,
    pub max_public_output_bytes: usize,
    pub tmpfs_bytes: u64,
    pub open_files: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunnerIsolation {
    pub backend: String,
    pub network: String,
    pub root_filesystem: String,
    pub job_mount: String,
    pub tmpfs: String,
    pub uid: u32,
    pub gid: u32,
    pub capabilities: String,
    pub no_new_privileges: bool,
    pub ipc: String,
    pub docker_socket_mounted: bool,
    pub host_environment_forwarded: bool,
    pub node_permission_model: bool,
    pub learner_vm: LearnerVmPolicy,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LearnerVmPolicy {
    pub process: bool,
    pub filesystem: bool,
    pub network: bool,
    pub console: bool,
    pub static_imports: bool,
    pub dynamic_imports: bool,
    pub string_code_generation: bool,
    pub wasm_code_generation: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("runner manifest is invalid: {0}")]
pub struct RunnerManifestError(String);

pub fn runner_manifest() -> &'static RunnerManifest {
    static MANIFEST: OnceLock<RunnerManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(include_str!("../../fixtures/zcash/v1/runner-manifest.json"))
            .expect("the reviewed runner manifest must be valid JSON")
    })
}

pub fn validate_runner_manifest() -> Result<(), RunnerManifestError> {
    let manifest = runner_manifest();
    ensure(
        manifest.runner_manifest_version == RUNNER_MANIFEST_VERSION
            && manifest.runner_version == RUNNER_VERSION
            && manifest.protocol_version == RUNNER_PROTOCOL_VERSION
            && manifest.scenario_manifest_version == crate::curriculum::SCENARIO_MANIFEST_VERSION,
        "version binding mismatch",
    )?;
    ensure(
        manifest.scenario_id == "shielded-checkout"
            && manifest.language == "typescript"
            && !manifest.production_enabled,
        "scope or production gate mismatch",
    )?;
    ensure(
        manifest.toolchain.image == APPROVED_NODE_IMAGE
            && manifest.toolchain.runtime == "node"
            && manifest.toolchain.runtime_version == "22.23.1"
            && manifest.toolchain.dependency_allowlist.is_empty(),
        "toolchain is not the approved dependency-free image",
    )?;
    ensure(
        manifest.source.path == "starter/src/checkout.ts"
            && manifest.source.max_bytes == 65_536
            && manifest.source.starter_sha256 == sha256_hex(STARTER_SOURCE.as_bytes()),
        "starter source binding mismatch",
    )?;
    ensure(
        manifest.tests.harness_path == "runner/harness.mjs"
            && manifest.tests.public_path == "runner/public-tests.mjs"
            && manifest.tests.hidden_path == "runner/hidden-tests.mjs"
            && manifest.tests.harness_sha256 == sha256_hex(HARNESS_SOURCE.as_bytes())
            && manifest.tests.public_sha256 == sha256_hex(PUBLIC_TESTS_SOURCE.as_bytes())
            && manifest.tests.hidden_sha256 == sha256_hex(HIDDEN_TESTS_SOURCE.as_bytes())
            && manifest.tests.public_case_count == 15
            && manifest.tests.hidden_case_count == 5,
        "test file binding mismatch",
    )?;
    let test_bundle = hash_named_files(&[
        ("runner/harness.mjs", HARNESS_SOURCE),
        ("runner/public-tests.mjs", PUBLIC_TESTS_SOURCE),
        ("runner/hidden-tests.mjs", HIDDEN_TESTS_SOURCE),
    ]);
    ensure(
        manifest.tests.bundle_sha256 == test_bundle,
        "test bundle digest mismatch",
    )?;
    let scenario_package = hash_named_files(&[
        ("starter/src/checkout.ts", STARTER_SOURCE),
        ("solution/src/checkout.ts", SOLUTION_SOURCE),
        ("tests/public.spec.ts", EDUCATIONAL_PUBLIC_TESTS_SOURCE),
        ("specs/hidden-tests.json", HIDDEN_TEST_SPEC_SOURCE),
        ("runner/harness.mjs", HARNESS_SOURCE),
        ("runner/public-tests.mjs", PUBLIC_TESTS_SOURCE),
        ("runner/hidden-tests.mjs", HIDDEN_TESTS_SOURCE),
    ]);
    ensure(
        manifest.scenario_package_sha256 == scenario_package,
        "scenario package digest mismatch",
    )?;
    ensure(
        manifest.limits.queue_capacity == 32
            && manifest.limits.max_concurrency == 1
            && manifest.limits.max_retries == 1
            && manifest.limits.wall_clock_ms == 8_000
            && manifest.limits.vm_module_ms == 1_000
            && manifest.limits.cpu_cores == 0.5
            && manifest.limits.memory_bytes == 128 * 1024 * 1024
            && manifest.limits.pids == 32
            && manifest.limits.max_output_bytes == 65_536
            && manifest.limits.max_public_output_bytes == 8_192
            && manifest.limits.tmpfs_bytes == 16 * 1024 * 1024
            && manifest.limits.open_files == 64,
        "resource limits changed without review",
    )?;
    let isolation = &manifest.isolation;
    ensure(
        isolation.backend == "docker"
            && isolation.network == "none"
            && isolation.root_filesystem == "read-only"
            && isolation.job_mount == "read-only"
            && isolation.tmpfs == "noexec,nosuid,nodev"
            && isolation.uid == 65_534
            && isolation.gid == 65_534
            && isolation.capabilities == "drop-all"
            && isolation.no_new_privileges
            && isolation.ipc == "none"
            && !isolation.docker_socket_mounted
            && !isolation.host_environment_forwarded
            && isolation.node_permission_model,
        "container isolation policy changed without review",
    )?;
    let learner = &isolation.learner_vm;
    ensure(
        !learner.process
            && !learner.filesystem
            && !learner.network
            && !learner.console
            && !learner.static_imports
            && !learner.dynamic_imports
            && !learner.string_code_generation
            && !learner.wasm_code_generation,
        "learner VM exposes an unreviewed capability",
    )
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn hash_named_files(files: &[(&str, &str)]) -> String {
    let mut digest = Sha256::new();
    for (path, contents) in files {
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(contents.as_bytes());
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())
}

fn ensure(condition: bool, message: &str) -> Result<(), RunnerManifestError> {
    condition
        .then_some(())
        .ok_or_else(|| RunnerManifestError(message.to_string()))
}
