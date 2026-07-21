# VibeQuestLearn Core

Rust and Axum backend for the ecosystem-neutral VibeQuestLearn platform. The v3 boundary currently registers only one Zcash developer track.

## Run

```bash
cp .env.example .env
cargo run --bin vibequest-core
```

The ignored local `.env` may be copied from the original repository. Never commit it.

## V3 Environment

| Variable | Required | Default | Purpose |
| --- | --- | --- | --- |
| `APP_ENV` | No | `development` | Runtime environment label. |
| `PORT` | No | `8080` | Native HTTP port. |
| `CORS_ORIGINS` | No | `http://localhost:3000` | Allowed Web origins. |
| `MONGODB_URI` | For persistence | empty | Mongo connection used by the fresh v3 store. |
| `MONGODB_DATABASE_V3` | No | `vibequestlearn_v3` | Fresh database namespace for neutral records. |

Legacy CKB, Fiber, payout, and OpenAI variables remain in `.env.example` only while compatibility routes compile. They are not part of the v3 catalog contract.

## V3 Endpoints

- `GET /v3/catalog` returns schema version 3 and the validated ecosystem registry.
- `GET /v3/catalog/{ecosystem_id}/tracks/{track_id}` resolves enabled entries.
- Unknown ecosystems and tracks return `404 catalog-entry-not-found`.
- Registered but disabled entries return `409 catalog-entry-disabled`.

The current registry contains:

- ecosystem `zcash`, enabled
- track `shielded-payments-safety`, building and disabled
- ZIP-316 and ZIP-321 registration metadata
- testnet, non-custodial lab scope

## V3 Records

New contracts use opaque `user_id` ownership and a namespace containing:

- `schema_version`
- `ecosystem_id`
- `track_id`
- `track_version`
- `content_version`

Learning sessions, scenarios, submissions, and completion receipts have dedicated types and indexes. No wallet address, JoyID proof, CKB status, Fiber invoice, reward, or badge is required.

## Persistence

V3 uses `MONGODB_DATABASE_V3` and does not import legacy users or wallet-keyed records. Startup creates indexes for:

- `learning_sessions`
- `scenarios`
- `submissions`
- `completion_receipts`

Connection selection is bounded at three seconds and total initialization at five seconds. Catalog startup continues when Mongo is unavailable.

## Compatibility Boundary

The inherited handlers remain temporarily available so the branch stays buildable while later chunks remove them. They still live in `src/lib.rs` and are not consumed by the active Web root.

- wallet identity removal: Chunk 02
- protocol replacement: Chunks 03-04
- client-authoritative workbench removal: Chunks 05-06
- legacy persistence removal: Chunk 07

## Checks

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
cargo deny check
```

The `zcashlearn` branch is the only implementation branch for this technical program.
