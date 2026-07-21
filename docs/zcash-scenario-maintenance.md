# Zcash Curriculum And Scenario Maintenance

The shielded-checkout track is a reviewed artifact graph:

`source-manifest.json` -> `curriculum-manifest.json` -> `scenario-manifest.json` -> starter, solution, and test specifications.

Core validates that graph before returning the public curriculum. A change is incomplete when any version, source reference, lesson mapping, or case ID is inconsistent.

## Updating A Protocol Claim

1. Verify the claim against an official Zcash ZIP, protocol specification, official crate, or official Zcash guidance.
2. Add or update the source manifest entry with its exact version, retrieval date, license, and supported scope.
3. Reference the source ID from the affected lesson.
4. Add a valid case or denial case that demonstrates the claim through the Core domain engine.
5. Update the content and scenario versions together when behavior changes.

Do not add a protocol claim only to prose. Every trusted claim must be traceable to a source entry and an executable case.

## Updating A Lesson

Keep exactly five lessons in the reviewed track. Each lesson requires:

- one explicit learner outcome;
- reviewed explanatory paragraphs;
- one exact snippet from the scenario solution;
- at least one official source;
- one misconception;
- one deterministic four-option checkpoint and internal answer key;
- one bridge to an allowlisted scenario symbol;
- at least one valid case and two denial cases;
- content version and reviewed status.

Checkpoint answer keys stay in Core. The public projection omits the correct option, rationale, denial case IDs, seeded defects, and hidden test data.

## Updating The Scenario

The starter and solution must retain the same exported types and function signatures. A seeded defect is allowed only when its file and symbol appear in `allowlisted_edit_locations`. Add the defect marker to the starter, map it to one lesson, and add it to the capstone repair list.

Public tests explain the visible contract. Hidden tests probe adjacent behavior and must never depend on a secret, production address, network service, clock, or nondeterministic state. The runner introduced in Chunk 05 owns test isolation and prevents solution access.

## Tutor Contract

AI may return explanation, hint, or remediation artifacts only. The request is bounded to:

- one reviewed lesson ID;
- the exact scenario manifest version;
- the selected tutor mode;
- a bounded learner prompt;
- bounded actual test output.

Every response must match the lesson and scenario version, cite only that lesson's reviewed sources, and contain lesson-specific anchors. Unsupported citations and generic responses are rejected rather than cached. Cache keys hash the bounded context; raw prompts and test output are not stored in the artifact cache.

When the provider is unavailable, Core returns a reviewed fallback assembled from the lesson outcome, explainer, code symbol, anchors, sources, and checkpoint. Tests and completion never depend on AI output.

## Review Gate

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo deny check
```

Then inspect the public curriculum JSON and confirm it contains no answer keys, hidden case IDs, solution body, sensitive fixture input, or custody instructions.
