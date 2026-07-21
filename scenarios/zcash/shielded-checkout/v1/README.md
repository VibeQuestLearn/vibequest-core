# Shielded Checkout Scenario v1

This package is the single codebase used by all five lessons in the Zcash shielded-payments track.

## Trust Boundary

The TypeScript scenario consumes structured reports and summaries produced by the reviewed Rust domain engine. Browser-created reports are untrusted. Chunk 05 will execute learner edits in an isolated runner and bind the resulting evidence to this scenario version.

The starter contains exactly five defects:

1. network and shielded-receiver policy;
2. exact ZIP-321 merchant intent;
3. non-spending viewing authority;
4. confirmation, reorg, and replay state;
5. privacy-safe diagnostic fields.

Only the five locations in `fixtures/zcash/v1/scenario-manifest.json` may be edited by a learner. The solution is the source for curriculum code lenses and maintainer review; it is never sent as part of a learner run.

## Tests

`tests/public.spec.ts` is educational and ships with the starter. It names the public case IDs and makes the expected denial behavior visible.

`specs/hidden-tests.json` defines runner-only behavior without containing credentials, spending material, production addresses, or learner-specific data. The authoritative case semantics are also executed against the Rust domain engine during Core tests.

## Safety

- No scenario code signs or broadcasts a transaction.
- No spending key or seed phrase is accepted.
- Google authentication identifies the learning account only.
- Raw addresses, memos, payment requests, viewing keys, and Google profile fields are excluded from diagnostic output.
- All values and fixture inputs are test-only.
