# ADR 0001: Platform And Ecosystem Boundaries

Status: Accepted  
Date: 2026-07-21

## Context

The inherited application coupled learner identity, persistence, progression, rewards, and interface copy to one ecosystem-specific stack. VibeQuestLearn must not replace that with a different ecosystem-native architecture.

The platform has to support CKB, Fiber, Zcash, and future ecosystems without changing the account model, dashboard model, lesson model, or completion model each time a new track is added.

## Decision

- Shared platform types use opaque user, ecosystem, track, lesson, scenario, and submission identifiers.
- Google OAuth authenticates the learning account.
- The server establishes authorization; browser identity headers and completion booleans are untrusted.
- Ecosystem protocol behavior lives only in explicit adapters and reviewed track packages.
- Current Zcash shielded-payment behavior is one track package, not the platform identity.
- AI may explain and personalize but may not define authoritative tests or award completion.
- No seed phrase, spending key, wallet custody, or mainnet spending enters the shared product boundary.

## Consequences

CKB, Fiber, Zcash, and future ecosystems must all use the same learner identity, persistence, catalog, curriculum, tutor, submission, and evidence boundaries.

Track-specific docs may use ecosystem language. Main product architecture, design docs, account docs, and dashboard docs must stay ecosystem-neutral.

Adding another ecosystem later requires a new adapter and reviewed content package, not changes to learner identity or global product structure.
