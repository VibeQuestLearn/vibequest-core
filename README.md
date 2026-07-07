# VibeQuest Core

Rust backend for VibeQuest. It owns AI quest generation, wallet proof verification, game progression, and CKB/Fiber readiness gates.

## Demo

Watch the product walkthrough: [demo.webm](https://github.com/buidlLabs3/vibequest-web/blob/main/demo.webm).

## Run

```bash
cp .env.example .env
cargo run
```

`/ai/quests/generate` requires `OPENAI_API_KEY`, `MONGODB_URI`, and a verified JoyID wallet proof. `/ready` returns `503` until OpenAI, CKB RPC, Fiber RPC, and MongoDB are configured.

## Environment

| Variable | Required | Default | Purpose |
| --- | --- | --- | --- |
| `APP_ENV` | No | `development` | Environment label returned by `/health`. |
| `PORT` | No | `8080` | HTTP server port. |
| `CORS_ORIGINS` | No | `http://localhost:3000` | Comma-separated frontend origins. Use `*` only for throwaway demos. |
| `OPENAI_API_KEY` | Yes | empty | Enables OpenAI quest generation. |
| `OPENAI_MODEL` | No | `gpt-5.5` | OpenAI model for quest generation. |
| `OPENAI_BASE_URL` | No | `https://share-ai.ckbdev.com` | OpenAI-compatible Responses API gateway. |
| `OPENAI_REASONING_EFFORT` | No | `xhigh` | Reasoning effort sent as `reasoning.effort`. |
| `OPENAI_DISABLE_RESPONSE_STORAGE` | No | `true` | Sends `store: false` to avoid retaining generated responses. |
| `OPENAI_TIMEOUT_SECONDS` | No | `180` | Request timeout for slower high-reasoning OpenAI calls. |
| `CKB_RPC_URL` | Yes | empty | CKB RPC endpoint used by proof receipt and reward-claim readiness gates. |
| `FIBER_RPC_URL` | Yes | empty | Fiber RPC endpoint used by payment, hint fee, and reward-claim readiness gates. Use a public endpoint for cloud deploys. |
| `MONGODB_URI` | Yes | empty | MongoDB Atlas connection string for users, quest runs, progress, and ship state. |
| `MONGODB_DATABASE` | No | `vibequest` | Database name used by the persistence layer. |
| `FIBER_PAYOUT_ENABLED` | No | `false` | Enables real Fiber `send_payment` execution after server-side completion checks pass. |
| `FIBER_PAYOUT_RPC_URL` | Required when payout enabled | empty | Private funded Fiber node JSON-RPC URL used for reward payouts. Do not expose this RPC publicly. |
| `VIBEQUEST_REWARD_SHANNONS` | No | `400` | Default incentive amount for completed quests. |
| `VIBEQUEST_REWARD_CURRENCY` | No | `Fibd` | Reward currency label for generated claims. |

## Endpoints

- `GET /health` - service status, integration readiness, and missing configuration.
- `GET /ready` - production readiness check; returns `503` until OpenAI, CKB RPC, and Fiber RPC are configured.
- `GET /season` - Season 0 tracks, gates, and product thesis.
- `GET /users/{address}/quests` - returns the wallet profile, created/completed/uncompleted counts, active run, and recent quest history.
- `GET /quests/{run_id}` - returns one persisted quest run.
- `POST /quests/{run_id}/progress` - verifies the JoyID wallet proof, then saves gate and boss progress.
- `POST /quests/{run_id}/complete` - performs server-side completion checks, creates a reward claim, and calls Fiber `send_payment` when payouts are enabled.
- `POST /ai/quests/generate` - verifies a JoyID wallet proof, generates a quest, and stores it as a MongoDB quest run.

Example:

```bash
curl -X POST http://localhost:8080/ai/quests/generate \
  -H 'content-type: application/json' \
  -d '{
    "build_prompt": "Build a Fiber-powered paid content app with CKB proof receipts",
    "skill_track": "Fiber Builder",
    "difficulty": "builder",
    "wallet": {
      "address": "ckt1...",
      "message": "VibeQuest wallet proof\nAddress: ckt1...\nIssued: 2026-06-24T00:00:00.000Z\nPurpose: bind generated quest runs, proof notes, and reward claims to this signer.",
      "signature": {
        "signature": "0x...",
        "identity": "0x...",
        "sign_type": "JoyId"
      }
    }
  }'
```

## Checks

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
```

## Deploy to Vercel

This backend ships with the official Vercel Rust runtime through `api/index.rs` and `vercel.json`.

```bash
vercel link --project vibequest-core
vercel env add OPENAI_API_KEY production
vercel env add OPENAI_MODEL production
vercel env add OPENAI_BASE_URL production
vercel env add OPENAI_REASONING_EFFORT production
vercel env add OPENAI_DISABLE_RESPONSE_STORAGE production
vercel env add OPENAI_TIMEOUT_SECONDS production
vercel env add CKB_RPC_URL production
vercel env add FIBER_RPC_URL production
vercel env add MONGODB_URI production
vercel env add MONGODB_DATABASE production
vercel env add FIBER_PAYOUT_ENABLED production
vercel env add FIBER_PAYOUT_RPC_URL production
vercel env add VIBEQUEST_REWARD_SHANNONS production
vercel env add VIBEQUEST_REWARD_CURRENCY production
vercel --prod
```

`FIBER_RPC_URL=http://127.0.0.1:8227` is only valid for local development. Cloud deployments need a public Fiber RPC endpoint, such as the current testnet public node used by the deployment.

## Docker

```bash
docker build -t vibequest-core .
docker run --rm -p 8080:8080 --env-file .env vibequest-core
```

## Runtime Modules

- OpenAI orchestration for quest design, code explanation, and challenge generation.
- MongoDB persistence for users, created quests, uncompleted quests, completed quests, gate progress, boss state, and ship envelopes.
- Test-runner workers for debug and no-prompt zones.
- CKB proof adapter for badges, receipts, and skill passport history.
- Fiber reward adapter for invoice-based quest payouts, with MongoDB claim ledger and payment receipts.
