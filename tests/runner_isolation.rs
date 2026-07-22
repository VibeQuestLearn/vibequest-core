use base64::Engine as _;
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use std::{io::Write as _, process::Stdio};
use tokio::sync::watch;
use vibequest_core::runner::{
    AuthenticatedRunnerResult, DockerSandboxExecutor, RunnerClassification, RunnerJob,
    RunnerResultVerifier, SOLUTION_SOURCE, SandboxExecutor,
};

const TIMEOUT_SOURCE: &str = include_str!("../fixtures/zcash/v1/runner-adversarial/timeout.ts");
const OUTPUT_FLOOD_SOURCE: &str =
    include_str!("../fixtures/zcash/v1/runner-adversarial/output-flood.ts");
const PROCESS_SOURCE: &str =
    include_str!("../fixtures/zcash/v1/runner-adversarial/process-access.ts");
const FILESYSTEM_SOURCE: &str =
    include_str!("../fixtures/zcash/v1/runner-adversarial/filesystem-import.ts");
const NETWORK_SOURCE: &str =
    include_str!("../fixtures/zcash/v1/runner-adversarial/network-access.ts");
const DYNAMIC_IMPORT_SOURCE: &str =
    include_str!("../fixtures/zcash/v1/runner-adversarial/dynamic-import.ts");
const HOST_GLOBAL_CHECK: &str =
    include_str!("../fixtures/zcash/v1/runner-adversarial/host-global-check.ts");

fn reviewed_job(suffix: &str, source: &str) -> RunnerJob {
    RunnerJob::reviewed(
        format!("job_{suffix}"),
        format!("sub_{suffix}"),
        "usr_0123456789abcdefghijklmnopqrstuv".to_string(),
        source.to_string(),
    )
    .expect("reviewed runner job")
}

async fn execute(suffix: &str, source: &str) -> vibequest_core::runner::RunnerEvidence {
    let (_cancel, cancellation) = watch::channel(false);
    DockerSandboxExecutor::approved()
        .execute(reviewed_job(suffix, source), 1, cancellation)
        .await
}

#[tokio::test]
#[ignore = "requires the pinned local Docker image and reviewed host controls"]
async fn docker_runner_enforces_the_reviewed_adversarial_boundary() {
    let image = vibequest_core::runner::APPROVED_NODE_IMAGE;
    let image_status = std::process::Command::new("/usr/bin/docker")
        .args(["image", "inspect", image])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("Docker must be installed for this acceptance test");
    assert!(
        image_status.success(),
        "the pinned runner image must exist locally"
    );

    let passing = execute("solution", SOLUTION_SOURCE).await;
    assert_eq!(passing.classification, RunnerClassification::Passed);
    assert_eq!(passing.public_cases.len(), 15);
    assert_eq!(passing.hidden_summary.passed, 5);
    assert_eq!(passing.hidden_summary.failed, 0);

    let key_document =
        Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("worker signing key");
    let key_pair = Ed25519KeyPair::from_pkcs8(key_document.as_ref()).expect("worker key pair");
    let signing_config = format!(
        "acceptance:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key_document.as_ref())
    );
    let verifying_config = format!(
        "acceptance:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key_pair.public_key().as_ref())
    );
    let worker_job = reviewed_job("worker_binary", SOLUTION_SOURCE);
    let mut worker = std::process::Command::new(env!("CARGO_BIN_EXE_vibequest-runner-worker"))
        .env_clear()
        .env("RUNNER_REVIEW_MODE", "1")
        .env("RUNNER_RESULT_SIGNING_KEY", signing_config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("standalone worker");
    worker
        .stdin
        .take()
        .expect("worker stdin")
        .write_all(&serde_json::to_vec(&worker_job).expect("worker job JSON"))
        .expect("write worker job");
    let worker_output = worker.wait_with_output().expect("worker output");
    assert!(
        worker_output.status.success(),
        "worker failed: {}",
        String::from_utf8_lossy(&worker_output.stderr)
    );
    let authenticated: AuthenticatedRunnerResult =
        serde_json::from_slice(&worker_output.stdout).expect("authenticated result JSON");
    RunnerResultVerifier::from_config(&verifying_config)
        .expect("worker public key")
        .verify(&authenticated)
        .expect("worker result signature");
    assert_eq!(authenticated.evidence.binding, worker_job.binding());
    assert_eq!(
        authenticated.evidence.classification,
        RunnerClassification::Passed
    );

    let host_safe_source = format!("{HOST_GLOBAL_CHECK}\n{SOLUTION_SOURCE}");
    let host_safe = execute("host_globals", &host_safe_source).await;
    assert_eq!(host_safe.classification, RunnerClassification::Passed);

    let static_import = execute("filesystem", FILESYSTEM_SOURCE).await;
    assert_eq!(
        static_import.classification,
        RunnerClassification::CompileError
    );
    assert_eq!(static_import.diagnostic_code, "import-denied");

    let dynamic_import = execute("dynamic_import", DYNAMIC_IMPORT_SOURCE).await;
    assert_eq!(
        dynamic_import.classification,
        RunnerClassification::CompileError
    );
    assert_eq!(dynamic_import.diagnostic_code, "import-denied");

    let network = execute("network", NETWORK_SOURCE).await;
    assert_eq!(network.classification, RunnerClassification::CompileError);

    let process = execute("process", PROCESS_SOURCE).await;
    assert_eq!(process.classification, RunnerClassification::CompileError);

    let timeout = execute("timeout", TIMEOUT_SOURCE).await;
    assert_eq!(timeout.classification, RunnerClassification::Timeout);

    let output = execute("output_flood", OUTPUT_FLOOD_SOURCE).await;
    assert!(matches!(
        output.classification,
        RunnerClassification::OutputLimit | RunnerClassification::CompileError
    ));
    if output.classification == RunnerClassification::OutputLimit {
        assert!(output.output_truncated);
    }

    let serialized = serde_json::to_string(&[
        static_import,
        dynamic_import,
        network,
        process,
        timeout,
        output,
    ])
    .expect("evidence JSON");
    for secret in [
        "VQ_OUTPUT_SENTINEL",
        "VQ_PROCESS_SENTINEL",
        "VQ_HOST_GLOBAL_SENTINEL",
        "/etc/passwd",
        "https://example.com",
    ] {
        assert!(
            !serialized.contains(secret),
            "worker evidence leaked raw source"
        );
    }
}
