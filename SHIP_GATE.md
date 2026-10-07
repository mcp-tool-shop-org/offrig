# Ship Gate

> No repo is "done" until every applicable line is checked.
> Copy this into your repo root. Check items off per-release.

**Tags:** `[all]` every repo · `[npm]` `[pypi]` `[vsix]` `[desktop]` `[container]` published artifacts · `[mcp]` MCP servers · `[cli]` CLI tools

---

## A. Security Baseline

- [ ] `[all]` SECURITY.md exists (report email, supported versions, response timeline) — executed by `npx @mcptoolshop/shipcheck security-docs` (A1: present + reporting contact, not an empty stub)
- [ ] `[all]` README includes threat model paragraph (data touched, data NOT touched, permissions required) — executed by `npx @mcptoolshop/shipcheck security-docs` (A2: trust/threat-model section present + non-empty; *quality* is not machine-checkable)
- [ ] `[all]` No secrets, tokens, or credentials in source or diagnostics output — executed by `npx @mcptoolshop/shipcheck secrets` (scans every publishable tarball; matches redacted; not a manual attestation)
- [ ] `[all]` No telemetry by default — state it explicitly even if obvious

### Default safety posture

- [ ] `[cli|mcp|desktop]` Dangerous actions (kill, delete, restart) require explicit `--allow-*` flag
- [ ] `[cli|mcp|desktop]` File operations constrained to known directories
- [ ] `[mcp]` Network egress off by default
- [ ] `[mcp]` Stack traces never exposed — structured error results only

## B. Error Handling

- [ ] `[all]` Errors follow the Structured Error Shape: `code`, `message`, `hint`, `cause?`, `retryable?`
- [ ] `[cli]` Exit codes: 0 ok · 1 user error · 2 runtime error · 3 partial success
- [ ] `[cli]` No raw stack traces without `--debug`
- [ ] `[mcp]` Tool errors return structured results — server never crashes on bad input
- [ ] `[mcp]` State/config corruption degrades gracefully (stale data over crash)
- [ ] `[desktop]` Errors shown as user-friendly messages — no raw exceptions in UI
- [ ] `[vscode]` Errors surface via VS Code notification API — no silent failures

## C. Operator Docs

- [ ] `[all]` README is current: what it does, install, usage, supported platforms + runtime versions
- [ ] `[all]` CHANGELOG.md (Keep a Changelog format)
- [ ] `[all]` LICENSE file present and repo states support status
- [ ] `[cli]` `--help` output accurate for all commands and flags
- [ ] `[cli|mcp|desktop]` Logging levels defined: silent / normal / verbose / debug — secrets redacted at all levels
- [ ] `[mcp]` All tools documented with description + parameters
- [ ] `[complex]` HANDBOOK.md: daily ops, warn/critical response, recovery procedures

## D. Shipping Hygiene

- [ ] `[all]` `verify` script exists (test + build + smoke in one command)
- [ ] `[all]` Version in manifest matches git tag — executed by `npx @mcptoolshop/shipcheck manifest` (D2: manifest version not behind the newest released tag; `--expect <ver>` for a strict release-time match)
- [ ] `[all]` Dependency scanning runs in CI (ecosystem-appropriate) — executed by `npx @mcptoolshop/shipcheck ci` (D3: a recognized scanner is *configured* in CI, or dependabot is present)
- [ ] `[all]` No known high/critical vulnerabilities in any dependency tree, and Dependabot alerts are enabled — executed by `npx @mcptoolshop/shipcheck deps` (the OUTCOME: audits **every** tree incl. subtrees, not just the root; `ci` only proves a scanner is configured)
- [ ] `[all]` Automated dependency **update** mechanism exists <!-- soft/optional: the org rule restricts the auto-PR bot (CI minutes), NOT alerts. The security outcome is enforced by `shipcheck deps`; the update bot is optional. -->
- [ ] `[npm]` Published via OIDC trusted publishing with `--provenance` — executed by `npx @mcptoolshop/shipcheck ci` (config/intent; `--registry <pkg>` also confirms the attestation on npm)
- [ ] `[npm]` **Every publishable package** passes `npx @mcptoolshop/shipcheck pack` — `npm pack --dry-run` on each workspace package includes README.md + LICENSE and all `files[]` entries resolve (executed check, not a manual attestation; in a monorepo it verifies all packages, not just the root)
- [ ] `[npm]` `engines.node` set · `[pypi]` `python_requires` set — executed by `npx @mcptoolshop/shipcheck manifest` (D6, checked per publishable package)
- [ ] `[npm]` Lockfile committed · `[pypi]` Clean wheel + sdist build — lockfile executed by `npx @mcptoolshop/shipcheck manifest` (D7); the pypi wheel/sdist build is not yet executed
- [ ] `[vsix]` `vsce package` produces clean .vsix with correct metadata
- [ ] `[desktop]` Installer/package builds and runs on stated platforms

## E. Identity (soft gate — does not block ship)

- [ ] `[all]` Logo in README header
- [ ] `[all]` Translations (polyglot-mcp, 8 languages)
- [ ] `[org]` Landing page (@mcptoolshop/site-theme)
- [ ] `[all]` GitHub repo metadata: description, homepage, topics

---

## Gate Rules

**Hard gate (A–D):** Must pass before any version is tagged or published.
If a section doesn't apply, mark `SKIP:` with justification — don't leave it unchecked.

**Soft gate (E):** Should be done. Product ships without it, but isn't "whole."

**Executed vs attested.** `shipcheck audit` only *counts these checkboxes* — it does not read your repo, so a box can be green while the fact is false. The lines that say **"executed by `npx @mcptoolshop/shipcheck <gate>`"** are backed by a command that reads the real artifact and exits 1 on the real defect. Run those gates (they are wired into shipcheck's own `verify`); don't just tick their boxes. Executed today: **A1/A2** (`security-docs`), **A3** (`secrets`), **D2/D6/D7** (`manifest`), **D3-config + OIDC/provenance** (`ci`), **real vulnerabilities + alerting** (`deps`), **D5** (`pack`), plus front-door (`front-door`) and dogfood freshness (`dogfood`). Every other line is still an attestation you are vouching for. Note the two dependency layers: `ci` proves a scanner is *configured*; `deps` proves there are *no known vulnerabilities* — a repo can pass the first while failing the second.

**Checking off:**
```
- [x] `[all]` SECURITY.md exists (2026-02-27)
```

**Skipping:**
```
- [ ] `[pypi]` SKIP: not a Python project
```
