use self::RunnerServiceError::*;
use chrono::Utc;
use std::{collections::BTreeMap, sync::Arc};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, mpsc, watch};
use uuid::Uuid;

use super::{
    AuthenticatedRunnerResult, CreateSubmissionRequest, RunnerClassification, RunnerJob,
    RunnerResultSigner, RunnerResultVerifier, RunnerSubmissionView, SandboxExecutor,
    SubmissionState, runner_manifest, sha256_hex,
};

#[derive(Clone)]
pub struct RunnerService {
    enabled: bool,
    sender: Option<mpsc::Sender<QueueItem>>,
    submissions: Arc<RwLock<BTreeMap<String, StoredSubmission>>>,
    deduplication: Arc<Mutex<BTreeMap<String, String>>>,
}

struct QueueItem {
    job: RunnerJob,
    cancellation: watch::Receiver<bool>,
}

struct StoredSubmission {
    owner_user_id: String,
    view: RunnerSubmissionView,
    cancellation: watch::Sender<bool>,
    authenticated_result: Option<AuthenticatedRunnerResult>,
}

impl RunnerService {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            sender: None,
            submissions: Arc::new(RwLock::new(BTreeMap::new())),
            deduplication: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn from_environment() -> Self {
        Self::disabled()
    }

    pub(crate) fn start(
        executor: Arc<dyn SandboxExecutor>,
        signer: RunnerResultSigner,
        verifier: RunnerResultVerifier,
    ) -> Self {
        let submissions = Arc::new(RwLock::new(BTreeMap::new()));
        let deduplication = Arc::new(Mutex::new(BTreeMap::new()));
        let (sender, receiver) = mpsc::channel(runner_manifest().limits.queue_capacity);
        tokio::spawn(worker_loop(
            receiver,
            submissions.clone(),
            executor,
            signer,
            verifier,
        ));

        Self {
            enabled: true,
            sender: Some(sender),
            submissions,
            deduplication,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub async fn submit(
        &self,
        user_id: &str,
        request: CreateSubmissionRequest,
    ) -> Result<RunnerSubmissionView, RunnerServiceError> {
        if !self.enabled {
            return Err(Disabled);
        }
        if request.scenario_id != runner_manifest().scenario_id
            || request.scenario_manifest_version != runner_manifest().scenario_manifest_version
        {
            return Err(InvalidRequest);
        }
        let source_digest = sha256_hex(request.source.as_bytes());
        let dedupe_key = deduplication_key(user_id, &source_digest);
        let mut deduplication = self.deduplication.lock().await;
        if let Some(existing_id) = deduplication.get(&dedupe_key) {
            let submissions = self.submissions.read().await;
            if let Some(existing) = submissions.get(existing_id)
                && existing.owner_user_id == user_id
                && existing.view.state != SubmissionState::Cancelled
            {
                return Ok(existing.view.clone());
            }
        }

        let submission_id = format!("sub_{}", Uuid::new_v4().simple());
        let job_id = format!("job_{}", Uuid::new_v4().simple());
        let job = RunnerJob::reviewed(
            job_id,
            submission_id.clone(),
            user_id.to_string(),
            request.source,
        )
        .map_err(|_| InvalidRequest)?;
        let (cancellation, cancellation_receiver) = watch::channel(false);
        let now = Utc::now();
        let view = RunnerSubmissionView {
            submission_id: submission_id.clone(),
            scenario_id: job.scenario_id.clone(),
            scenario_manifest_version: job.scenario_manifest_version.clone(),
            runner_version: job.runner_version.clone(),
            source_digest: job.source_digest.clone(),
            test_bundle_digest: job.test_bundle_digest.clone(),
            state: SubmissionState::Queued,
            classification: None,
            public_cases: Vec::new(),
            hidden_passed: 0,
            hidden_failed: 0,
            result_digest: None,
            diagnostic_code: None,
            output_truncated: false,
            created_at: now,
            updated_at: now,
        };
        self.submissions.write().await.insert(
            submission_id.clone(),
            StoredSubmission {
                owner_user_id: user_id.to_string(),
                view: view.clone(),
                cancellation,
                authenticated_result: None,
            },
        );
        deduplication.insert(dedupe_key.clone(), submission_id.clone());
        drop(deduplication);

        let item = QueueItem {
            job,
            cancellation: cancellation_receiver,
        };
        let Some(sender) = &self.sender else {
            return Err(Disabled);
        };
        if sender.try_send(item).is_err() {
            self.submissions.write().await.remove(&submission_id);
            self.deduplication.lock().await.remove(&dedupe_key);
            return Err(QueueFull);
        }
        Ok(view)
    }

    pub async fn get(
        &self,
        user_id: &str,
        submission_id: &str,
    ) -> Result<RunnerSubmissionView, RunnerServiceError> {
        if !self.enabled {
            return Err(Disabled);
        }
        let submissions = self.submissions.read().await;
        let submission = submissions.get(submission_id).ok_or(NotFound)?;
        if submission.owner_user_id != user_id {
            return Err(NotFound);
        }
        Ok(submission.view.clone())
    }

    pub async fn cancel(
        &self,
        user_id: &str,
        submission_id: &str,
    ) -> Result<RunnerSubmissionView, RunnerServiceError> {
        if !self.enabled {
            return Err(Disabled);
        }
        let mut submissions = self.submissions.write().await;
        let submission = submissions.get_mut(submission_id).ok_or(NotFound)?;
        if submission.owner_user_id != user_id {
            return Err(NotFound);
        }
        if matches!(
            submission.view.state,
            SubmissionState::Passed
                | SubmissionState::Failed
                | SubmissionState::Cancelled
                | SubmissionState::Error
        ) {
            return Ok(submission.view.clone());
        }
        let _ = submission.cancellation.send(true);
        submission.view.state = SubmissionState::Cancelled;
        submission.view.classification = Some(RunnerClassification::Cancelled);
        submission.view.diagnostic_code = Some("cancelled".to_string());
        submission.view.updated_at = Utc::now();
        Ok(submission.view.clone())
    }

    #[cfg(test)]
    pub(crate) async fn authenticated_result(
        &self,
        submission_id: &str,
    ) -> Option<AuthenticatedRunnerResult> {
        self.submissions
            .read()
            .await
            .get(submission_id)
            .and_then(|submission| submission.authenticated_result.clone())
    }
}

async fn worker_loop(
    mut receiver: mpsc::Receiver<QueueItem>,
    submissions: Arc<RwLock<BTreeMap<String, StoredSubmission>>>,
    executor: Arc<dyn SandboxExecutor>,
    signer: RunnerResultSigner,
    verifier: RunnerResultVerifier,
) {
    while let Some(item) = receiver.recv().await {
        let submission_id = item.job.submission_id.clone();
        if *item.cancellation.borrow() {
            store_cancelled(&submissions, &submission_id).await;
            continue;
        }
        {
            let mut records = submissions.write().await;
            let Some(record) = records.get_mut(&submission_id) else {
                continue;
            };
            if record.view.state == SubmissionState::Cancelled || *item.cancellation.borrow() {
                record.view.state = SubmissionState::Cancelled;
                record.view.classification = Some(RunnerClassification::Cancelled);
                record.view.diagnostic_code = Some("cancelled".to_string());
                record.view.updated_at = Utc::now();
                continue;
            }
            record.view.state = SubmissionState::Running;
            record.view.updated_at = Utc::now();
        }

        let mut attempt = 1_u8;
        let authenticated = loop {
            let evidence = executor
                .execute(item.job.clone(), attempt, item.cancellation.clone())
                .await;
            if evidence.classification == RunnerClassification::WorkerLost
                && attempt <= runner_manifest().limits.max_retries
                && !*item.cancellation.borrow()
            {
                attempt += 1;
                continue;
            }
            break signer.sign(evidence);
        };
        let Ok(authenticated) = authenticated else {
            store_worker_error(&submissions, &submission_id, "worker-signature-failed").await;
            continue;
        };
        if verifier.verify(&authenticated).is_err()
            || authenticated.evidence.binding != item.job.binding()
        {
            store_worker_error(&submissions, &submission_id, "worker-result-rejected").await;
            continue;
        }
        store_authenticated(&submissions, &submission_id, authenticated).await;
    }
}

async fn store_authenticated(
    submissions: &RwLock<BTreeMap<String, StoredSubmission>>,
    submission_id: &str,
    authenticated: AuthenticatedRunnerResult,
) {
    let mut records = submissions.write().await;
    let Some(record) = records.get_mut(submission_id) else {
        return;
    };
    if record.view.state == SubmissionState::Cancelled {
        return;
    }
    let evidence = &authenticated.evidence;
    record.view.state = submission_state(evidence.classification);
    record.view.classification = Some(evidence.classification);
    record.view.public_cases = evidence.public_cases.clone();
    record.view.hidden_passed = evidence.hidden_summary.passed;
    record.view.hidden_failed = evidence.hidden_summary.failed;
    record.view.result_digest = Some(evidence.result_digest.clone());
    record.view.diagnostic_code = Some(evidence.diagnostic_code.clone());
    record.view.output_truncated = evidence.output_truncated;
    record.view.updated_at = Utc::now();
    record.authenticated_result = Some(authenticated);
}

async fn store_cancelled(
    submissions: &RwLock<BTreeMap<String, StoredSubmission>>,
    submission_id: &str,
) {
    let mut records = submissions.write().await;
    let Some(record) = records.get_mut(submission_id) else {
        return;
    };
    record.view.state = SubmissionState::Cancelled;
    record.view.classification = Some(RunnerClassification::Cancelled);
    record.view.diagnostic_code = Some("cancelled".to_string());
    record.view.updated_at = Utc::now();
}

async fn store_worker_error(
    submissions: &RwLock<BTreeMap<String, StoredSubmission>>,
    submission_id: &str,
    diagnostic: &str,
) {
    let mut records = submissions.write().await;
    let Some(record) = records.get_mut(submission_id) else {
        return;
    };
    if record.view.state == SubmissionState::Cancelled {
        return;
    }
    record.view.state = SubmissionState::Error;
    record.view.classification = Some(RunnerClassification::WorkerLost);
    record.view.diagnostic_code = Some(diagnostic.to_string());
    record.view.updated_at = Utc::now();
}

fn submission_state(classification: RunnerClassification) -> SubmissionState {
    match classification {
        RunnerClassification::Passed => SubmissionState::Passed,
        RunnerClassification::Cancelled => SubmissionState::Cancelled,
        RunnerClassification::Failed
        | RunnerClassification::CompileError
        | RunnerClassification::Timeout
        | RunnerClassification::OutputLimit
        | RunnerClassification::ResourceLimit => SubmissionState::Failed,
        RunnerClassification::IsolationError | RunnerClassification::WorkerLost => {
            SubmissionState::Error
        }
    }
}

fn deduplication_key(user_id: &str, source_digest: &str) -> String {
    let manifest = runner_manifest();
    sha256_hex(
        format!(
            "{}\0{}\0{}\0{}\0{}",
            user_id,
            manifest.scenario_manifest_version,
            source_digest,
            manifest.tests.bundle_sha256,
            manifest.runner_version
        )
        .as_bytes(),
    )
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RunnerServiceError {
    #[error("runner is disabled")]
    Disabled,
    #[error("runner request is invalid")]
    InvalidRequest,
    #[error("runner queue is full")]
    QueueFull,
    #[error("submission was not found")]
    NotFound,
}
