# VibeQuestLearn Core

Rust and Axum backend for the ecosystem-neutral VibeQuestLearn platform. Core owns account identity verification, catalog delivery, learning state, reviewed track projections, optional runner evidence, and persistence.

The current grant-facing implementation is the Zcash Shielded Payments Track, but the backend contract is not tied to that track. Any CKB, Fiber, Zcash, or future ecosystem track must enter through the same catalog, curriculum, identity, submission, and evidence boundaries.

## Run

```bash
cp .env.example .env
cargo run --bin vibequest-core
```

The ignored local `.env` may be copied from the original repository. Never commit it.

## Identity Configuration

| Variable | Required for protected routes | Purpose |
| --- | --- | --- |
| `CORE_ASSERTION_KEYS` | Yes | Comma-separated verification key ring shared with Web. |
| `IDENTITY_DERIVATION_SECRET` | Yes | Independently derives the opaque ID from Google's stable `sub`. |
| `CORE_ASSERTION_ISSUER` | No | Expected issuer, default `vibequest-web`. |
| `CORE_ASSERTION_AUDIENCE` | No | Expected audience, default `vibequest-core`. |
| `MONGODB_URI` | For persistence | Connection used by the fresh v3 store. |
| `MONGODB_DATABASE_V3` | No | Fresh database namespace, default `vibequestlearn_v3`. |

Keys are unpadded base64url values decoding to at least 32 bytes. The identity derivation key must be independent of every signing and session key. Empty identity configuration disables protected routes with `503`; partial or malformed configuration also fails closed.

## Route Classes

Public:

- `GET /health`
- `GET /ready`
- `GET /v3/catalog`
- `GET /v3/catalog/{ecosystem_id}/tracks/{track_id}`
- `GET /v3/catalog/{ecosystem_id}/tracks/{track_id}/curriculum`

Protected by the assertion middleware:

- `GET /v3/me` upserts and returns the Google-backed profile.
- `GET /v3/me/export` exports the principal's owned v3 records.
- `DELETE /v3/me` deletes the principal's profile and owned v3 records.
- `POST /v3/submissions` validates a reviewed track submission; it fails closed when runner execution is not reviewed for that track.
- `GET /v3/submissions/{submission_id}` returns only the authenticated principal's bounded result view.
- `DELETE /v3/submissions/{submission_id}` requests terminal cancellation for the owner.

Wallet-address ownership, reward payout, invoice completion, and client-authoritative learning handlers are not part of the v3 router contract.

## Ownership And Persistence

Core verifies signature, key ID, issuer, audience, provider, issue time, expiry, maximum lifetime, and assertion ID. It then recomputes `user_id` from provider plus stable Google `sub`; a valid signature cannot assign an arbitrary owner.

The `users` collection has a unique provider-plus-provider-subject index. Email and display name are mutable metadata. All account export and deletion filters come from `AuthenticatedPrincipal`, never request paths or bodies.

See `docs/authentication.md` for the trust boundary and key rotation procedure.

## Ecosystem Track Contract

Every ecosystem track must be represented as a reviewed package with:

- ecosystem ID;
- track ID;
- content version;
- source manifest version;
- curriculum projection;
- optional scenario graph;
- optional runner manifest and runner version;
- completion policy;
- public/hidden test boundaries when execution exists.

Track-specific protocol behavior stays behind explicit adapters. A track may cite ecosystem sources, expose code lenses, or provide scenario cases, but it cannot define identity, ownership, persistence, or global completion behavior.

## Source-Grounded AI Tracks

AI-generated tracks use the same global validation contract across ecosystems: official-source grounding, minimum depth, repetition checks, checkpoint quality, placeholder rejection, learner-intent alignment, and persisted evaluation artifacts. The artifact now records source IDs, source categories, code-mode state, denial-test coverage, final-lab readiness, and unsupported-claim warnings so Web can expose why a generated lesson is trusted enough to show.

