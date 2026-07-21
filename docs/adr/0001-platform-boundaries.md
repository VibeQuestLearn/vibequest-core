# ADR 0001: Platform And Ecosystem Boundaries

Status: Accepted
Date: 2026-07-21

## Context

The inherited application couples learner identity, persistence, progression, rewards, and interface copy to CKB, Fiber, JoyID, and wallet addresses.

## Decision

- Shared platform types use opaque user, ecosystem, track, lesson, scenario, and submission identifiers.
- Zcash protocol behavior lives in explicit Zcash adapters and reviewed scenario packages.
- Google OAuth authenticates the learning account.
- The server establishes authorization; browser identity headers and completion booleans are untrusted.
- AI may explain and personalize but may not define authoritative tests or award completion.
- Reviewed curriculum and scenario manifests are Core-owned, versioned artifacts.
- Public lesson projections exclude answer keys, hidden cases, defect metadata, and solution code.
- Track content may be previewed while execution remains disabled pending an isolated runner.
- No seed phrase, spending key, wallet custody, or mainnet spending enters the product.

## Consequences

CKB, Fiber, JoyID, rewards, and wallet authentication will be removed instead of renamed.
The replacement data model starts fresh rather than merging wallet-address identities.
Completion requires reproducible server evidence bound to scenario and runner versions.
Adding another ecosystem later requires a new adapter and reviewed content, not changes to learner identity.
