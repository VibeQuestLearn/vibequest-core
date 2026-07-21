# Legacy Coupling Inventory

Snapshot: 2026-07-21, inherited commit `1ceedfb`.

## Runtime Surfaces

| Area | Current coupling | Removal target |
| --- | --- | --- |
| `src/lib.rs` domain models | Wallet addresses, CKB/Fiber readiness, rewards | Chunks 01-03 |
| `src/lib.rs` handlers | JoyID proof, wallet-owned records, client progress | Chunks 01-02 |
| `src/lib.rs` AI prompts | CKB/Fiber curricula, quests, payouts | Chunks 03-04 |
| `src/lib.rs` verification | Lexical gates, boss answers, reward completion | Chunks 05-06 |
| `src/lib.rs` persistence | Wallet-keyed users, sessions, quests, claims | Chunks 01-02 and 07 |
| `src/main.rs` and `api/index.rs` | Legacy router and readiness surface | Chunks 01-03 |
| `.env.example` and README | CKB/Fiber RPC and payout configuration | Chunks 01-03 |

## Removal Sequence

- Chunk 01 introduces neutral identifiers, collections, routing, and configuration.
- Chunk 02 establishes trusted Google-backed internal identity.
- Chunk 03 adds reviewed Zcash protocol parsing and fixtures.
- Chunks 04-06 replace AI-authored lexical checks with executable evidence.
- Chunk 07 makes server-owned learner records and receipts authoritative.

## Invariants

- Wallet-keyed legacy records are not migrated into the new identity model.
- Browser identity headers and completion booleans are rejected.
- Completion is derived only from authenticated runner evidence.
