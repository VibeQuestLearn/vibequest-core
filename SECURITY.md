# Security

## Reporting

Do not open a public issue for a vulnerability that could expose accounts, secrets, learner data, or runner isolation.
Report it privately to the repository owners with affected versions, impact, reproduction steps, and a minimal proof.
Do not include real credentials, wallet material, or personal data in the report.

## Sensitive Data

Never commit or place in fixtures:
- `.env` files or service credentials
- Google client secrets, access tokens, refresh tokens, or session secrets
- Zcash seed phrases, spending keys, or production viewing keys
- Real learner source, addresses, memos, or payment requests

`.env*` is ignored except for reviewed example files containing placeholders.

## Supported Branch

Security fixes for this development phase target `zcashlearn`.

## Lab Boundary

Browser results are untrusted until verified by the server.
Never request seed phrases or spending authority from a learner.
Executable labs must run without network access, host secrets, or cross-job filesystem access.