The TON / STON.fi track uses an expanded source pack covering STON.fi DEX overview, SDK, smart contracts, REST API, Omniston widget, Omniston SDK, TON Connect, TON Connect UI, TON token standards, and jetton processing/interface/architecture. Validation treats SDK/widget/REST outputs as integration inputs, not settlement proof, and expects denial cases for fake jettons, stale quotes, unsafe min-out, wallet rejection, manifest mismatch, duplicate connector state, referral-fee disclosure, and pending transaction state.

The Golem track uses a compute-specific source pack covering Golem docs, quickstarts, JS SDK, task model, requestor/provider interaction, Python, Ray, dApp deployment, provider docs, and Ray limitations. Validation treats Golem as decentralized compute infrastructure rather than a smart-contract chain, records execution path and compute-model coverage, expects requestor/provider/Yagna/task/result boundaries, and flags overclaims around provider output, cost, GPU/AI support, Ray support, and production certification.

The AIBTC / Stacks Agents track uses a source pack covering AIBTC home, LLM source map, bounty surfaces, bounty workflow documentation, OpenAPI schema, and Stacks docs. Validation records agent identity coverage, signed-action coverage, bounty-workflow coverage, sBTC payment-proof coverage, reputation evidence, unsafe-autonomy warnings, and final agent-lab readiness. It blocks lessons from implying wallet secret exposure, unapproved autonomous spending, pending-payment proof, or source-unsupported escrow/payout guarantees.

The Golem and AIBTC catalog entries expose grant-review proof metadata through `/v3/catalog`: sample topics, five-module sample paths, required evidence artifacts, demo steps, and source IDs. This makes reviewer paths inspectable without replacing the normal AI-generated course flow.

## Current Reviewed Track: Zcash Shielded Payments

`src/zcash` is a network-free verifier boundary for the shielded-checkout track. It uses exact official crate versions to inspect Revision 0 Unified Addresses, enforce receiver and network policy, validate bounded ZIP-321 ZEC requests, classify Unified Viewing Keys, and evaluate reviewed payment lifecycle fixtures.

The engine returns rule IDs, source references, and safe messages without serializing raw addresses, memos, or viewing keys. It has no HTTP wallet route and cannot construct or sign a transaction. Track catalog records expose source manifest `zcash-sources-2026-07-21.2`; see `fixtures/zcash/v1/source-manifest.json` and `docs/zcash-dependency-decision.md`.

This section documents the current track, not the platform architecture.

## Reviewed Curriculum

`src/curriculum.rs` validates the active curriculum, scenario graph, official source references, answer keys, allowlisted defects, public/hidden case specifications, capstone requirements, and AI tutor contract. Every declared scenario case executes against the reviewed verifier during Core tests when a runner is available for that track.

The public curriculum endpoint works while a track is in `building` state, but returns only reviewed teaching content, code lenses, source links, checkpoint prompts, and aggregate case counts. Correct answers, rationales, hidden case IDs, seeded defects, and solution bodies remain server-side.

AI output is optional and non-authoritative. Accepted artifacts must match one lesson and scenario version, cite only reviewed sources, and contain lesson-specific anchors. A bounded cache stores accepted artifacts; when a provider is unavailable, the contract returns reviewed local material. AI never defines tests, execution evidence, or completion.

## Isolated Scenario Runner

Runner execution is optional per track. The current pinned dependency-free Node worker executes only the reviewed shielded-checkout scenario in a locked-down Docker container. Jobs and signed evidence bind user, submission, scenario, source, tests, protocol, and runner versions. Output is truncated and raw learner diagnostics are not returned.

Production execution remains disabled and catalog status is `review-required` until an external queue adapter is reviewed. The Core API never loads the worker private signing key. See `docs/runner-operations.md` for the exact limits, adversarial acceptance suite, key boundary, and enablement checklist.

## Checks

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
cargo deny check
```

The `zcashlearn` branch is the active implementation branch for this technical program.
