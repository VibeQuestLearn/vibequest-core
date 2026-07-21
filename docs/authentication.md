# Core Assertion Verification

## Contract

VibeQuestLearn Web is the only identity issuer. Core does not process Google OAuth tokens and does not accept browser identity headers. Protected handlers receive `AuthenticatedPrincipal` only after middleware validation.

Required assertion claims are `iss`, `aud`, `sub`, `provider`, `provider_sub`, `iat`, `exp`, and `jti`. The header must specify `HS256`, `JWT`, and a known `kid`. Assertions live for 60 seconds; Core rejects lifetimes over 120 seconds and permits five seconds of clock skew.

Verified assertion IDs are stored in a shared, bounded in-process replay cache until expiry. A second use is rejected, and an unavailable or full cache fails closed.

Core independently derives `sub` as `usr_` plus the first 32 base64url characters of `HMAC-SHA256(IDENTITY_DERIVATION_SECRET, "google:" + provider_sub)`. The derivation key is not an assertion signing key.

## Rotation

Core accepts every comma-separated entry in `CORE_ASSERTION_KEYS`; Web signs with the first. Deploy Core with both old and new keys before switching Web to the new first key. Remove the old key only after deployment overlap and the maximum assertion lifetime have elapsed.

## Failure Behavior

Missing or invalid bearer assertions return the same `401 unauthorized` response without validation details. Missing or invalid server configuration returns `503 identity-unavailable`. Persistence failures return `503 identity-store-unavailable`. All error responses are marked `no-store`.

## Ownership

Profile upsert is keyed by provider plus provider subject and stores the opaque ID as `_id`. Export and deletion accept no owner parameter; they filter users, learning sessions, submissions, and completion receipts using only the verified principal. Shared scenario definitions are outside account deletion.

Legacy wallet and quest handlers are deliberately absent from `build_router`, so their wallet-address ownership model cannot bypass the v3 identity boundary.
