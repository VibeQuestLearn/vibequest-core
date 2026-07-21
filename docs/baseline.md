# Technical Baseline

Date: 2026-07-21
Branch: `zcashlearn`
Inherited commit: `1ceedfb`

## Toolchain

- rustc `1.91.0`
- cargo `1.91.0`

## Results

| Check | Result |
| --- | --- |
| `cargo fmt --check` | Pass |
| strict `cargo clippy` | Pass |
| `cargo test --all-features` | Pass, 15 tests |
| `cargo build --release` | Pass |
| `cargo deny check` | Pass after updating `crossbeam-epoch` to `0.9.20` |

## Observations

- The inherited tests cover AI response parsing, wallet proof helpers, Fiber invoice validation, and client-reported completion rules.
- No inherited test executes generated learner code or validates a Zcash protocol artifact.
- Cargo dependency and license policy is introduced by `deny.toml` and CI.
- The copied local `.env` is mode 600, ignored, and absent from Git status.

This snapshot records inherited behavior before ecosystem, identity, protocol, persistence, or runner changes.
