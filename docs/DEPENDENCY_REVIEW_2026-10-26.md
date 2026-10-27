# Dependency and tooling review — 2026-10-26

Status: coordinated refresh in progress. No new PR or merge has been published
yet. Dependabot #243 is closed because Alloy 2 is incompatible with the current
Autonomi/evmlib provider types. The remaining updates will be consolidated into
a replacement PR; upstream security findings still prevent declaring it ready.

## Updated versions

| Component | Previous main | Refreshed version |
| --- | --- | --- |
| Rust | 1.98.1 | 1.99.1 |
| Rust builder image | 1.98.0 | digest-pinned 1.99.0, explicitly installing 1.99.1 |
| Node | 24.20.0 | 24.22.1 |
| ant-core | 0.8.1 | 0.14.0 |
| ant-protocol | 2.3.5 | 5.0.1 |
| saorsa-core / transport | 0.27.3 / 0.36.3 | 0.29.2 / 0.38.2 |
| evmlib | 0.9.1 | 0.10.0 |
| self_encryption | vendored 0.36.0 | verified vendored 0.36.2, bounds preserved |
| ant-node / devnet | 0.18.1 | 0.23.1 |
| ant-sdk / upstream antd | 0.12.1 | 0.17.0 |
| ant CLI | 0.3.6 | 0.3.12 |
| Foundry | 1.8.1 | 1.8.6, release SHA-256 pinned for both Linux architectures |
| Tauri / tauri-build | 2.11.5 / 2.6.3 | 2.12.5 / 2.7.4 |
| Tauri CLI / JavaScript API | 2.11.4 / 2.11.1 | 2.12.4 / 2.12.4 |
| React / React DOM | 19.2.8 | 19.3.1, upgraded together |
| hls.js | 1.7.2 | 1.7.3 |
| Vite | 8.2.2 | 8.3.5 |
| Vitest / coverage | 5.0.0 | 5.0.5 |
| Playwright | 1.63.0 | 1.64.1 |
| React Router | 7.18.3 | 7.18.4 |
| Reqwest | 0.13.4 | 0.13.5 |
| jsonwebtoken | 11.0.0 | 11.1.0 |
| Prometheus | 3.14.0 | 3.15.1 |
| Alertmanager | 0.34.0 | 0.34.1 |
| Grafana | 13.2.1 | 13.2.4 |
| Loki | 3.7.7 | 3.7.9 |
| Grafana Alloy | 1.19.2 | 1.21.2 |

Both Rust graphs and npm lockfiles were regenerated against live registries.
Compatible updates include Rustls 0.23.46, rustls-webpki 0.103.16, Tokio 1.54.1,
webpki-root-certs 1.0.11, UUID 1.28.0, and serde_json 1.0.154. The upstream
unsafe-libyaml graph and obsolete UNIC crates are gone. No vulnerability
exception was added.

The desktop invokes `open::that_detached` directly after the existing strict
loopback-URL validation. This is the same browser-opening implementation the
Tauri opener wrapper used. Removing that otherwise unused plugin and its
capability removes zbus 5 and the three unmaintained zvariant/zbus_names crates.
The existing custom launcher-command permission remains enforced.

The newer compiler deprecates Atomic::fetch_update; its test fixture now uses
the equivalent try_update API. No warning suppression was added.

## Compatibility choices

- Keep Alloy 1: evmlib exposes its provider types. The standalone Alloy 2 PR
  fails compilation and does not supply a coordinated upstream migration.
- Keep Node 24 and its typings. Node 26 is a separate coordinated migration.
- Keep ESLint 9: the current eslint-plugin-react peer range excludes ESLint 10.
- Keep TypeScript 6: current typescript-eslint requires TypeScript below 6.1.
- Node exporter 1.12.1 is already the latest stable upstream release.
- Dependabot groups React/React DOM/types and Tauri crates. Its PRs still need
  manual alignment of deploy/versions.json and cross-ecosystem Tauri versions;
  grouping does not automatically update runtime metadata.

## Autonomi provenance and decompression

