# Zcash Protocol Dependency Decision

Date: 2026-07-21
Decision: accepted for verifier `1.0.0`
Source manifest: `zcash-sources-2026-07-21.1`

## Selected Stable Set

| Crate | Exact version | Features | License | librustzcash revision |
| --- | --- | --- | --- | --- |
| `zcash_address` | `0.12.0` | default `std` | MIT OR Apache-2.0 | `d47691c6b620e9c1fa3574a5a63deb4da544da2e` |
| `zcash_protocol` | `0.9.0` | default `std` | MIT OR Apache-2.0 | `d47691c6b620e9c1fa3574a5a63deb4da544da2e` |
| `zip321` | `0.8.0` | no optional features | MIT OR Apache-2.0 | `d47691c6b620e9c1fa3574a5a63deb4da544da2e` |

`url 2.5.8` is an already-resolved dependency promoted to direct use only for bounded query-key classification after the official ZIP-321 parser rejects a request. `proptest 1.6.0` is test-only.

## Compatibility Decision

The newest stable `zcash_address 0.13.0` and `zcash_protocol 0.10.0` do not form one dependency graph with the newest stable `zip321 0.8.0`, which depends on the selected `0.12` and `0.9` lines. `zip321 0.9.0-rc.1` and `zcash_client_backend 0.24.0-rc.1` are prereleases. The verifier therefore pins the latest cohesive all-stable set from one upstream revision instead of compiling duplicate protocol versions or adopting an RC.

## Scope

The selected crates provide exactly the required primitives:

- canonical Zcash and Unified Address parsing;
- mainnet/testnet discrimination;
- Orchard, Sapling, transparent, and unknown receiver inspection;
- bounded `Zatoshis` values;
- ZIP-321 indexed payment parsing, memo constraints, required-parameter rejection, and canonical serialization;
- Unified Full and Incoming Viewing Key decoding without spending authority.

The canonical ZIP 316 is currently Active at Revision 2. The stable crate set implements Revision 0 `u`, `utest`, and `uregtest` Unified encodings and does not implement Revision 2 `zu`/`tu` metadata or expiry items. Those encodings are explicitly outside verifier `1.0.0`; the source manifest records this rather than silently claiming full current-ZIP support.

## Excluded Backend

`zcash_client_backend 0.23.0` is stable but intentionally excluded. It introduces light-client, synchronization, transaction construction, optional Orchard proving, transparent key import, and transport boundaries that this deterministic learning verifier does not need. It should be reconsidered only when a reviewed payment-detection requirement cannot be met by a smaller official crate.

## Compile And Security Impact

- Runtime lockfile additions: 18 packages for address encoding, F4Jumble, bounded values, and ZIP-321 parsing.
- Test-only lockfile additions: 14 packages for property testing.
- `url` was already present transitively and adds no new package.
- Cold all-target `cargo check` after the additions completed in 38.67 seconds on the local development machine.
- The native release binary changed from 17,012,824 bytes to 16,997,432 bytes (-15,392); the verifier has no binary growth while it remains an internal library boundary that is not yet linked into an HTTP handler.
- `cargo deny check` passes for advisories, bans, licenses, and sources.

No transaction signing, spending-key parsing, node synchronization, network access, or mainnet spend construction is introduced.
