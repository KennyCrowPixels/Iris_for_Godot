# Iris Release Automation (v0.3 Pre-Alpha)

This document describes the current automated Windows release path and what remains manual.

## Windows Development Startup

Run `launch-dev.bat` from the repository root. Setup uses incremental `npm install --include=dev` rather than deleting and reinstalling `node_modules` on every launch. CI still uses `npm ci` in its clean workspace.

If npm reports EPERM or EBUSY on `esbuild.exe`, stop this workspace's Vite/Tauri/preview processes with Ctrl+C in their terminals and retry. A previous failed `npm ci` can leave TypeScript missing; the incremental install restores it. Setup stops immediately on any nonzero npm exit, including Windows negative error codes. Running as Administrator is not a general solution to an executable held open by a process.

## Local Installer Files

On Windows, run:

```powershell
.\build-release.bat
```

The current build's Windows `.exe` and `.msi` installers are copied to `builds` in the repository root by default. `BUILD_OUT_DIR` can override that location. Original Tauri outputs are:

- `src-tauri/target/release/bundle/nsis/*.exe`
- `src-tauri/target/release/bundle/msi/*.msi`

On a Mac, run:

```sh
bash setup-macos.sh --build --models none
```

The Mac installer is `src-tauri/target/release/bundle/dmg/*.dmg`; the application is `src-tauri/target/release/bundle/macos/iris-app.app`. See [MACOS_SETUP.md](MACOS_SETUP.md) for prerequisites, architecture and signing limitations.

Attach the Windows installer and Mac DMG to the appropriate version on GitHub Releases. GitHub's automatically generated source ZIP is not an application installer. Development launches do not rebuild production installers; existing files may be stale. Increment the app version consistently before publishing a new version, and rebuild before uploading.

## Current Automation

Workflow: [.github/workflows/windows-prealpha-release.yml](../.github/workflows/windows-prealpha-release.yml)

Trigger conditions:

- Push to `main`: runs validation + Windows build + artifact upload.
- Push a tag like `v0.3.0-alpha.1`: runs validation/build and publishes a prerelease with installer files.
- Manual run (`workflow_dispatch`) from GitHub Actions tab.

What is automated now:

- `npm ci`
- `npm run check`
- `cargo check -q --manifest-path src-tauri/Cargo.toml`
- `npm run tauri:build`
- Upload built `.exe` and `.msi` artifacts
- Publish prerelease for version tags

The separate [Cross-Platform Checks workflow](../.github/workflows/cross-platform-check.yml) runs for pull requests and manual dispatch. Its `iris-macos-test-bundles` Actions artifact contains the Mac DMG. It does not publish that DMG to GitHub Releases automatically; download/extract the artifact and attach the DMG manually. The Windows workflow's artifact is `iris-windows-bundles`.

## In-App Updates Today

GitHub Releases can host both the initial installer downloads and later manual updates. In Iris, use Settings > Network > Manual App Updates, enter the GitHub repository and select Resolve Latest Installer From GitHub. Save the resolved links, then use Open Installer Download. This opens the installer URL in the browser; it does not install an update inside Iris.

The current resolver selects Windows `.exe`/`.msi` assets only. On macOS, paste the matching DMG's direct download URL into Installer download URL instead. Automatic Updates and Check for Updates remain disabled. Uploading assets to GitHub, or saving an update feed URL, does not enable automatic updates. A signed updater and its feed still need to be implemented/configured; a GitHub Releases API response is not a ready-made Tauri updater manifest.

## Release Feed Template

A template feed file lives at [public/update-feed.template.json](../public/update-feed.template.json).

Use it as a starting point for your future auto-update feed endpoint.

## Recommended Tag Strategy

Examples:

- `v0.3.0-alpha.0`
- `v0.3.0-alpha.1`
- `v0.3.1-alpha.0`

Tagging `main` with `v*` is what triggers release publication.

## Remaining Manual/Policy Steps

These still need team decisions or infrastructure:

- Code-signing for production installer trust.
- Final auto-update feed schema and hosting policy.
- In-app updater activation in Settings (currently intentionally gated).
- Optional stable channel (non-prerelease) promotion process.

## Minimal Release Operator Flow

1. Merge PR into `main`.
2. Validate Action run on `main` succeeded.
3. Tag commit with `vX.Y.Z-alpha.N`.
4. Confirm prerelease artifacts are attached.
5. Paste installer URL/release notes URL in Iris Network settings for manual update path.
