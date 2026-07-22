# Runner Adversarial Fixtures

These fixtures exercise only the reviewed shielded-checkout runner boundary.

- `timeout.ts` exhausts the learner VM evaluation window.
- `output-flood.ts` attempts to exceed combined worker output.
- `process-access.ts` attempts host process access.
- `filesystem-import.ts` attempts a static filesystem import and host file read.
- `dynamic-import.ts` attempts a runtime import.
- `network-access.ts` attempts an outbound request.
- `host-global-check.ts` is prepended to the reviewed solution and passes only when process, network, and CommonJS globals are unavailable.

The fixtures contain sentinel values so the acceptance test can prove that structured runner evidence does not echo learner source or raw diagnostics.
