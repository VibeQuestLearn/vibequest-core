use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncWriteExt, duplex},
    sync::{Notify, mpsc, watch},
};

use super::docker::ExecutorFuture;
use super::*;

fn request(source: &str) -> CreateSubmissionRequest {
    CreateSubmissionRequest {
        scenario_id: RUNNER_SCENARIO_ID.to_string(),
        scenario_manifest_version: runner_manifest().scenario_manifest_version.clone(),
        source: source.to_string(),
    }
}

fn evidence(job: &RunnerJob, classification: RunnerClassification, attempt: u8) -> RunnerEvidence {
    RunnerEvidence {
        binding: job.binding(),
        classification,
        public_cases: Vec::new(),
        hidden_summary: HiddenCaseSummary {
            passed: 0,
            failed: runner_manifest().tests.hidden_case_count,
        },
        worker_attempts: attempt,
        output_bytes: 0,
        output_truncated: false,
        diagnostic_code: match classification {
            RunnerClassification::Passed => "tests-passed",
            RunnerClassification::WorkerLost => "worker-lost",
            RunnerClassification::Cancelled => "cancelled",
            _ => "tests-failed",
        }
        .to_string(),
        result_digest: String::new(),
    }
}

struct SequenceExecutor {
    calls: AtomicUsize,
    classifications: Mutex<VecDeque<RunnerClassification>>,
    corrupt_binding: bool,
}

impl SequenceExecutor {
    fn new(classifications: impl IntoIterator<Item = RunnerClassification>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            classifications: Mutex::new(classifications.into_iter().collect()),
            corrupt_binding: false,
        }
    }

    fn corrupting() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            classifications: Mutex::new(VecDeque::from([RunnerClassification::Passed])),
            corrupt_binding: true,
        }
    }
}

impl SandboxExecutor for SequenceExecutor {
    fn execute<'a>(
        &'a self,
        job: RunnerJob,
        attempt: u8,
        _cancellation: watch::Receiver<bool>,
    ) -> ExecutorFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let classification = self
                .classifications
                .lock()
                .expect("classification lock")
                .pop_front()
                .unwrap_or(RunnerClassification::Passed);
            let mut result = evidence(&job, classification, attempt);
            if self.corrupt_binding {
                result.binding.source_digest = "0".repeat(64);
            }
            result
        })
    }
}

struct GateExecutor {
    calls: AtomicUsize,
    started: Notify,
    release: Notify,
}

impl SandboxExecutor for GateExecutor {
    fn execute<'a>(
        &'a self,
        job: RunnerJob,
        attempt: u8,
        _cancellation: watch::Receiver<bool>,
    ) -> ExecutorFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            evidence(&job, RunnerClassification::Passed, attempt)
        })
    }
}

async fn test_service(executor: Arc<dyn SandboxExecutor>) -> RunnerService {
    let (signer, verifier) = RunnerResultSigner::generate_for_tests().expect("test signing key");
    RunnerService::start(executor, signer, verifier)
}

