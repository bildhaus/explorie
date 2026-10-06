<p align="center">
  <img src="apps/desktop/native-assets/icons/icon.png" alt="explorie logo" width="128" height="128">
</p>

# explorie

**Pre-release, local-first file manager for Windows. macOS is a build target, not yet a verified release target.**
_MIT-licensed, built to be understandable, extensible, and easy to customize._

---

## Overview

explorie is a native GPUI file manager currently validated on **Windows**, with macOS support under active release validation. It uses plain JSON metadata and a themeable native UI. The Rust core owns directory listing, file operations, size calculation, archives, and `.explorie.json` custom fields; a native service layer owns previews, jobs, recovery, remote drives, and OS integration.

Key traits:

- No paywalls, no telemetry.
- Hackable front to back: native theme tokens, `.explorie.json` metadata, and readable Rust crates.
- Fast-first: virtualization, cached folder sizes, and async previews.

Current features:

- **Multiple view modes:** List, Grid, and Finder-style Column views with a terminal preview column.
- **Tabbed browsing:** Open multiple directories in tabs (Ctrl/Cmd+T).
- **File previews:** One Explorie-owned Quick Look experience on Windows and macOS, including selected-set navigation and an index sheet, plus images, embedded audio/video, locally rendered PDFs, highlighted text/code, archive listings, and optional helper-generated previews. On macOS, system Quick Look thumbnails and preview images cover HEIC/HEIF, camera RAW, video, Office and iWork files without extra helpers, and files, folders and apps show their Finder icons.
- **Finder conventions on macOS:** A native menu bar (including the Services menu), Finder's default shortcuts, application bundles that open like files (with Show Package Contents), folder symlinks you can browse into, Finder tags shown in rows and searchable in smart folders, cloud-only (iCloud/File Provider) items marked and never downloaded just to preview them, and Spotlight-backed searches with a crawler fallback.
- **System clipboard:** Copy, cut and paste files between explorie windows and Finder or Explorer.
- **Archives:** Built-in ZIP, 7z, TAR/TAR.GZ and RAR handling, plus bundled upstream 7-Zip 26.03 for inspecting and extracting XZ, BZIP2, GZIP, CAB, ISO, WIM, MSI, DMG and other supported archive/disk-image formats. No separate 7-Zip installation is needed. Additional formats are extract-only in Explorie; single-stream formats decompress one layer (for example, `files.tar.xz` produces `files.tar`).
- **Custom metadata:** Read/write `.explorie.json` for custom fields per folder.
- **Theming:** Dark/light/system themes, accent colors, local font stacks, UI scale, density, and more.
- **Drag & drop:** Move files between folders with visual feedback.
- **Safe file operations:** Copies are staged and published without overwriting, cross-volume moves are verified before the source is removed, symbolic links are copied as links, operations work on exFAT/FAT volumes, extraction never merges into an existing folder, replaced items go to the Trash, and interrupted operations can be restored after a crash.
- **Settings panel:** Comprehensive appearance and behavior customization.
- **OS integration:** Native window controls and platform file opening.
- **Persistent Remote Drives:** Reconnect existing rclone remotes as native Windows drive letters or macOS volumes while explorie is running.
- **Optional integrations:** Syncthing, Git, and Obsidian integrations are included and start disabled. Enable them in Settings → Integrations, with separate detection of existing local apps and connection status. Recognize overlapping folder types, inspect local status, and open related applications without leaving the native file browser. See [integration setup and plugin development](docs/plugins.md).

---

## Tech Stack

- **UI/Desktop:** Native GPUI pinned to an exact Zed revision with AccessKit semantics; Rust 2024 edition.
- **CLI:** Rust binary sharing the core crate, with listing and FFmpeg command-preview modes.
- **Libs:** `crates/core` (filesystem, metadata, archives, and file operations), `crates/native-services` (desktop jobs and OS integration), and `crates/ffmpeg-wrapper` (FFmpeg command builder).
- **Tests:** Rust unit/integration tests, GPUI-native rendered/input fixtures, and packaged-runtime smoke checks.

---

## System Requirements

### Required Dependencies

