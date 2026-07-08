# Iris Release Automation (v0.3 Pre-Alpha)

This document describes the current automated Windows release path and what remains manual.

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
