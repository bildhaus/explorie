# Integration validation — Windows x64, 2026-09-05

## Cutover preparation — 2026-09-09

Filesystem watcher bursts previously canceled every unfinished listing and discarded its
generation, allowing active folders to remain loading indefinitely. Watcher refreshes now
coalesce until the current List/Grid/Column listings finish, including failed ancestors;
one follow-up refresh incorporates the changes. Existing rows remain visible during refresh.
Smart-folder invalidation still restarts its canceled search immediately, and explicit user
cancellation remains final. The native regression injects event bursts and delayed/stale
results without depending on filesystem timing. A modal input regression also ensures archive
controls cannot click through to the visible file rows underneath.

Failed migration from a downloaded integration to a missing/corrupt bundled package now blocks
the previous executable, marks the integration unavailable, and preserves preferences for
repair. The regression starts the old executable, verifies it stops and cannot restart after
failed bundle verification, then repairs the bundle and verifies activation succeeds.

The packaged integration navigation fixture is now required by both Windows and macOS CI,
alongside the native manager's offline activation smoke. Release-proof validation requires
bundled integration activation and remote-drive lifecycle checks on both platforms. Its only
updater exemption is explicit, documented `bildhaus/explorie` first-release proof at `0.0.1`.

These corrections address demonstrated failure paths. They do not establish the cause of the
earlier isolated `STATUS_STACK_OVERFLOW` or replace real-machine candidate attestations. Final
verification logs and the cutover source SHA are kept under ignored `.release-checks/`.

## Bundled integration update

The three official integrations now ship as catalog-verified ZIPs inside the app. Native
runtime tests cover offline registration and activation, disabled defaults, migration from
downloaded integrations, preserved preferences across updates/restarts, corrupted-cache
repair, and unavailable bundles refusing a download fallback. Native GPUI tests verify
enablement controls and separate detection rows without install/uninstall controls.

Validation for this update: 429 workspace tests passed (8 ignored), followed by the focused
16-test runtime suite for the final missing-bundle fallback correction. Strict workspace
Clippy, formatting, 31 packaging checks, and the real three-plugin offline activation smoke
passed. The Windows release executable and local installer built successfully.

The final desktop launch/temporary-profile cleanup smoke command was rejected by automatic
approval review with “blocked by policy”; it did not run. This update has not been executed
on macOS locally. Both platform CI/release lanes now require execution of the exact bundled
ZIPs through the native manager, in addition to existing app/signing checks. No new release
or version bump was made for this source change.

## Loading-files investigation

The user reported that the installed Windows app remained responsive but folder listings
stayed on “Loading files” with Git enabled, and disabling Git restored navigation. They later
reported normal navigation with Git enabled again. The intermittent installed-app failure
has not been reproduced in the native test harness; no production fix or new installer is
claimed.

An isolated GPUI test loads the actual three bundled release ZIPs, enables them, starts native
service events and filesystem watching, and awaits column listings and integration results.
The initial run including the machine's Downloads and Program Files folders passed in 8.12s.
All columns finished loading and all three integration scans completed. This checks the
headless native lifecycle, not real-window activation or the installed application.

The retained ignored test uses a temporary Git repository and Obsidian vault by default and
requires successful integration results and a detected Git repository. An extra real folder
can be supplied explicitly without changing the user's configuration:

```powershell
$env:EXPLORIE_PLUGIN_SMOKE_CATALOG = Join-Path $PWD 'release-artifacts/plugins-x86_64-pc-windows-msvc/explorie-plugin-catalog-x86_64-pc-windows-msvc.json'
$env:EXPLORIE_PLUGIN_SMOKE_DIRECTORY = Join-Path $PWD 'target/release/plugins'
# Optional: $env:EXPLORIE_PLUGIN_NAVIGATION_PATH = 'C:\path\to\investigate'
cargo test --locked -p explorie-gpui bundled_integrations_do_not_stall_column_navigation -- --ignored --nocapture
```

The strengthened test, including the Git fixture and opt-in Downloads navigation, passed
in 17.58s. Its first run aborted in an unnamed native thread with `STATUS_STACK_OVERFLOW`;
the unchanged rerun passed. That isolated test-process failure remains unexplained and is
not evidence that the intermittent installed-app problem is resolved.

## Original plugin implementation

Local implementation evidence; this is not a release attestation. No release was published
and no installed application or user integration configuration was modified for these checks.

| Check | Evidence |
| --- | --- |
| Formatting | `cargo fmt --all -- --check` and `git diff --check` passed. |
| Strict lint | `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` passed. |
| Workspace tests | `cargo test --locked --workspace --no-fail-fast -- --test-threads=1` passed: 410 tests, 7 explicitly ignored fixtures/benchmarks/platform checks. |
| Final provider change | Git and Obsidian tests passed after correcting vault URIs, including the additional vault-action regression test. |
| Native UI | Onboarding/settings defaults, overlapping badges and decorations, navigation/window isolation, native badge clicks, and List/Grid/Column layouts passed. |
| Runtime | Real executable fixtures exercised crashes, hangs, malformed/oversized output, status notifications, shared state and cleanup. Package tests covered integrity, compatibility, extraction paths, failed updates and developer provenance. |
| Syncthing | Local HTTP fixtures covered authentication/offline states and event reconciliation. Real localhost TLS handshakes verified the configured certificate and rejected a different certificate before sending the API key. |
| Packaging scripts | All 28 tests in `package-plugins.test.mjs`, `release-check.test.mjs` and `prepare-7zip.test.mjs` passed. |
| Windows packages | All three release executables were packaged and installed through the native manager's verified extraction/registry path, then executed against a folder containing all three integrations. |

Reproduce the real-package runtime check after `pnpm desktop:build`:

```powershell
$env:EXPLORIE_PLUGIN_SMOKE_CATALOG = Join-Path $PWD 'release-artifacts/plugins-x86_64-pc-windows-msvc/explorie-plugin-catalog-x86_64-pc-windows-msvc.json'
cargo test --locked -p explorie-native-services plugins::tests::official_packages_install_and_execute_through_native_manager -- --ignored --exact --nocapture
```

This test reads local release ZIP bytes instead of performing an HTTPS download; it uses
the production hash, manifest, extraction, atomic installation and executable process paths.
It does not establish that the unpublished GitHub URLs are available.

The release build embeds the corresponding catalog. The Windows installer is produced with
`scripts/package-gpui-windows.ps1` and that same `EXPLORIE_PLUGIN_CATALOG`, including catalog
verification and the bundled 7-Zip smoke check. Local artifacts are under
`release-artifacts/plugins-x86_64-pc-windows-msvc/` and `release-artifacts/windows/`.

Remaining release evidence:

- Exercise the actual HTTPS download and install/enable/update/uninstall flow in the packaged
  Windows app once the release assets are accessible. The local package test does not replace
  this check. Interactive installed-app testing was not performed in this session.
- Obtain equivalent macOS arm64 execution evidence for the signed/notarized packages and app.
  Windows compilation and macOS packaging-script tests do not establish macOS readiness.
- Run the existing signing, release smoke and publication gates against the final release
  candidate. Publication remains outside this implementation.

The GPUI suite was run serially because parallel native operation tests were timing-sensitive
under concurrent compiler load. The drag/drop fixture now minimizes its floating operation
panel before clicking a row that onboarding can otherwise position beneath that panel.
