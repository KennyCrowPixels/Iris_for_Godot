# Iris App (v0.3 Pre-Alpha)

Iris is a Tauri + React desktop assistant focused on real project work.

This branch includes the Async ReAct runtime overhaul and post-overhaul onboarding upgrades.

## Windows Quick Start

### Developers (one command)

```bat
setup-windows.bat --dev --yes
launch-dev.bat
```

What this does:

- Auto-installs missing system dependencies with `winget`
- Installs npm dependencies
- Runs `npm run check`
- Launches Tauri dev mode

### Build installers (one command)

```bat
build-release.bat
```

What this does:

- Runs the same automated bootstrap path (`setup-windows.bat --consumer --yes`)
- Builds Windows installer bundles
- Copies `.exe` and `.msi` outputs into `D:\Iris_for_Godot\Builds`

## Auto-Installed Dependencies

`setup-windows.bat` checks and installs the following when missing:

- Git for Windows
- Node.js LTS
- Rust toolchain (rustup + cargo)
- Visual Studio 2022 Build Tools (C++ workload)

The script prints progress percentages at each major step so contributors and build operators can see where setup is spending time.

## Contributor Guide

See [CONTRIBUTING.md](CONTRIBUTING.md) for full setup, workflow, and troubleshooting notes.

## CI Release Automation

Windows release automation is now scaffolded with GitHub Actions:

- Workflow: [.github/workflows/windows-prealpha-release.yml](.github/workflows/windows-prealpha-release.yml)
- Validation on `main`: `npm run check` + `cargo check`
- Installer build on Windows runner: `npm run tauri:build`
- Tag-based prerelease publishing for tags matching `v*`

Detailed operator flow: [docs/RELEASE_AUTOMATION.md](docs/RELEASE_AUTOMATION.md)

## Shipping Iris As A Download

1. Run `build-release.bat`.
2. Upload the newest installer from `D:\Iris_for_Godot\Builds`.
3. In-app, open `Settings -> Network -> Manual App Updates`.
4. Save installer URL (and optional release notes URL).

Automatic in-app updater plumbing is still intentionally gated while feed + signing workflows are finalized.

## What Still Needs To Be Done

These are the remaining steps before truly frictionless consumer installs + updates:

- Configure installer code-signing certificates in CI.
- Finalize and host update feed JSON schema (template: [public/update-feed.template.json](public/update-feed.template.json)).
- Enable and test in-app automatic updater flow end-to-end.
- Add platform coverage beyond Windows (if required).
