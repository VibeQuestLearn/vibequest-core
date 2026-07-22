use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::{ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{mpsc, watch},
    task::JoinHandle,
};

use super::{
    CaseStatus, HARNESS_SOURCE, HIDDEN_TESTS_SOURCE, HarnessClassification, HarnessResult,
    HiddenCaseSummary, PUBLIC_TESTS_SOURCE, PublicCaseResult, RunnerClassification, RunnerEvidence,
    RunnerJob, runner_manifest,
};

pub type ExecutorFuture<'a> = Pin<Box<dyn Future<Output = RunnerEvidence> + Send + 'a>>;

pub trait SandboxExecutor: Send + Sync {
    fn execute<'a>(
        &'a self,
        job: RunnerJob,
        attempt: u8,
        cancellation: watch::Receiver<bool>,
    ) -> ExecutorFuture<'a>;
}

#[derive(Clone, Debug)]
pub struct DockerSandboxExecutor {
    docker_binary: PathBuf,
}

impl DockerSandboxExecutor {
    pub fn approved() -> Self {
        Self {
            docker_binary: PathBuf::from("/usr/bin/docker"),
        }
    }

    pub fn docker_arguments(&self, job_root: &Path, container_name: &str) -> Vec<String> {
        let manifest = runner_manifest();
        vec![
            "run".to_string(),
            "--rm".to_string(),
            "--pull=never".to_string(),
            "--name".to_string(),
            container_name.to_string(),
            "--hostname".to_string(),
            "vibequest-runner".to_string(),
            "--network".to_string(),
            "none".to_string(),
            "--ipc".to_string(),
            "none".to_string(),
            "--read-only".to_string(),
            "--tmpfs".to_string(),
            format!(
                "/tmp:rw,noexec,nosuid,nodev,size={}",
                manifest.limits.tmpfs_bytes
            ),
            "--mount".to_string(),
            format!("type=bind,src={},dst=/job,readonly", job_root.display()),
            "--workdir".to_string(),
            "/job".to_string(),
            "--user".to_string(),
            format!("{}:{}", manifest.isolation.uid, manifest.isolation.gid),
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "--pids-limit".to_string(),
            manifest.limits.pids.to_string(),
            "--memory".to_string(),
            manifest.limits.memory_bytes.to_string(),
            "--memory-swap".to_string(),
            manifest.limits.memory_bytes.to_string(),
            "--cpus".to_string(),
            manifest.limits.cpu_cores.to_string(),
            "--ulimit".to_string(),
            format!(
                "nofile={}:{}",
                manifest.limits.open_files, manifest.limits.open_files
            ),
            "--ulimit".to_string(),
            "core=0".to_string(),
            "--stop-timeout".to_string(),
            "1".to_string(),
            "--log-driver".to_string(),
            "none".to_string(),
            "--label".to_string(),
            format!("com.vibequestlearn.runner={}", manifest.runner_version),
            manifest.toolchain.image.clone(),
            "node".to_string(),
            "--no-warnings".to_string(),
            "--experimental-vm-modules".to_string(),
            "--permission".to_string(),
            "--allow-fs-read=/job".to_string(),
            "--no-addons".to_string(),
            "--disable-proto=delete".to_string(),
            "--frozen-intrinsics".to_string(),
            "--disallow-code-generation-from-strings".to_string(),
            "/job/runner/harness.mjs".to_string(),
        ]
    }

