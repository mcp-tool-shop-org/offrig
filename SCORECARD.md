# Scorecard

**Repo:** offrig
**Date:** 2026-10-11
**Type tags:** `[all]` `[cli]` `[mcp]` `[desktop]` `[complex]`

## Pre-remediation assessment

| Category | Score | Notes |
|----------|-------|-------|
| A. Security | 9/10 | SECURITY.md and a full threat model already present; secrets redaction was not tested at every output level |
| B. Error handling | 5/10 | MCP errors were results with a next action but no stable code; the CLI exited 1 for everything; bad MCP arguments surfaced as protocol errors |
| C. Operator docs | 7/10 | A thorough README and design doc; no logging levels, no handbook |
| D. Shipping hygiene | 5/10 | No verify script, no release workflow or binaries, no dependency scanner shipcheck recognises (cargo deny only), no coverage |
| E. Identity (soft) | 2/10 | Description and topics only: no logo, landing page, homepage or translations |
| **Overall** | **28/50** | |

## Key gaps

1. MCP tool errors had no machine-readable `code` or `retryable`.
2. CLI exit codes didn't separate user errors from runtime failures.
3. No downloadable release: users had to build from source.
4. No logging levels, and no test that the API key never appears in output.
5. No logo, landing page or handbook.

## Remediation (offrig#20, offrig#21 and the full-treatment PR)

1. Structured MCP errors with `code` and `retryable`; CLI exit codes 0/1/2; `-q`, `-v`, `--debug` with redaction tests.
2. `scripts/verify.sh` and `verify.ps1`; an OSV scan and coverage in CI.
3. A tag-triggered workflow that builds the Windows zip with `SHA256SUMS` into a draft release.
4. The app's icon, a brand logo, the landing page, a seven-page handbook, and repo homepage and topics.

## Post-remediation

| Category | Score |
|----------|-------|
| A. Security | 10/10 |
| B. Error handling | 10/10 |
| C. Operator docs | 10/10 |
| D. Shipping hygiene | 10/10 |
| E. Identity (soft) | 10/10 |
| **Overall** | **50/50** |

Re-checked 2026-10-11 for 1.1.0. `shipcheck audit` counted 29 checked, 0 unchecked, 10 skipped. The version in the workspace is 1.1.0. The newest released tag is still v1.0.0, so the manifest is ahead of the tag and not behind it. Section E stays checked from the 1.0.0 treatment: logo, translations, landing page, and metadata were already in place. Translations of the 1.1.0 README are the Publisher's, after review.
