mod docker;
mod manifest;
mod protocol;
mod service;
mod signature;
#[cfg(test)]
mod tests;

pub use docker::{DockerSandboxExecutor, SandboxExecutor};
pub use protocol::*;
pub use service::{RunnerService, RunnerServiceError};
pub use signature::{RunnerResultSigner, RunnerResultVerifier, SignatureConfigError};

pub use manifest::{
    APPROVED_NODE_IMAGE, RUNNER_MANIFEST_VERSION, RUNNER_PROTOCOL_VERSION, RUNNER_VERSION,
    RunnerManifest, hash_named_files, runner_manifest, sha256_hex, validate_runner_manifest,
};

pub const HARNESS_SOURCE: &str =
    include_str!("../../scenarios/zcash/shielded-checkout/v1/runner/harness.mjs");
pub const PUBLIC_TESTS_SOURCE: &str =
    include_str!("../../scenarios/zcash/shielded-checkout/v1/runner/public-tests.mjs");
pub const HIDDEN_TESTS_SOURCE: &str =
    include_str!("../../scenarios/zcash/shielded-checkout/v1/runner/hidden-tests.mjs");
pub const STARTER_SOURCE: &str =
    include_str!("../../scenarios/zcash/shielded-checkout/v1/starter/src/checkout.ts");
pub const SOLUTION_SOURCE: &str =
    include_str!("../../scenarios/zcash/shielded-checkout/v1/solution/src/checkout.ts");
pub const EDUCATIONAL_PUBLIC_TESTS_SOURCE: &str =
    include_str!("../../scenarios/zcash/shielded-checkout/v1/tests/public.spec.ts");
pub const HIDDEN_TEST_SPEC_SOURCE: &str =
    include_str!("../../scenarios/zcash/shielded-checkout/v1/specs/hidden-tests.json");
