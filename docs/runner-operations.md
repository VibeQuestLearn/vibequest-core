# Isolated Runner Operations

The Chunk 05 runner executes one reviewed TypeScript scenario. It is not a general code-hosting service, package builder, wallet, transaction signer, or network client.

## Current Gate

Production execution is disabled in `fixtures/zcash/v1/runner-manifest.json`. Catalog and curriculum responses expose the exact runner versions with status `review-required`. Authenticated submission routes exist, but return `503 runner-review-required`.

The API process never loads `RUNNER_RESULT_SIGNING_KEY`. The private Ed25519 key belongs only to the standalone `vibequest-runner-worker` process. The in-memory queue adapter is available only to Core unit tests.

Before production can be enabled, implement and review an external bounded queue adapter that:

- sends the validated `RunnerJob` envelope to a separately deployed worker;
- gives the API only `RUNNER_RESULT_VERIFY_KEYS`;
- gives the worker only `RUNNER_RESULT_SIGNING_KEY`;
- preserves cancellation, one retry for worker loss, capacity 32, and concurrency one;
- rejects results unless signature, full binding, and deterministic result digest all match the queued job.

Do not flip `production_enabled` or catalog status as part of a routine deployment.

## Pinned Runtime

The only approved image is:

`node:22-alpine@sha256:16e22a550f3863206a3f701448c45f7912c6896a62de43add43bb9c86130c3e2`

It contains Node `22.23.1`. Dependency installation is not supported and the dependency allowlist is empty.

Each job receives only four files: learner source, harness, public runner tests, and hidden runner tests. The solution, host environment, application secrets, Docker socket, other jobs, and repository are not mounted.

## Container Controls

The worker uses the exact policy pinned by the manifest and Rust validation:

- Docker network and IPC disabled;
- read-only root filesystem and read-only job bind mount;
- isolated `/tmp` tmpfs with `noexec,nosuid,nodev`;
- UID/GID 65534, all capabilities dropped, no new privileges;
- 0.5 CPU, 128 MiB memory and swap, 32 PIDs, 64 open files;
- eight-second wall timeout and one-second learner VM evaluation timeout;
- 64 KiB combined stdout/stderr limit;
- no Docker logging driver and no host environment forwarding;
- Node permission model plus a nested VM with imports, dynamic imports, process, filesystem, network, console, string code generation, and WebAssembly code generation unavailable.

The worker stores structured case statuses and bounded counters only. Raw stdout, stderr, source, hidden case details, addresses, memos, viewing keys, Google fields, and secrets are not returned in API evidence.

## Review Execution

The pinned image must already exist locally; the runner never pulls at job time.

Run the fast checks:

```bash
cargo test runner:: --lib
cargo test --test runner_isolation --no-run
```

Run the Docker acceptance suite explicitly:

```bash
cargo test --test runner_isolation -- --ignored --nocapture
```

The acceptance suite covers the reviewed solution, hidden/public case counts, timeout, output flood, static filesystem import, dynamic import, network access, process access, unavailable host globals, and evidence redaction.

To exercise the standalone worker during review, set `RUNNER_REVIEW_MODE=1` and a dedicated `RUNNER_RESULT_SIGNING_KEY` in the worker process only, then pass one serialized `RunnerJob` on stdin. Never place the private worker key in Web, Core API, a learner container, logs, or committed files.

## Incident Response

If a result signature, binding, or digest fails verification, classify the submission as `error` with `worker-result-rejected`; do not retry it as learner code. Retry only `worker-lost`, once.

Cancellation is terminal. A late result cannot replace a cancelled state. User ownership is derived from the authenticated Google principal, and another principal receives `404` for the submission.

After a worker crash or timeout, confirm no `com.vibequestlearn.runner` containers remain. Disable the external queue consumer before rotating worker keys. Add the new public key to API verification first, roll workers to the new private key, drain old jobs, then remove the old public key.
