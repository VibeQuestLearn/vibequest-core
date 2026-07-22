use std::{
    env,
    io::{self, Read, Write},
    process::ExitCode,
};

use tokio::sync::watch;
use vibequest_core::runner::{
    DockerSandboxExecutor, RunnerJob, RunnerResultSigner, SandboxExecutor, runner_manifest,
    validate_runner_manifest,
};

const JOB_ENVELOPE_OVERHEAD_BYTES: usize = 16 * 1024;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            eprintln!("runner worker stopped: {code}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), &'static str> {
    validate_runner_manifest().map_err(|_| "manifest-validation-failed")?;
    let manifest = runner_manifest();
    if !manifest.production_enabled && env::var("RUNNER_REVIEW_MODE").ok().as_deref() != Some("1") {
        return Err("production-review-required");
    }

    let signing_config =
        env::var("RUNNER_RESULT_SIGNING_KEY").map_err(|_| "signing-key-unavailable")?;
    let signer =
        RunnerResultSigner::from_config(&signing_config).map_err(|_| "signing-key-invalid")?;

    let limit = manifest
        .source
        .max_bytes
        .saturating_add(JOB_ENVELOPE_OVERHEAD_BYTES);
    let mut encoded = Vec::with_capacity(limit.min(128 * 1024));
    io::stdin()
        .take((limit + 1) as u64)
        .read_to_end(&mut encoded)
        .map_err(|_| "job-read-failed")?;
    if encoded.is_empty() || encoded.len() > limit {
        return Err("job-envelope-limit");
    }

    let job: RunnerJob = serde_json::from_slice(&encoded).map_err(|_| "job-envelope-invalid")?;
    job.validate().map_err(|_| "job-validation-failed")?;

    let (_cancellation, cancellation) = watch::channel(false);
    let evidence = DockerSandboxExecutor::approved()
        .execute(job, 1, cancellation)
        .await;
    let authenticated = signer.sign(evidence).map_err(|_| "result-signing-failed")?;
    let output = serde_json::to_vec(&authenticated).map_err(|_| "result-serialization-failed")?;
    io::stdout()
        .write_all(&output)
        .map_err(|_| "result-write-failed")?;
    io::stdout()
        .write_all(b"\n")
        .map_err(|_| "result-write-failed")
}
