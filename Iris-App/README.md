# Iris App (v0.3 Pre-Alpha)

Iris is a Tauri + React desktop assistant focused on real project work.

The active Rust runtime streams ordinary conversation directly and uses a bounded action/evidence/review loop when work needs tools. It has saved task notes, model-aware budgets, schema-checked tools and real approval/cancellation handling. React 19 is the shared frontend.

Architecture, limitations and acceptance tests: [docs/AGENT_RUNTIME_REVIEW.md](docs/AGENT_RUNTIME_REVIEW.md).

## Research and Automation

Enable Network for live research. A small model-driven evidence-needs check decides whether a request needs current or uncertain facts; there is no topic keyword router. Research can revise queries, change providers, inspect source pages and retain supported findings. Scope ceilings adapt to model size, context and available memory. Options > Agent Stats exposes research attempts, method notes and user feedback. This is persistent method memory, not model-weight training; provider availability and local-model judgment still limit reliability.

Auto Tasks supports recurring intervals and ongoing work with explicit completion criteria. Tasks have their own chats, saved progress and pause/resume controls; schedules run only while Iris is open. Settings > Server enables an authenticated, opt-in external-controller API with read/control permissions and revocable session tokens. It binds to localhost; use a trusted encrypted tunnel for another device. Protocol and security details: [docs/CONTROLLER_API.md](docs/CONTROLLER_API.md).

Focus the chat transcript and use Ctrl+A (Cmd+A on macOS) to select the full log for copying, including names and timestamps. The down-arrow returns to the newest message without interrupting reading higher in the log.

## macOS Quick Start

```sh
bash setup-macos.sh --dev --models balanced
bash launch-macos.command
```

For an M1 Pro with 16 GB unified memory, start with 7B Q4 models. Optional 13B and 14B profiles use a constrained 4K context. Larger does not automatically mean better or faster. Installation, permissions, native builds and memory guidance: [docs/MACOS_SETUP.md](docs/MACOS_SETUP.md).

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
- Installs Ollama if missing; optional `--with-models` downloads the balanced model set
- The second command launches Tauri dev mode

### Build installers (one command)

```bat
build-release.bat
```

What this does:

- Runs the same automated bootstrap path (`setup-windows.bat --consumer --yes`)
- Builds Windows installer bundles
- Copies `.exe` and `.msi` outputs into `builds` (override with `BUILD_OUT_DIR`)

## Auto-Installed Dependencies

`setup-windows.bat` checks and installs the following when missing:

- Git for Windows
- Node.js LTS
- Rust toolchain (rustup + cargo)
- Visual Studio 2022 Build Tools (C++ workload)
- Ollama

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
2. Upload the newest installer from `builds` or your configured `BUILD_OUT_DIR`.
3. In-app, open `Settings -> Network -> Manual App Updates`.
4. Save installer URL (and optional release notes URL).

Automatic in-app updater plumbing is still intentionally gated while feed + signing workflows are finalized.

## What Still Needs To Be Done

These are the remaining steps before truly frictionless consumer installs + updates:

- Configure installer code-signing certificates in CI.
- Finalize and host update feed JSON schema (template: [public/update-feed.template.json](public/update-feed.template.json)).
- Enable and test in-app automatic updater flow end-to-end.
- Run native Mac acceptance testing and configure Apple signing/notarization before distributing public Mac releases.

## Checks

```sh
npm run check
npm test
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
```

Cross-platform CI: [.github/workflows/cross-platform-check.yml](.github/workflows/cross-platform-check.yml).
