# Contributing to Iris (Windows-First)

Iris v0.3 pre-alpha is currently optimized for Windows desktop development.

## One-Click Setup

Run from the repo root:

```bat
setup-windows.bat --dev --yes
```

This script auto-installs missing dependencies with `winget`, prints percentage progress, installs npm packages, and validates TypeScript.

## Dependency List (Auto-Installed)

`setup-windows.bat` installs these if they are missing:

- Git for Windows (`Git.Git`)
- Node.js LTS (`OpenJS.NodeJS.LTS`)
- Rust toolchain via rustup (`Rustlang.Rustup`)
- Visual Studio 2022 Build Tools (`Microsoft.VisualStudio.2022.BuildTools`) with C++ workload

## Run Iris In Development

After setup:

```bat
launch-dev.bat
```

## Build Desktop Installers

For contributor QA or release packaging:

```bat
build-release.bat
```

Outputs are copied to:

- `D:\Iris_for_Godot\Builds`

## Consumer-Oriented Build Path

To prepare a machine for building installers with minimal manual steps:

```bat
setup-windows.bat --consumer --yes
build-release.bat
```

## Troubleshooting

- If `winget` install is blocked, rerun terminal as Administrator.
- If Rust/C++ toolchain changed during setup, close and reopen terminal once.
- If builds fail after a dependency upgrade, run:

```bat
npm install
npm run check
```

## Branch And Commit Workflow

- Create a feature branch from `main`.
- Keep commits focused and descriptive.
- Run checks before opening PR:

```bat
npm run check
cargo check -q --manifest-path src-tauri/Cargo.toml
```

- Open PR to `main`.
- After merge, delete feature branch locally/remotely.

### Delete merged branch

Local:

```bat
git branch -d your-feature-branch
```

Remote:

```bat
git push origin --delete your-feature-branch
```

## Automated Pre-Alpha Release Flow

GitHub Actions workflow:

- [.github/workflows/windows-prealpha-release.yml](.github/workflows/windows-prealpha-release.yml)

For release publication:

1. Merge into `main`.
2. Tag the target commit with `vX.Y.Z-alpha.N`.
3. Push the tag.
4. Confirm prerelease artifacts were attached on GitHub Releases.

For more detail, see [docs/RELEASE_AUTOMATION.md](docs/RELEASE_AUTOMATION.md).
