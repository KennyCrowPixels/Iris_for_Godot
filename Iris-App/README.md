# Tauri + React + Typescript

This template should help get you started developing with Tauri, React and Typescript in Vite.

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## Shipping Iris As A Download

Use this flow to publish Iris from your website (including Google Sites) with manual updates now and auto-updates later.

### Build release installers

Run:

```bat
build-release.bat
```

This generates Windows installer artifacts in:

- `src-tauri/target/release/bundle/nsis/*.exe`
- `src-tauri/target/release/bundle/msi/*.msi`

### Host installers on your website

1. Upload the latest installer to your Google Site (or linked Drive file).
2. Copy the public installer download URL.
3. In Iris, open `Settings -> Network -> Manual App Updates`.
4. Paste:
	- Installer download URL
	- Release notes URL (optional)
5. Click `Save Update Links`.

Users can then click `Open Installer Download` in-app, download, and run the installer to update manually.

### Automatic updates (coming soon)

The Network tab includes a disabled `Automatic Updates (Coming Soon)` section with a reserved update-feed URL field.
You can store your planned feed URL now, but auto-update checks are intentionally disabled until the feed/installer pipeline is finalized.