    async fn execute_inner(
        &self,
        job: RunnerJob,
        attempt: u8,
        mut cancellation: watch::Receiver<bool>,
    ) -> RunnerEvidence {
        let binding = job.binding();
        if job.validate().is_err() {
            return empty_evidence(
                binding,
                RunnerClassification::IsolationError,
                attempt,
                "job-validation-failed",
                0,
                false,
            );
        }
        if *cancellation.borrow() {
            return empty_evidence(
                binding,
                RunnerClassification::Cancelled,
                attempt,
                "cancelled",
                0,
                false,
            );
        }

        let staged = match stage_job(&job).await {
            Ok(staged) => staged,
            Err(_) => {
                return empty_evidence(
                    binding,
                    RunnerClassification::WorkerLost,
                    attempt,
                    "job-staging-failed",
                    0,
                    false,
                );
            }
        };
        let container_name = format!("vq-{}", job.job_id);
        let arguments = self.docker_arguments(staged.path(), &container_name);
        let mut command = Command::new(&self.docker_binary);
        command
            .args(arguments)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                return empty_evidence(
                    binding,
                    RunnerClassification::WorkerLost,
                    attempt,
                    "worker-start-failed",
                    0,
                    false,
                );
            }
        };

        let output_limit = runner_manifest().limits.max_output_bytes;
        let total_bytes = Arc::new(AtomicUsize::new(0));
        let (limit_tx, mut limit_rx) = mpsc::channel(1);
        let stdout_task = child.stdout.take().map(|stdout| {
            tokio::spawn(read_bounded(
                stdout,
                output_limit,
                total_bytes.clone(),
                limit_tx.clone(),
            ))
        });
        let stderr_task = child.stderr.take().map(|stderr| {
            tokio::spawn(read_bounded(
                stderr,
                output_limit,
                total_bytes.clone(),
                limit_tx,
            ))
        });

        let termination = {
            let wait = child.wait();
            tokio::pin!(wait);
            tokio::select! {
                status = &mut wait => match status {
                    Ok(status) => Termination::Exited(status),
                    Err(_) => Termination::WorkerLost,
                },
                _ = tokio::time::sleep(Duration::from_millis(
                    runner_manifest().limits.wall_clock_ms
                )) => Termination::Timeout,
                Some(()) = limit_rx.recv() => Termination::OutputLimit,
                changed = cancellation.changed() => {
                    if changed.is_ok() && *cancellation.borrow() {
                        Termination::Cancelled
                    } else {
                        Termination::WorkerLost
                    }
                }
            }
        };

        if !matches!(&termination, Termination::Exited(_)) {
            cleanup_container(&self.docker_binary, &container_name).await;
            let _ = child.wait().await;
        }

        let stdout = join_capture(stdout_task).await;
        let _stderr = join_capture(stderr_task).await;
        let observed_bytes = total_bytes.load(Ordering::Relaxed);
        let public_output_bytes = observed_bytes.min(output_limit.saturating_add(1));
        let truncated = observed_bytes > output_limit;

        match termination {
            Termination::Exited(status) => evidence_from_exit(
                &job,
                attempt,
                status,
                &stdout,
                public_output_bytes,
                truncated,
            ),
            Termination::Timeout => empty_evidence(
                binding,
                RunnerClassification::Timeout,
                attempt,
                "timeout",
                public_output_bytes,
                truncated,
            ),
            Termination::OutputLimit => empty_evidence(
                binding,
                RunnerClassification::OutputLimit,
                attempt,
                "output-limit",
                public_output_bytes,
                true,
            ),
            Termination::Cancelled => empty_evidence(
                binding,
                RunnerClassification::Cancelled,
                attempt,
                "cancelled",
                public_output_bytes,
                truncated,
            ),
            Termination::WorkerLost => empty_evidence(
                binding,
                RunnerClassification::WorkerLost,
                attempt,
                "worker-lost",
                public_output_bytes,
                truncated,
            ),
        }
    }
}

impl SandboxExecutor for DockerSandboxExecutor {
    fn execute<'a>(
        &'a self,
        job: RunnerJob,
        attempt: u8,
        cancellation: watch::Receiver<bool>,
    ) -> ExecutorFuture<'a> {
        Box::pin(self.execute_inner(job, attempt, cancellation))
    }
}

enum Termination {
    Exited(ExitStatus),
    Timeout,
    OutputLimit,
    Cancelled,
    WorkerLost,
}

