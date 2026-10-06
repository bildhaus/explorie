# Explorie integrations

Git, Obsidian and Syncthing integrations ship with Explorie as opt-in executable plugins.
Settings → Integrations offers **Enable integration** and configuration. All three start
disabled; no plugin process or connection starts until you enable it. Enabling requires
no download, including in draft builds. Their integration versions update with Explorie.
Syncthing, Git and Obsidian can contribute to the same folder. Remote Drives remains
the existing native feature, linked from integration settings.

## Trust and privacy

Plugins run as separate processes with the current user's normal filesystem and network
access. They are **trusted programs, not a sandbox**. Capability descriptions disclose
intended use; they are not enforced permissions. Only install or load code you trust.
The host owns every badge, decoration, detail row and action; plugins cannot supply UI code.

Bundled provenance comes from the catalog embedded in the installed Explorie app,
never from a field a plugin can claim in its manifest. The catalog pins each package's
target, version, exact HTTPS release URL and SHA-256. Third-party public installation
and a marketplace are outside v1. Explicitly loaded local plugins are marked Development.

Settings distinguishes integration enablement from passive detection of an existing Git
executable, Obsidian application/URI handler, or Syncthing configuration. Detection starts
no applications or connections, and configuration presence does not mean a daemon is running.

Enablement is separate from connecting Syncthing. Its plugin can detect
folder markers without connecting. An explicit connection reads the selected/discovered
local Syncthing configuration; Explorie retains its path, not the API key. Git reads local
repository state and does not fetch. Obsidian actions use its URI handler and do not edit
vault configuration. Plugins are not bundled copies of these applications.

Browsing a repository must not run code from it, even an extracted archive. The Git plugin
only runs read-only commands with the pager disabled, `--no-optional-locks` (so `status`
never writes the index and no hooks fire) and a command-line `core.fsmonitor=false` that
outranks repository settings. Filter drivers can't be neutralized that way, so before
`git status` the plugin reads the configuration (reading runs nothing) and refuses to
inspect a repository whose own `.git/config`, `config.worktree` or included files define
`filter.*.clean|smudge|process`, `diff.external`, `diff.*.command|textconv`,
`include.path` or `includeIf.*.path`. Such folders show **Git · Untrusted configuration**
and list the keys. User and system configuration, such as a global Git LFS filter, stays
trusted. This check needs Git 2.26 or newer.

Updates preserve enablement and configuration, including a previous Disable. Bundled
integrations can be disabled; their files are managed with the app. Obsidian vault actions
address the vault by its folder name using the documented
`vault` URI parameter; note actions use an encoded absolute file path. Vaults with identical
folder names can be ambiguous for Open Vault; opening a selected note addresses its path.

## Developer example

The independent example package uses the same public Rust protocol as official plugins.
From the repository root, build it and copy its executable beside its manifest:

```powershell
cargo build --manifest-path plugins/example/Cargo.toml
Copy-Item plugins/example/target/debug/explorie-plugin-example.exe plugins/example/
cargo run -p explorie-gpui -- --load-plugin plugins/example
```

On macOS arm64:

```sh
cargo build --manifest-path plugins/example/Cargo.toml
cp plugins/example/target/debug/explorie-plugin-example plugins/example/
cargo run -p explorie-gpui -- --load-plugin plugins/example
```

The flag takes a local directory containing `plugin.json` and the manifest's executable
for the current target. It does not add the plugin to the official catalog. Use Settings
to enable the loaded integration. Normal debug builds embed an empty download catalog;
the source manifests still describe the available integrations, and developer loading
allows exercising them before release assets exist.

## Protocol version 1

The source of truth for wire types is `crates/plugin-protocol/src/lib.rs`.
Messages are UTF-8 JSON-RPC 2.0 objects, one per newline, flushed after each message.
The frame limit is 16 MiB. stdout is reserved exclusively for the protocol; stderr is
bounded diagnostic output. Never include credentials in logs, errors or contributions.
The host enforces request deadlines and may terminate an unresponsive process.

| Method | Parameters | Result |
| --- | --- | --- |
| `initialize` | `{ "protocolVersion": 1 }` | Manifest |
| `configure` | Plugin-specific JSON configuration | `null` |
| `inspect` | Folder inspection context | Contribution |
| `invoke` | `{ "actionId": "…", "context": … }` | Action effect |
| `shutdown` | None | Exit without response |