async fn wait_for_terminal(
    service: &RunnerService,
    user_id: &str,
    submission_id: &str,
) -> RunnerSubmissionView {
    for _ in 0..100 {
        let view = service
            .get(user_id, submission_id)
            .await
            .expect("owned submission");
        if matches!(
            view.state,
            SubmissionState::Passed
                | SubmissionState::Failed
                | SubmissionState::Cancelled
                | SubmissionState::Error
        ) {
            return view;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("submission did not reach a terminal state");
}

#[test]
fn manifest_and_public_contract_are_pinned_and_disabled() {
    validate_runner_manifest().expect("reviewed runner manifest");
    let manifest = runner_manifest();
    assert!(!manifest.production_enabled);
    assert_eq!(manifest.toolchain.image, APPROVED_NODE_IMAGE);
    assert!(manifest.toolchain.dependency_allowlist.is_empty());
    assert_eq!(manifest.isolation.network, "none");
    assert!(!manifest.isolation.docker_socket_mounted);
    assert!(!RunnerService::from_environment().is_enabled());
}

#[test]
fn submission_request_rejects_client_claimed_results() {
    let encoded = serde_json::json!({
        "scenario_id": RUNNER_SCENARIO_ID,
        "scenario_manifest_version": runner_manifest().scenario_manifest_version,
        "source": SOLUTION_SOURCE,
        "passed": true
    });
    assert!(serde_json::from_value::<CreateSubmissionRequest>(encoded).is_err());
}

#[test]
fn signatures_cover_evidence_and_deterministic_results_ignore_runtime_noise() {
    let (signer, verifier) = RunnerResultSigner::generate_for_tests().expect("test signing key");
    let job = RunnerJob::reviewed(
        "job_signature".to_string(),
        "sub_signature".to_string(),
        "usr_0123456789abcdefghijklmnopqrstuv".to_string(),
        SOLUTION_SOURCE.to_string(),
    )
    .expect("reviewed job");

    let first = signer
        .sign(evidence(&job, RunnerClassification::Passed, 1))
        .expect("signed result");
    let mut noisy = evidence(&job, RunnerClassification::Passed, 2);
    noisy.output_bytes = 53_000;
    noisy.output_truncated = true;
    let second = signer.sign(noisy).expect("second signed result");

    verifier.verify(&first).expect("valid signature");
    verifier.verify(&second).expect("valid second signature");
    assert_eq!(first.evidence.result_digest, second.evidence.result_digest);

    let mut tampered = first;
    tampered.evidence.classification = RunnerClassification::Failed;
    assert!(verifier.verify(&tampered).is_err());
}

#[tokio::test]
async fn service_deduplicates_per_user_and_isolates_ownership() {
    let executor = Arc::new(SequenceExecutor::new([RunnerClassification::Passed]));
    let service = test_service(executor).await;
    let user = "usr_0123456789abcdefghijklmnopqrstuv";

    let first = service
        .submit(user, request(SOLUTION_SOURCE))
        .await
        .expect("first submission");
    let duplicate = service
        .submit(user, request(SOLUTION_SOURCE))
        .await
        .expect("deduplicated submission");
    assert_eq!(first.submission_id, duplicate.submission_id);
    assert_eq!(
        service
            .get("usr_abcdefghijklmnopqrstuvwxyz012345", &first.submission_id)
            .await,
        Err(RunnerServiceError::NotFound)
    );

    let completed = wait_for_terminal(&service, user, &first.submission_id).await;
    assert_eq!(completed.state, SubmissionState::Passed);
    let authenticated = service
        .authenticated_result(&first.submission_id)
        .await
        .expect("authenticated worker result");
    assert_eq!(authenticated.evidence.binding.user_id, user);
}

#[tokio::test]
async fn service_retries_worker_loss_once() {
    let executor = Arc::new(SequenceExecutor::new([
        RunnerClassification::WorkerLost,
        RunnerClassification::Passed,
    ]));
    let service = test_service(executor.clone()).await;
    let user = "usr_0123456789abcdefghijklmnopqrstuv";
    let submitted = service
        .submit(user, request(SOLUTION_SOURCE))
        .await
        .expect("queued submission");

    let completed = wait_for_terminal(&service, user, &submitted.submission_id).await;
    assert_eq!(completed.state, SubmissionState::Passed);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        service
            .authenticated_result(&submitted.submission_id)
            .await
            .expect("signed result")
            .evidence
            .worker_attempts,
        2
    );
}

#[tokio::test]
async fn cancellation_is_terminal_even_if_the_executor_finishes_afterward() {
    let executor = Arc::new(GateExecutor {
        calls: AtomicUsize::new(0),
        started: Notify::new(),
        release: Notify::new(),
    });
    let started = executor.started.notified();
    let service = test_service(executor.clone()).await;
    let user = "usr_0123456789abcdefghijklmnopqrstuv";
    let submitted = service
        .submit(user, request(SOLUTION_SOURCE))
        .await
        .expect("queued submission");

    started.await;
    let cancelled = service
        .cancel(user, &submitted.submission_id)
        .await
        .expect("cancelled submission");
    assert_eq!(cancelled.state, SubmissionState::Cancelled);
    executor.release.notify_one();
    tokio::time::sleep(Duration::from_millis(30)).await;

    let final_view = service
        .get(user, &submitted.submission_id)
        .await
        .expect("cancelled view");
    assert_eq!(final_view.state, SubmissionState::Cancelled);
    assert!(
        service
            .authenticated_result(&submitted.submission_id)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn queued_cancellation_never_reaches_the_executor() {
    let executor = Arc::new(GateExecutor {
        calls: AtomicUsize::new(0),
        started: Notify::new(),
        release: Notify::new(),
    });
    let started = executor.started.notified();
    let service = test_service(executor.clone()).await;
    let user = "usr_0123456789abcdefghijklmnopqrstuv";
    let first = service
        .submit(user, request(SOLUTION_SOURCE))
        .await
        .expect("first queued submission");
    started.await;
    let second_source = format!("{SOLUTION_SOURCE}\n// second submission");
    let second = service
        .submit(user, request(&second_source))
        .await
        .expect("second queued submission");
    let cancelled = service
        .cancel(user, &second.submission_id)
        .await
        .expect("cancel queued submission");
    assert_eq!(cancelled.state, SubmissionState::Cancelled);

    executor.release.notify_one();
    let first_completed = wait_for_terminal(&service, user, &first.submission_id).await;
    assert_eq!(first_completed.state, SubmissionState::Passed);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        service
            .get(user, &second.submission_id)
            .await
            .expect("cancelled queued view")
            .state,
        SubmissionState::Cancelled
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn service_rejects_signed_results_not_bound_to_the_exact_job() {
    let service = test_service(Arc::new(SequenceExecutor::corrupting())).await;
    let user = "usr_0123456789abcdefghijklmnopqrstuv";
    let submitted = service
        .submit(user, request(SOLUTION_SOURCE))
        .await
        .expect("queued submission");

    let completed = wait_for_terminal(&service, user, &submitted.submission_id).await;
    assert_eq!(completed.state, SubmissionState::Error);
    assert_eq!(
        completed.diagnostic_code.as_deref(),
        Some("worker-result-rejected")
    );
}

#[tokio::test]
async fn combined_worker_output_is_truncated_at_the_reviewed_limit() {
    let limit = 64;
    let total = Arc::new(AtomicUsize::new(0));
    let (signal, mut signalled) = mpsc::channel(1);
    let (mut writer, reader) = duplex(256);
    let write = tokio::spawn(async move {
        writer
            .write_all(&[b'x'; 128])
            .await
            .expect("write probe output");
    });

    let captured = super::docker::read_bounded(reader, limit, total.clone(), signal).await;
    write.await.expect("writer task");
    assert_eq!(captured.bytes.len(), limit);
    assert_eq!(total.load(Ordering::SeqCst), 128);
    assert_eq!(signalled.recv().await, Some(()));
}

#[test]
fn docker_arguments_apply_the_exact_reviewed_isolation_boundary() {
    let executor = DockerSandboxExecutor::approved();
    let arguments = executor.docker_arguments(std::path::Path::new("/tmp/reviewed-job"), "vq-test");
    let rendered = arguments.join(" ");

    for required in [
        "--network none",
        "--ipc none",
        "--read-only",
        "/tmp:rw,noexec,nosuid,nodev",
        "dst=/job,readonly",
        "--user 65534:65534",
        "--cap-drop ALL",
        "--security-opt no-new-privileges",
        "--pids-limit 32",
        "--memory 134217728",
        "--memory-swap 134217728",
        "--cpus 0.5",
        "--log-driver none",
        "--permission",
        "--allow-fs-read=/job",
        "--no-addons",
        "--disallow-code-generation-from-strings",
    ] {
        assert!(
            rendered.contains(required),
            "missing isolation argument: {required}"
        );
    }
    assert!(!rendered.contains("docker.sock"));
    assert!(!rendered.contains("--env"));
    assert!(rendered.contains(APPROVED_NODE_IMAGE));
}