pub(super) struct StreamCapture {
    pub(super) bytes: Vec<u8>,
}

pub(super) async fn read_bounded<R>(
    mut reader: R,
    limit: usize,
    total: Arc<AtomicUsize>,
    limit_signal: mpsc::Sender<()>,
) -> StreamCapture
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 4_096];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let previous = total.fetch_add(read, Ordering::Relaxed);
        if previous < limit {
            let retained = (limit - previous).min(read);
            captured.extend_from_slice(&buffer[..retained]);
        }
        if previous.saturating_add(read) > limit {
            let _ = limit_signal.try_send(());
        }
    }
    StreamCapture { bytes: captured }
}

async fn join_capture(task: Option<JoinHandle<StreamCapture>>) -> Vec<u8> {
    match task {
        Some(task) => task.await.map(|capture| capture.bytes).unwrap_or_default(),
        None => Vec::new(),
    }
}

async fn stage_job(job: &RunnerJob) -> Result<tempfile::TempDir, std::io::Error> {
    let directory = tempfile::Builder::new()
        .prefix("vibequest-runner-")
        .tempdir()?;
    let starter = directory.path().join("starter/src");
    let runner = directory.path().join("runner");
    tokio::fs::create_dir_all(&starter).await?;
    tokio::fs::create_dir_all(&runner).await?;
    tokio::fs::write(starter.join("checkout.ts"), job.source.as_bytes()).await?;
    tokio::fs::write(runner.join("harness.mjs"), HARNESS_SOURCE.as_bytes()).await?;
    tokio::fs::write(
        runner.join("public-tests.mjs"),
        PUBLIC_TESTS_SOURCE.as_bytes(),
    )
    .await?;
    tokio::fs::write(
        runner.join("hidden-tests.mjs"),
        HIDDEN_TESTS_SOURCE.as_bytes(),
    )
    .await?;
    make_job_readable(directory.path())?;
    Ok(directory)
}