The [SDK 0.17.0 release](https://github.com/WithAutonomi/ant-sdk/releases/tag/v0.17.0)
pairs ant-core 0.14.0, node 0.23.1, CLI 0.3.12 and protocol 5.0.1.
The application retains its custom gateway and payment-budget journal; upstream
SDK tooling remains separate. The coordinated update compiles without changing
application client/payment APIs.

self_encryption 0.36.2 was downloaded from crates.io and checked against its
published SHA-256. Its new DataMapIndex bounds decompression by declared size,
but public decrypt_chunk still passes usize::MAX. The downstream bounded writer
therefore remains necessary: it enforces min(declared size, MAX_CHUNK_SIZE)
before extending a buffer on both paths. Source formats and valid addresses are
unchanged. See [vendor provenance](../vendor/self_encryption/AUTVID_PATCH.md).
Upstream now supplies MIT/Apache license files and notices; those are retained.

## Verification

Passed on the refreshed versions, using Rust 1.99.1 and Node 24.22.1:

- Locked Autonomi gateway cargo check.
- Full workspace Rust suite: 110 tests passed, no skips.
- Frontend lint, production build, formatting: passed.
- Frontend tests: 42 Vitest tests and 7 Node tests passed.
- Live full npm audit for the web lockfile: zero reported vulnerabilities.
- Desktop Rust advisory check: passed after removing the unused opener wrapper.
- Runtime version/lock consistency check: passed.
- All eight Compose configuration render paths: passed.
- git diff --check: passed.

Additional checks passed:

- Workspace Clippy with warnings denied, and Rust formatting.
- Admin suite with DB features: 51 unit tests plus 7 integration tests.
- Desktop compilation and 2 launcher URL validation tests. Temporary empty
  sidecars use the existing CI compile-check procedure; this is not packaging.
- Full desktop cargo-deny policy: advisories, licenses, bans and sources pass.
- Root licenses, bans and sources pass; advisory failures remain listed below.
- Runtime consistency plus 3 Python security-tool tests.

The vendor suite and CI/container verification are still pending.
The old modernization record describes earlier validation; its results do not
substitute for validation of this dependency graph.

## Remaining root Rust advisory blockers

A freshly updated advisory database still rejects the current stable upstream
Autonomi dependency graph:

- crypto-bigint 0.5.5 via elliptic-curve 0.13/k256 0.13/Alloy 1:
  RUSTSEC-2026-0387, timing behavior in intended constant-time operations.
  The payment-signing path uses this stack; a compatible upstream migration is
  required. Do not blindly override crypto-bigint to its incompatible 0.7 API.
- zip 2.6.1 via ant-core's native node-management feature:
  RUSTSEC-2026-0367, 0368, 0369. Fixed upstream on the 9.x line.
  The gateway does not invoke node binary installation, and ant-core's helper
  reads entries rather than using the affected extract APIs, but the dependency
  is still present and policy still rejects it.
- zstd 0.13.3 and zstd-safe 7.3.0 via zip's default features:
  RUSTSEC-2026-0391, 0398, 0403. Fixed on zstd >=0.14.3 and zstd-safe >=8.2.0.

These findings were not suppressed. A current version label alone does not mean
the entire transitive graph is free of known advisories.

## Independent review and environment

Claude Code 2.1.310 completed a preliminary read-only review using
`claude-opus-5-5`. It observed intermediate manifest/lock/pin drift while the
refresh was running; those mismatches were subsequently resolved and checked.
A final review of the stable diff is required before merge.

The first review attempt was blocked by automatic approval review because the
CLI contacted Datadog telemetry. The successful retry set the documented
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC, DISABLE_TELEMETRY,
DISABLE_ERROR_REPORTING, and DO_NOT_TRACK controls. No telemetry exception was
requested and no security review was bypassed.

GitHub CLI now authenticates with repository write scopes, and registry and
Anthropic network access work. Local .git remains read-only, so publishing must
use the authenticated GitHub API. OrbStack is stopped; starting it from this
sandbox fails on a log-directory write outside the repository. Container smoke,
restart-recovery and large-original tests have not yet run for this refresh.

## Dependabot disposition

Reviewed PRs: #245, #244, #243, #242, #241, #239, #238, #237, #236, #234,
#233, #229, #226, #220, #219, #218, #217, #216, #214, #213, #212.

- #243 is closed with the Alloy provider incompatibility documented.
- #236 and #219 should close because their standalone Node 26 upgrades conflict
  with the supported Node 24 runtime.
- #234 should close because the Tauri opener plugin is removed.
- The other updates are included or superseded by newer versions in this
  consolidated refresh, including Vite 8.3.5 in place of vulnerable 8.3.0.
  Link their closures to the replacement PR so pending work remains traceable.

No existing application data, Docker volumes or paid-network transactions were
modified by this review.
