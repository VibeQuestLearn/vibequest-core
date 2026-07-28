# Open AI Learning Engine — Core Task Plan

Status: proposed implementation plan  
Scope owner: vibequest-core  
Primary goal: harden VibeQuest's AI learning engine without changing the existing learner flow.

## Product Guardrail

The current VibeQuest flow must remain intact:

`choose ecosystem -> set learning intent -> generate course/modules -> learn -> checkpoint/quest -> dashboard progress`

The open-AI work adds a quality and portability layer around generation. It must not remove the current OpenAI-compatible production provider, force local inference, or narrow VibeQuest into a single ecosystem.

## Why This Exists

For Sentient-aligned work, VibeQuest needs to become more than a hosted AI course generator. The core system should be able to prove that generated education is grounded, useful, non-repetitive, privacy-aware, and portable across open-model providers.

## Task 1 — Provider Abstraction

Create a backend model-provider boundary so generation is not coupled to one hosted endpoint.

Implementation tasks:

- Define a provider interface for lesson generation, tutor responses, and validation calls.
- Keep the existing OpenAI-compatible configuration working as the default provider.
- Add provider metadata that can be exposed safely: provider kind, model name, timeout, reasoning setting, storage-disabled flag.
- Redact secrets from all logs, errors, reports, and exports.
- Add configuration support for future open/local OpenAI-compatible endpoints without changing route handlers.

Acceptance criteria:

- Existing environment variables continue to work unchanged.
- A second provider can be configured through environment variables without touching lesson-generation business logic.
- Tests prove provider metadata never includes API keys or raw authorization headers.

## Task 2 — Source-Grounded Validation Pipeline

Add a validation stage after lesson/module generation.

Validation dimensions:

- Source coverage: generated claims must map to official ecosystem references where possible.
- Citation consistency: resource links should match the ecosystem and lesson topic.
- Repetition score: repeated module names, repeated paragraphs, and repeated concepts should be flagged.
- Lesson depth: lessons should meet minimum substance requirements without padded filler.
- Checkpoint quality: checkpoint questions should test protocol reasoning, not memorization.
- Unsafe confidence: unsupported technical claims should be flagged for review instead of presented as authoritative.

Acceptance criteria:

- Generation can return both lesson content and a validation report.
- Validation failure does not crash the product; it marks the lesson/module for retry or review.
- Reports include actionable reasons, not generic "quality failed" messages.

## Task 3 — Public Eval Artifact Schema

Create an exportable artifact that makes AI-generated education auditable.

Artifact fields:

- ecosystem id and topic
- learning profile and intent
- source manifest ids used
- prompt version or prompt hash
- provider metadata with secrets removed
- generated module/lesson ids
- validation scores and warnings
- checkpoint summary
- resource/citation coverage
- generated timestamp

Acceptance criteria:

- A JSON artifact can be produced for a generated course or official track.
- Artifacts contain enough information to review quality without exposing secrets or private user data.
- Snapshot tests cover artifact shape for at least one Zcash and one Stacks course fixture.

## Task 4 — Official Source Registry

Strengthen the registry of ecosystem references used by generation and validation.

Implementation tasks:

- Store source manifests per ecosystem: CKB, Fiber, Zcash, Stacks, and Web3/blockchain basics.
- Mark sources by type: official docs, protocol spec, repository, standards document, ecosystem guide.
- Add stable source ids so validation reports can reference sources without duplicating long URLs everywhere.
- Prefer official ecosystem resources for primary grounding.

Acceptance criteria:

- Every ecosystem has a source manifest.
- Generated courses can cite source ids from the manifest.
- Validation can flag a lesson that uses unrelated ecosystem references.

## Task 5 — Privacy-Safe Tutor and Generation Logs

Make tutor and generation behavior safer by default.

Implementation tasks:

- Minimize stored learner text and tutor chat payloads.
- Redact emails, API keys, wallet addresses, and obvious secrets from logs and reports.
- Separate operational telemetry from lesson content.
- Add a retention flag/config for tutor sessions.
- Document what is stored, what is not stored, and what is exported.

Acceptance criteria:

- Tutor logs do not expose account emails or generated private context in server logs.
- Exported eval artifacts do not include raw user identity fields.
- Privacy behavior is documented in the core docs.

## Task 6 — Eval Fixtures and Regression Tests

Create practical tests that prevent shallow AI output from silently regressing.

Implementation tasks:

- Add fixture courses for at least Zcash, Stacks, and Web3/blockchain basics.
- Add tests for repeated title detection, repeated paragraph detection, source mismatch, missing checkpoint, and unsupported claim warnings.
- Add tests for artifact redaction.
- Add a small command or test helper that can run evals locally without calling a live model.

Acceptance criteria:

- Core tests can run offline against fixtures.
- A generated fixture with obvious repetition fails validation.
- A generated fixture with missing sources produces clear warnings.

## Task 7 — Feature Flags and Rollout

Ship the open-AI/eval layer incrementally.

Implementation tasks:

- Add feature flags for validation, public artifact export, and alternate providers.
- Keep validation report generation non-blocking at first.
- Add a stricter mode for official tracks or public grant/demo tracks.

Acceptance criteria:

- Production generation keeps working if validation is disabled.
- Validation can be enabled without changing frontend routes.
- Strict validation can be applied to selected tracks later.

## Non-Goals for This Phase

- Do not build a full local inference runtime inside core.
- Do not remove the current OpenAI-compatible provider.
- Do not rebuild the entire curriculum system.
- Do not make VibeQuest blockchain-only or Sentient-only.
- Do not expose raw prompts, secrets, or user private data in public artifacts.

## Implementation Order

1. Provider abstraction while preserving current config.
2. Source registry and manifest ids.
3. Validation report schema.
4. Offline eval fixtures and tests.
5. Public artifact export.
6. Privacy/tutor logging hardening.
7. Feature flags and strict-mode rollout.