#[cfg(unix)]
fn make_job_readable(root: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    for entry in [
        root.to_path_buf(),
        root.join("starter"),
        root.join("starter/src"),
        root.join("runner"),
    ] {
        std::fs::set_permissions(entry, std::fs::Permissions::from_mode(0o755))?;
    }
    for entry in [
        root.join("starter/src/checkout.ts"),
        root.join("runner/harness.mjs"),
        root.join("runner/public-tests.mjs"),
        root.join("runner/hidden-tests.mjs"),
    ] {
        std::fs::set_permissions(entry, std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_job_readable(_root: &Path) -> Result<(), std::io::Error> {
    Err(std::io::Error::other(
        "the reviewed runner requires a Unix host",
    ))
}

async fn cleanup_container(docker_binary: &Path, container_name: &str) {
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        Command::new(docker_binary)
            .args(["rm", "--force", container_name])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
}

fn evidence_from_exit(
    job: &RunnerJob,
    attempt: u8,
    status: ExitStatus,
    stdout: &[u8],
    output_bytes: usize,
    output_truncated: bool,
) -> RunnerEvidence {
    if status.code() == Some(137) {
        return empty_evidence(
            job.binding(),
            RunnerClassification::ResourceLimit,
            attempt,
            "resource-limit",
            output_bytes,
            output_truncated,
        );
    }
    if status.code() == Some(124) {
        return empty_evidence(
            job.binding(),
            RunnerClassification::Timeout,
            attempt,
            "timeout",
            output_bytes,
            output_truncated,
        );
    }

    let Some(harness) = parse_harness_result(stdout) else {
        return empty_evidence(
            job.binding(),
            RunnerClassification::IsolationError,
            attempt,
            "invalid-worker-result",
            output_bytes,
            output_truncated,
        );
    };
    if validate_harness_result(&harness, status).is_err() {
        return empty_evidence(
            job.binding(),
            RunnerClassification::IsolationError,
            attempt,
            "invalid-worker-result",
            output_bytes,
            output_truncated,
        );
    }
    let (classification, diagnostic_code) = match harness.classification {
        HarnessClassification::Passed => (RunnerClassification::Passed, "tests-passed"),
        HarnessClassification::Failed => (RunnerClassification::Failed, "tests-failed"),
        HarnessClassification::Timeout => (RunnerClassification::Timeout, "timeout"),
        HarnessClassification::CompileError | HarnessClassification::SourceLimit => {
            (RunnerClassification::CompileError, "compile-error")
        }
        HarnessClassification::ImportDenied | HarnessClassification::DynamicImportDenied => {
            (RunnerClassification::CompileError, "import-denied")
        }
    };

    RunnerEvidence {
        binding: job.binding(),
        classification,
        public_cases: harness.public_cases,
        hidden_summary: harness.hidden_summary,
        worker_attempts: attempt,
        output_bytes,
        output_truncated,
        diagnostic_code: diagnostic_code.to_string(),
        result_digest: String::new(),
    }
}

fn parse_harness_result(stdout: &[u8]) -> Option<HarnessResult> {
    let output = std::str::from_utf8(stdout).ok()?;
    let mut results = output
        .lines()
        .filter_map(|line| line.strip_prefix("VQ_RESULT "))
        .map(serde_json::from_str::<HarnessResult>);
    let result = results.next()?.ok()?;
    if results.next().is_some() {
        return None;
    }
    Some(result)
}

fn validate_harness_result(result: &HarnessResult, status: ExitStatus) -> Result<(), ()> {
    if result.protocol_version != super::RUNNER_PROTOCOL_VERSION
        || result.hidden_summary.passed + result.hidden_summary.failed
            != runner_manifest().tests.hidden_case_count
    {
        return Err(());
    }
    let expected_public = crate::curriculum::scenario_manifest()
        .cases
        .iter()
        .filter(|case| case.visibility == crate::curriculum::CaseVisibility::Public)
        .map(|case| case.case_id.as_str())
        .collect::<Vec<_>>();
    let actual_public = result
        .public_cases
        .iter()
        .map(|case| case.case_id.as_str())
        .collect::<Vec<_>>();
    let public_shape_valid = match result.classification {
        HarnessClassification::Passed | HarnessClassification::Failed => {
            actual_public == expected_public
        }
        HarnessClassification::CompileError
        | HarnessClassification::SourceLimit
        | HarnessClassification::ImportDenied
        | HarnessClassification::DynamicImportDenied
        | HarnessClassification::Timeout => result.public_cases.is_empty(),
    };
    if !public_shape_valid {
        return Err(());
    }

    match result.classification {
        HarnessClassification::Passed
            if status.success()
                && result
                    .public_cases
                    .iter()
                    .all(|case| case.status == CaseStatus::Passed)
                && result.hidden_summary.failed == 0 =>
        {
            Ok(())
        }
        HarnessClassification::Failed if status.code() == Some(1) => Ok(()),
        HarnessClassification::CompileError
        | HarnessClassification::SourceLimit
        | HarnessClassification::ImportDenied
        | HarnessClassification::DynamicImportDenied
            if status.code() == Some(2) =>
        {
            Ok(())
        }
        HarnessClassification::Timeout if status.code() == Some(124) => Ok(()),
        _ => Err(()),
    }
}

fn empty_evidence(
    binding: super::RunnerBinding,
    classification: RunnerClassification,
    attempt: u8,
    diagnostic_code: &str,
    output_bytes: usize,
    output_truncated: bool,
) -> RunnerEvidence {
    RunnerEvidence {
        binding,
        classification,
        public_cases: Vec::<PublicCaseResult>::new(),
        hidden_summary: HiddenCaseSummary {
            passed: 0,
            failed: runner_manifest().tests.hidden_case_count,
        },
        worker_attempts: attempt,
        output_bytes,
        output_truncated,
        diagnostic_code: diagnostic_code.to_string(),
        result_digest: String::new(),
    }
}
