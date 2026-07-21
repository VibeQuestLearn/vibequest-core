# VibeQuestLearn Core

Rust and Axum backend for the ecosystem-neutral VibeQuestLearn platform. The active v3 boundary registers one focused Zcash developer track and accepts Google-backed identities only through signed Web assertions.

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

The inherited wallet-address, quest, AI, payout, and learning handlers are no longer registered in the router. They remain temporarily as unrouted migration source and cannot be called over HTTP.

## Ownership And Persistence

Core verifies signature, key ID, issuer, audience, provider, issue time, expiry, maximum lifetime, and assertion ID. It then recomputes `user_id` from provider plus stable Google `sub`; a valid signature cannot assign an arbitrary owner.

The `users` collection has a unique provider-plus-provider-subject index. Email and display name are mutable metadata. All account export and deletion filters come from `AuthenticatedPrincipal`, never request paths or bodies.

See `docs/authentication.md` for the trust boundary and key rotation procedure.

## Zcash Domain Engine

`src/zcash` is a network-free verifier boundary for the single shielded-checkout track. It uses exact official crate versions to inspect Revision 0 Unified Addresses, enforce receiver and network policy, validate bounded ZIP-321 ZEC requests, classify Unified Viewing Keys, and evaluate reviewed payment lifecycle fixtures.

The engine returns rule IDs, source references, and safe messages without serializing raw addresses, memos, or viewing keys. It has no HTTP wallet route and cannot construct or sign a transaction. Track catalog records expose source manifest `zcash-sources-2026-07-21.2`; see `fixtures/zcash/v1/source-manifest.json` and `docs/zcash-dependency-decision.md`.

## Reviewed Curriculum

`src/curriculum.rs` validates the complete five-lesson curriculum, scenario graph, official source references, answer keys, allowlisted defects, public/hidden case specifications, capstone requirements, and AI tutor contract. Every declared scenario case executes against the real Zcash verifier during Core tests.

The public curriculum endpoint works while the track is in `building` state, but returns only reviewed teaching content, code lenses, source links, checkpoint prompts, and aggregate case counts. Correct answers, rationales, hidden case IDs, seeded defects, and the solution body remain server-side.

AI output is optional and non-authoritative. Accepted artifacts must match one lesson and scenario version, cite only reviewed sources, and contain lesson-specific anchors. A bounded in-memory cache stores accepted artifacts; when a provider is unavailable, the contract returns reviewed local material. AI never defines tests, execution evidence, or completion.

## Checks

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
cargo deny check
```

The `zcashlearn` branch is the only implementation branch for this technical program.