| Dependency  | Version               | Installation                                                           |
| ----------- | --------------------- | ---------------------------------------------------------------------- |
| **Node.js** | 20.x LTS              | [nodejs.org](https://nodejs.org) or `winget install OpenJS.NodeJS.LTS` |
| **pnpm**    | 9.x                   | `npm install -g pnpm` or `corepack enable`                             |
| **Rust**    | 1.98.0 (2024 edition) | [rustup.rs](https://rustup.rs)                                         |

### Optional Dependencies

| Dependency      | Purpose                                          | Installation                                                                           |
| --------------- | ------------------------------------------------ | -------------------------------------------------------------------------------------- |
| **FFmpeg**      | Video playback; video thumbnails on Windows      | Windows: `winget install ffmpeg`<br>macOS: `brew install ffmpeg`                       |
| **LibreOffice** | Office/OpenDocument previews (optional on macOS) | Windows: install from libreoffice.org<br>macOS: `brew install --cask libreoffice`      |
| **ImageMagick** | HEIC/TIFF/PSD previews (optional on macOS)       | Windows: `winget install ImageMagick.ImageMagick`<br>macOS: `brew install imagemagick` |
| **cargo-watch** | Rust hot reload during dev                       | `cargo install cargo-watch`                                                            |

### Platform Notes

- **Windows:** Targets Windows 10/11 and has no WebView2 dependency. When Remote Drives first need WinFsp, Explorie offers the bundled official installer with a native administrator prompt.
- **macOS:** Builds target macOS 13+ and require Xcode Command Line Tools. Remote Drives use an administrator-approved, bundle-contained mount helper. Do not treat macOS as release-ready until the signed package passes the real-machine checklist below.

---

## Monorepo Layout

```
apps/
  desktop/
    gpui/                  # Native GPUI desktop application
    native-assets/         # Shared icons, sidecars, installers, licenses, macOS helpers
  cli/                     # CLI binary (Rust)
crates/
  core/                    # Rust business logic for listing, sizes, metadata
  native-services/         # jobs, previews, recovery, remote drives, OS integration
  ffmpeg-wrapper/          # FFmpeg command builder
sample/                    # Demo data + .explorie.json examples
```

---

## Install

Download the latest build from [GitHub Releases](https://github.com/bildhaus/explorie/releases/latest):

- **Windows 10/11 x64:** Download `explorie-<version>-windows-x64-setup-unsigned.exe` and run the per-user installer. Windows may show an unsigned-app warning. The completion page offers a default-enabled option to remove the downloaded installer after Setup exits; silent in-app updates clean up their verified installer automatically. After installation, Settings → System Integration can reversibly make Explorie the app Windows uses to open folders.
- **macOS 13+ on Apple silicon:** Download `explorie-<version>-macos-arm64.dmg`, open it, and move Explorie to Applications. On first launch from Applications while that release image is still mounted, Explorie offers to eject it and move the downloaded DMG to Trash.

Both installed apps check for newer published versions at startup. After confirmation, Explorie verifies the platform release, replaces itself, removes the update payload and temporary backup, and reopens automatically without changing user settings.

Each release has just two downloads: the Windows installer and the macOS DMG. SHA-256 digests are supplied by GitHub release asset metadata and verified by the updater.

The installers include the integrations and the exact bundled 7-Zip source (`7z2603-src.tar.xz`) with its licenses and notices. On Windows, find the source in the installation directory under `licenses`; on macOS, use Show Package Contents and open `Contents/Resources/licenses`.

---

## Quickstart

```bash
git clone https://github.com/bildhaus/explorie.git
cd explorie

pnpm install                             # install release-script tooling
pnpm prepare:native                      # prepare pinned native helpers and 7-Zip
pnpm desktop:dev                         # run the native GPUI desktop app

cargo run -p explorie-cli -- --help      # CLI help (listing and ffmpeg-preview)
```

---

## Commands & Scripts

### Development

| Command           | Description                                     |
| ----------------- | ----------------------------------------------- |
| `pnpm dev`        | Run the native GPUI desktop app                  |
| `pnpm dev:watch`  | Dev with Rust hot reload (requires cargo-watch) |
| `pnpm rust:watch` | Watch Rust crates and run tests on change       |

### Development Setup

- **macOS without the Metal toolchain:** GPUI compiles its shaders with Xcode's `metal` tool. With only the Command Line Tools installed, add `--features runtime-shaders` to GPUI commands (`cargo run -p explorie-gpui --features runtime-shaders`), or `--features explorie-gpui/runtime-shaders` to workspace-wide ones such as clippy. CI and releases precompile shaders, so never enable it there.
- **Fast local builds without the heavy preview backends:** 3D, audio, SQLite, Parquet/Arrow, font, and email previews are cargo features of `explorie-gpui` (forwarded to `explorie-native-services`) that are on by default, so plain `cargo build`/`cargo test` and every release build include all of them. Leaving them out skips their largest dependencies, including assimp's C++ build, so no CMake is needed: `cargo run -p explorie-gpui --no-default-features --features runtime-shaders` (drop `--features runtime-shaders` if you have the Metal toolchain or are not on macOS). Add single backends back with, for example, `--features preview-sqlite`. Files a build cannot preview are still detected: their preview says the backend isn't included in this build, 3D models keep their file icon instead of a thumbnail, and videos play without sound. CI lints this reduced build.
  - `preview-3d`: 3D model previews and thumbnails (glTF/GLB, OBJ, STL, PLY, 3MF, FBX) through assimp, which builds from C++ source with CMake.
  - `preview-audio`: audio playback and video soundtracks (rodio and Symphonia; needs ALSA headers on Linux).
  - `preview-columnar`: Parquet and Arrow IPC/Feather table previews (the Apache Arrow and Parquet crates).
  - `preview-sqlite`: SQLite database previews (bundled SQLite, compiled from C).
  - `preview-fonts`: TTF, OTF, WOFF, and WOFF2 font specimens.
  - `preview-mail`: `.eml` email previews.
  - `full-previews`: all of the above; the default.
- **Prepare 7-Zip before running tests:** `node scripts/prepare-7zip.mjs` downloads and verifies the pinned 7-Zip that archive tests and GPUI builds use (`pnpm prepare:native` also fetches rclone and WinFsp). `release` and `ci` GPUI builds fail without it.
- **Run tests like CI with [cargo-nextest](https://nexte.st):** `cargo nextest run --workspace` runs every test in its own process and in parallel, and kills any test that runs longer than three minutes (`.config/nextest.toml`). Plain `cargo test` still works, but it runs a crate's tests as threads of one process. Doctests need `cargo test --doc`.
- **Use a short, canonical `TMPDIR` for tests, like CI:** macOS's default `/var/folders/…/T` is long and sits behind the `/var` → `/private/var` symlink, which filesystem-safety tests (canonical paths, link-ancestor checks) are sensitive to. For example: `mkdir -p /private/tmp/explorie-tests && TMPDIR=/private/tmp/explorie-tests cargo test --locked -p explorie-core`.
- **Cargo profiles:**
  - `dev` builds workspace crates at `opt-level = 0` for fast incremental rebuilds and dependencies at `opt-level = 2`, so image, archive, and PDF decoding stay usable in debug runs. The first build after changing these settings recompiles every dependency once.
  - `release` (thin LTO, one codegen unit) is what `pnpm desktop:build` and tagged releases ship. It keeps `panic = "unwind"`: preview decoders and background jobs rely on `catch_unwind`, and the build fails if a profile switches to `abort`.
  - `ci` is `release` without LTO and with 16 codegen units, for fast optimized PR builds. Like `release`, it needs the prepared 7-Zip and an `EXPLORIE_PLUGIN_CATALOG` from `node scripts/package-plugins.mjs`; `cargo build -p explorie-gpui --profile ci` then writes to `target/ci/`.
- **Fuzzing (optional):** `fuzz/` holds cargo-fuzz targets for the 7-Zip listing parser, archive path validation, `.explorie.json` custom fields, and plugin protocol frames. Run them with `cargo install cargo-fuzz`, then `cargo +nightly fuzz run <target>`. A weekly workflow also runs each target.

### Building & Testing

| Command              | Description                                                            |
| -------------------- | ---------------------------------------------------------------------- |
| `pnpm desktop:build` | Build the GPUI app for production                                      |
| `pnpm release:check` | Run local release-candidate checks and write `.release-checks` reports |
| `pnpm test`          | Run the complete authoritative Rust workspace                          |
| `pnpm test:rust`     | Run the complete authoritative Rust workspace                          |
| `pnpm lint`          | Check Rust formatting and strict workspace clippy                      |

For release-candidate verification, run `pnpm release:check` and use the release checklist below.

To measure GPUI listing-state updates at 10,000 and 100,000 entries, including sorting, refreshing, and filtering with every file selected:

```bash
cargo test --locked -p explorie-gpui records_large_folder_interaction_baselines --release -- --ignored --nocapture
```

Set `EXPLORIE_BENCH_COUNTS` to comma-separated sizes for smaller comparisons. These in-memory fixtures measure browser-state work; filesystem enumeration, preview decoding, and GPU frame time are excluded. Compare revisions using the same machine, toolchain, profile, and fixture sizes.

To measure List and Grid draws while scrolling with 10,000 and 100,000 files selected:

```bash
cargo test --locked -p explorie-gpui records_large_selection_render_baselines --release -- --ignored --nocapture
```

This uses GPUI's native test runtime at an 800×600 viewport and checks that drawing does not construct multi-file drag payloads. It measures CPU layout/paint work, not GPU presentation time.

### CLI

```bash
cargo run -p explorie-cli -- --help
explorie [--with-sizes] [path]                           # List directory
explorie ffmpeg-preview in.mp4 out.webm --vf scale=1280:720  # Preview FFmpeg args
```

---

## Environment Variables

| Variable   | Default                    | Description               |
| ---------- | -------------------------- | ------------------------- |
| `RUST_LOG` | `info,explorie_core=debug` | Rust logging level filter |

---

## Screenshots

Add screenshots or a short demo GIF here before publishing the final public repository.

---

## Security and Filesystem Access

explorie is a local file manager. The GPUI process reads paths the user opens and delegates privileged or blocking work to typed native services. Write access is used only for explicit operations such as rename, move, copy, delete, archive, extract, metadata edits, and versioned app-local state.

Review the native-service path protections and packaged resources before shipping forks or release artifacts. Treat custom builds and helper binaries with the same care as any other local file-management tool. Do not paste sensitive file contents, private paths, credentials, or exploit details into public issues.

Security vulnerabilities should be reported through GitHub private vulnerability reporting for the release repository. If private reporting is unavailable, open a minimal public issue asking for a private contact route without including exploit details.

explorie does not include telemetry. Diagnostics exports are local-only and are designed to redact path-like and sensitive values, but review any report before sharing it.

Remote Drives use the bundled, pinned rclone executable with the user's existing rclone configuration. Choose **Remote Drives → Configure** to open rclone's own interactive setup in a terminal; when it closes, Explorie refreshes the remote list and opens the Add Drive dialog. Explorie never stores provider credentials or OAuth tokens. Encrypted rclone configurations must be unlockable non-interactively through the user's existing rclone environment or password command. Mount processes run only while explorie is open; stable per-profile VFS caches allow interrupted uploads to resume.

On macOS, a connected remote drive is served by `rclone serve nfs` on a fresh random `127.0.0.1` port for as long as it stays connected. Before the administrator-approved helper mounts it as root (with `nosuid,nodev`), the helper checks that the only listener on that port is the requesting user's team-signed rclone, bound to IPv4 loopback. rclone's NFS server has no authentication, though, and doesn't check which user is asking. While a drive is connected, any process on the Mac that finds the port, including software running under other local accounts, can read and change the files in that cloud drive, and other local accounts can browse the mounted volume. Connect remote drives only on Macs whose other accounts and installed software you trust, and disconnect drives you aren't using. Closing this gap needs an authenticated transport, such as an rclone FUSE mount through macFUSE or FUSE-T, or WebDAV with per-mount credentials. That is future work. Windows drives use `rclone mount` with WinFsp instead.

---

## Known Limitations

- Public binary releases still need real-machine packaged-app QA before broad distribution; macOS is explicitly not release-ready until that proof exists.
- Video playback uses an optional local FFmpeg process for probing and bounded native playback; unavailable helpers produce an actionable fallback. On macOS, video thumbnails come from Quick Look without FFmpeg.
- On Windows, Office/OpenDocument previews require LibreOffice and HEIC/HEIF/TIFF/PSD previews require ImageMagick. On macOS these fall back to Quick Look when the helpers are missing.
- Linux is compile- and lint-checked in CI but is not a product target: `main()` exits early, and Windows/macOS-only features (remote drives, bundled 7-Zip, integrations, the menu bar) are unavailable there.
- Explorie Quick Look and Column View behavior, plus notarized DMG behavior, should be checked on a real Mac for each release candidate.

---

## Release Checklist

Run:

```bash
cargo install cargo-audit --locked # once per machine
pnpm release:check
```

The command writes local evidence under `.release-checks/`, which is ignored by git. It requires a clean, version-aligned working tree; runs dependency audits, Rust formatting, the full workspace tests, strict clippy, and a locked GPUI release build; then verifies the executable under the workspace `target/release` directory.

Windows and macOS builds check the latest published GitHub Release automatically. Updates use the exact platform asset and refuse to install unless its size and SHA-256 match GitHub's release asset size and SHA-256 digest, and, once an update signing key is configured (see [Update signing](#update-signing)), unless its detached ed25519 signature verifies against the key compiled into the running app; failed checks leave the installed app untouched. Windows updates run the per-user installer, replace the installed files, remove the installer, and reopen Explorie. macOS updates additionally require the downloaded DMG and app to pass Developer ID, Gatekeeper, bundle-ID, version, and signing-team checks before a staged bundle swap; failures roll back to the old app, while success removes the DMG and backup before reopening. Windows Authenticode signing is intentionally not part of the release contract. Releases remain immutable and version-tag based. Bump the matching versions in the root package and GPUI Cargo manifest, merge the candidate commit to `main`, and wait for its `CI Gate` to pass before pushing a new `v<version>` tag. That immutable tag builds the candidate packages once and attaches them to a draft release. After those exact assets pass the real-machine checklist below, manually dispatch the release workflow from that tag with the Windows and macOS attestations and the two tested artifact SHA-256 values. The protected publish job verifies those hashes against the existing draft before publishing it without rebuilding. Existing releases, assets, and tags are never replaced; failures are fixed in a new version.

Protect `v*` tags against update/deletion, require `CI Gate` on `main`, protect the `release-signing` and `release-publish` environments, and enable immutable releases in the GitHub repository settings before public distribution.

macOS releases require these signing secrets:

- macOS: base64-encoded P12 in `APPLE_CERTIFICATE`, plus `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`.

The release workflow publishes an explicitly named unsigned per-user Windows x64 installer, a signed/notarized macOS arm64 DMG. Windows packages intentionally remain unsigned, so SmartScreen or antivirus warnings are expected. CI installs, launch-smokes, and uninstalls the Windows package; publication additionally requires explicit proof that both candidates passed disposable filesystem operations on real machines.

Each `v<version>` draft contains exactly two assets: `explorie-<version>-windows-x64-setup-unsigned.exe` and `explorie-<version>-macos-arm64.dmg`.

### Update signing

GitHub's SHA-256 digest comes from the same API response as the download, so on its own it only proves the bytes match what the GitHub account published. Update signing adds an independent check: the release workflow signs both installers with an offline-generated ed25519 key, and the updater refuses any payload whose signature does not verify against the public key embedded at build time. This matters most for the unsigned Windows installer, which the updater runs silently.

Signatures use [minisign](https://jedisct1.github.io/minisign/)'s prehashed format. They are carried as machine-readable lines inside a hidden `<!-- explorie-update-signatures ... -->` block in the release notes (`explorie-signature <asset-name> <signature> <global-signature>`), so each release still has exactly two assets. Each signature's trusted comment is `explorie-update <asset-name>`, which binds it to one version and platform.

To turn signing on:

1. Generate a key pair offline with `minisign -G -p update-signing-key.pub -s update-signing.key` (or `rsign generate`). Keep the password-protected secret key in your password manager, not in the repository.
2. Store the full contents of `update-signing.key` as the `UPDATE_SIGNING_KEY` secret of the `release-signing` environment, and its password as `UPDATE_SIGNING_KEY_PASSWORD`. The candidate job now runs in that environment, so any reviewers that environment requires also gate the draft.
3. Replace `crates/native-services/update-signing-key.pub` with the generated public key file and ship it in a release. Builds can instead take the key from the `EXPLORIE_UPDATE_PUBLIC_KEY` environment variable at compile time.

When a public key is compiled in, every update on every platform must carry a valid signature. The candidate job fails if the public key and `UPDATE_SIGNING_KEY` don't match or if the secret is missing, and the candidate and publication checks verify the signatures in the draft notes. Without a key (for example in development builds) updates keep the SHA-256-only behavior and the notes are left unchanged. `node scripts/update-signatures.mjs verify --notes <notes.md> <installer>...` checks a release by hand. To rotate the key, ship the new public key in a release that is still signed with the old key, then sign the following releases with the new key. Anyone who installs a build that predates the embedded key updates once through the SHA-256-only path.

Integration ZIPs and catalogs stay inside the build and installed app. Build checksum files remain internal CI evidence. Candidate and publication checks verify that GitHub exposes exactly the two expected installers and that their sizes and SHA-256 digests match the uploaded bytes; publication also requires the exact hashes attested on real machines.

Before creating a new tag, manually verify:

- Launch the generated app on the target OS.
- Open folders in List, Grid, and Column views.
- Preview text, image, PDF/document, video, archive, and unsupported files.
- Exercise copy, move, rename, delete/trash, undo/redo, archive, and extract flows on disposable files.
- Reopen the app and confirm persisted settings.
- Confirm Windows and macOS packaged-app behavior on real machines.
- Install the previous public version on each platform, accept the in-app update, and verify it replaces the app, preserves settings, removes the update payload and backup, and reopens at the new version. Install the Windows package, verify the completion-page cleanup removes the downloaded installer, run `cargo test -p explorie-native-services integration::tests::windows_system_open_produces_a_real_shell_side_effect -- --exact --ignored` from an interactive Windows session, verify the System Integration toggle routes folder opens to Explorie and restores the prior handler when disabled or uninstalled, and confirm the unsigned warning is expected. Install the macOS package, verify Explorie offers to eject the mounted release image and moves its DMG to Trash, and verify signing/notarization plus both installer SHA-256 digests.

Create a per-candidate real-machine evidence file with `pnpm platform:proof:init`, fill in the exact artifact names and SHA-256 hashes, then mark each observed check. `pnpm platform:proof:verify` rejects wrong artifact names, missing Windows multi-window/DnD/mixed-DPI/crash/folder-handler proof, and missing macOS multi-window/DnD/multi-monitor/crash/signing/notarization/Gatekeeper proof. It prints the two tested hashes to paste into the protected publication dispatch. The evidence stays under ignored `.release-checks/`; archive it alongside the candidate checksums before enabling the workflow attestations.

On macOS versions that keep Finder as the folder handler (macOS 26 refuses every change to it), Settings ▸ System Integration disables the folder toggle and says why; record that reason as the macOS section's `folderIntegrationUnavailable` and set `folderIntegrationEnabled` and `folderIntegrationDisabled` to `"not-applicable"`. No other check can be waived this way.

Both platforms also require remote-drive lifecycle and bundled-integration activation proof. Enable each included integration from a fresh profile with no package downloads, verify disabled defaults and persistence after restart, and exercise Git folder navigation during filesystem changes. Use disposable files for remote writes and confirm disconnect/reconnect behavior.

Only the first `bildhaus/explorie` release at `0.0.1` may record an updater exemption: add `"firstRelease": { "reason": "First public Bildhaus release; no previous GPUI release to upgrade from" }` to the evidence file and set each platform's `automaticUpdateReplacedCleanedAndReopened` check to `"not-applicable"`. Every other check remains mandatory. Omit `firstRelease` for subsequent releases; `0.0.2` and later must pass the real upgrade check.

---

## Keyboard Shortcuts

Each platform gets its native file manager's conventions: Explorer-style keys on Windows and Finder-style keys on macOS. Every editable shortcut can be rebound in **Settings → Shortcuts**; press `?` in the browser for the live list, including fixed keys.

| Action                                | Windows                        | macOS                         |
| ------------------------------------- | ------------------------------ | ----------------------------- |
| Open selected item                    | `Enter`                        | `Cmd+O` or `Cmd+Down`         |
| Quick Look                            | `Space`                        | `Space`                       |
| Rename                                | `F2`                           | `Return`                      |
| Move to Trash                         | `Delete`                       | `Cmd+Backspace`               |
| Delete permanently                    | `Shift+Delete`                 | `Cmd+Option+Backspace`        |
| Back / Forward                        | `Alt+Left` / `Alt+Right`       | `Cmd+[` / `Cmd+]`             |
| Enclosing folder                      | `Alt+Up` or `Backspace`        | `Cmd+Up`                      |
| Go to folder                          | `Ctrl+G`                       | `Cmd+Shift+G`                 |
| Search filenames                      | `Ctrl+F`                       | `Cmd+F`                       |
| New folder                            | `Ctrl+Shift+N`                 | `Cmd+Shift+N`                 |
| Copy / Cut / Paste                    | `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | `Cmd+C` / `Cmd+X` / `Cmd+V`   |
| Undo / Redo                           | `Ctrl+Z` / `Ctrl+Y`            | `Cmd+Z` / `Cmd+Shift+Z`       |
| Show or hide hidden files             | `Ctrl+H`                       | `Cmd+Shift+.`                 |
| List / Grid / Column view             | `Ctrl+1` / `Ctrl+2` / `Ctrl+3` | `Cmd+2` / `Cmd+1` / `Cmd+3`   |
| Refresh                               | `F5`                           | `Cmd+R`                       |
| New window / New tab                  | `Ctrl+N` / `Ctrl+T`            | `Cmd+N` / `Cmd+T`             |
| Close tab (or window on its last tab) | `Ctrl+W`                       | `Cmd+W`                       |
| Next / Previous tab                   | `Ctrl+Tab` / `Ctrl+Shift+Tab`  | `Ctrl+Tab` / `Ctrl+Shift+Tab` |
| Add or remove favorite                | `Ctrl+D`                       | `Ctrl+Cmd+T`                  |
| Command palette                       | `Ctrl+Shift+P`                 | `Cmd+Shift+P`                 |
| Settings                              | `Ctrl+,`                       | `Cmd+,`                       |
| Connect remote drives                 | `Ctrl+Shift+R`                 | `Cmd+K`                       |
| Keyboard shortcuts                    | `?`                            | `?`                           |
| Close dialogs, menus, or Quick Look   | `Escape`                       | `Escape`                      |

On macOS, explorie also has a native menu bar (File, Edit, View, Go, Window, Help, plus Services, Hide, and Quit) whose key equivalents follow your current shortcuts. The Go menu adds Finder's `Cmd+Shift+H` (Home), `Cmd+Shift+D` (Desktop), and `Cmd+Shift+O` (Documents). Plain `Backspace` does nothing in the browser (as in Finder), and `Cmd+H`, `Cmd+Q`, `Cmd+M`, `Cmd+Tab`, and ``Cmd+` `` keep their standard meanings.

---

## Custom Metadata

explorie reads optional `.explorie.json` files from folders to attach custom fields to entries. A minimal file looks like:

```json
{
  "report.pdf": {
    "status": "review",
    "owner": "Alex",
    "tags": ["finance", "q2"]
  }
}
```

The metadata stays next to your files and is not synced by explorie itself.

---

## Third-Party Licenses and Attribution

explorie source code is MIT-licensed. The authoritative dependency graph is primarily MIT, Apache-2.0, Apache-2.0/MIT dual-licensed, BSD-2-Clause, BSD-3-Clause, ISC, MIT-0, and compatible permissive licenses.

Notable runtime and UI dependencies include GPUI, AccessKit, and Rust crates for filesystem, archive, tracing, local media decoding, and platform integration. Explorie bundles rclone v1.74.4 under its MIT license and includes the license in packaged applications. Windows packages also include the official, unmodified WinFsp installer: **WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos**, [source and license](https://github.com/winfsp/winfsp). Optional external helpers such as FFmpeg, LibreOffice, and ImageMagick are not bundled; their own licenses apply to user-installed copies.

The bundled [7-Zip](https://www.7-zip.org/) 26.03 engine is copyright Igor Pavlov and distributed under LGPL-2.1-or-later with the additional BSD notices and unRAR restriction in its [license](https://www.7-zip.org/license.txt). Its license, LGPL text and attribution are included in packaged applications; exact corresponding source is included in each installed application's licenses directory. Explorie's own source remains MIT-licensed. The fallback streams decoded bytes through Explorie's existing path checks, byte limits and staged extraction; it never delegates output-path creation to 7-Zip. Archives with ambiguous names, unsupported links or alternate streams are rejected. Formats that omit decoded sizes may show zero in inspection until extracted. Existing archive creation and ZIP/7z/TAR/RAR extraction continue to use the Rust integrations.

App icons and sample assets in this repository are project assets unless replaced before release.

Before publishing a binary distribution, regenerate dependency license evidence from the final release repository and artifact build:

```bash
pnpm licenses list
cargo metadata --format-version 1
```

---

## License

MIT

---

Contributions, forks, and experiments are welcome.
