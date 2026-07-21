# Contributing

VibeQuestLearn Core is being rebuilt around ecosystem-neutral learner records and narrow, reviewed Zcash protocol labs.

## Branch And Scope

- Work on `zcashlearn`; do not commit directly to `main`.
- Keep changes within the approved technical execution plan.
- Keep CKB-era repositories reference-only.

## Local Setup

```bash
cp .env.example .env
cargo run
```

`.env` is local-only. Never stage or commit it.

## Required Checks

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
cargo deny check
```

## Engineering Rules

- Keep shared domain models ecosystem-neutral.
- Put Zcash-specific parsing and verification behind explicit modules.
- Reject browser identity headers, client pass booleans, and unsigned worker results.
- Add deterministic denial-path tests for every security boundary.
- Update API and privacy documentation with behavior or data changes.

## Attribution

Alternate primary commit authorship between the authorized FidelCoder and buidlLabs3 identities.
Use a `Co-authored-by` trailer when both profiles contributed to the same technical change.