Requests carry `id`; responses repeat it and contain either `result` or an `error` object.
A plugin may publish `statusChanged` notifications with a contribution in `params`.
The host can ignore stale notifications and results: always copy the inspection's `contextId`, `path`
and `generation` into the contribution, including asynchronous updates.

An inspection contains a stable per-window `contextId`, `path`, `entries` (`path`, `isDir`), `selected` paths, `generation`
and `force`. A contribution contains `contextId`, `path`, `generation`, optional `root` and `badge`,
`details` (`label`, `value`), `decorations` (`path`, `label`), `actions` (`id`, `label`),
and Unix-seconds `observedAt`. No match is a contribution with no badge or actions.
Action effects are `{ "kind": "none" }`, `{ "kind": "openUrl", "value": "…" }`,
or `{ "kind": "copyText", "value": "…" }`; the host applies them through native services.

Manifests include `id`, `name`, `version`, `protocolVersion`, `description`, `executables`
(target triple → package-root filename), `capabilities`, `dependencies`, and `settings`.
Each setting has `key`, `label`, `kind` (`text`, `file`, `directory`, `boolean`) and
`description`. Unknown protocol versions are incompatible. Executable paths must be
single filenames, excluding traversal, separators and drive prefixes.

## Packaging and release proof

`pnpm desktop:build` prepares 7-Zip, builds the three official plugin executables,
packages them, then builds the desktop app with the resulting catalog. It creates local
build artifacts only. The desktop app does not automatically install these packages.

For explicit platform packaging:

```powershell
node scripts/package-plugins.mjs --target x86_64-pc-windows-msvc
$env:EXPLORIE_PLUGIN_CATALOG = Join-Path $PWD 'release-artifacts/plugins-x86_64-pc-windows-msvc/explorie-plugin-catalog-x86_64-pc-windows-msvc.json'
cargo build -p explorie-gpui --release --locked
```

Use target `aarch64-apple-darwin` on macOS and export the corresponding catalog path.
Every GPUI release build requires `EXPLORIE_PLUGIN_CATALOG`, including a direct
`cargo build --workspace --release`. Debug/test builds may omit it. Build scripts never
download a catalog during compilation. Installer scripts verify the catalog against its
local packages; use the same catalog that built the app.

Each ZIP contains exactly `plugin.json` and its executable, preserving executable mode
and signed bytes. The builder validates all inputs before emitting packages and requires
plugin versions to equal the app version. Output includes three ZIPs, a platform catalog
and a platform checksum file. The exact verified ZIPs ship inside the desktop installer
alongside the embedded catalog. First launch verifies and extracts them into Explorie's
integration storage without enabling them. Later app upgrades refresh bundled versions
while preserving enablement and configuration. Published ZIP assets remain available for
developer use; bundled activation does not depend on their URLs.

### Personal testing from a draft release

Download the desktop installer while signed into GitHub as the repository owner. In
Settings → Integrations, choose **Enable integration** for the integrations you want.
No separate plugin download or developer flag is required. Syncthing live status still
requires an explicit connection in Configure. Test disable/re-enable and settings persistence
as well as upgrading from a prior build with enabled integrations.

The release workflow packages plugins before compiling the app. On macOS it signs each
executable with the existing Developer ID identity and hardened runtime, submits each ZIP
to Apple's notary service, requires Accepted status, and verifies extracted signatures with
the `notarized` code requirement. Bare executables and ZIPs cannot carry stapled tickets; Gatekeeper
retrieves tickets for notarized executable bytes. See Apple's
[notarization guidance](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

Existing draft-release, checksum, signing and publication gates remain in force. Candidate
and publication jobs verify all desktop and plugin assets. Real-machine attestations include
enabling bundled plugins from the packaged app. Neither fixture ZIP tests nor a local
build establishes macOS readiness: verify downloaded execution on macOS before release.
Also exercise Windows installer upgrades, disable/re-enable, failed update recovery,
and settings persistence in the packaged application.

Run `node --test scripts/package-plugins.test.mjs` for packaging compatibility, byte/mode
preservation, integrity failure, input failure recovery and release-order tests. The runtime
suite separately owns extraction safety, process failures and UI behavior.
