# Iris on macOS

Iris uses one shared Windows/macOS Tauri project. There is no separate Mac fork or in-app OS toggle. Native desktop adapters select the platform automatically.

## Install a Packaged Release

For end users, publish a macOS `.dmg` on the repository's GitHub Releases page. On an M1 Pro, use an Apple Silicon / aarch64 build, not a Windows `.exe` or an Intel-only Mac build.

1. Download and open the `.dmg`.
2. Drag `iris-app.app` into Applications, then launch it from Applications.
3. Configure Ollama and the Iris models during initial setup. Model weights are separate downloads, not included in the Iris installer.

A packaged app does not require Node, Rust, Git or Xcode. These development tools are only needed when building from source. Current builds are not signed/notarized for public distribution; only approve a build you trust in System Settings > Privacy & Security. Do not disable Gatekeeper globally.

No Mac installer is produced by a Windows development launch. Build on a Mac using the instructions below, or download `iris-macos-test-bundles` from a successful Cross-Platform Checks run in GitHub Actions. Extract that artifact ZIP and attach its `.dmg` to a GitHub Release. An Actions artifact is not automatically a public Release asset.

## M1 Pro, 16 GB: Start Here

Use macOS 12 or later, an internet connection and sufficient disk space for the source build and model downloads. The 7B profile is the recommended starting point. Installing all optional model families requires substantially more space than installing one profile.

Get the source revision containing `setup-macos.sh` and `launch-macos.command`: clone that branch or use GitHub's Code > Download ZIP and extract it. Uncommitted Windows changes must be transferred or published first. Do not transfer Windows `node_modules` or `src-tauri/target`; dependencies and native binaries must be built on the Mac.

The current working changes include new Rust research/controller modules and frontend components. A GitHub ZIP or clone will not include them until they are committed and published. For a direct private transfer, copy the updated project folder, including its new source files, while excluding `node_modules`, `dist`, `src-tauri/target*` and generated installer output. Do not reuse Windows build artifacts.

Open Terminal, change into the extracted Iris-App folder, then run:

```sh
cd "/path/to/Iris-App"
bash setup-macos.sh --dev --models balanced
bash launch-macos.command
```

Replace the example path with the actual folder. If the Terminal cannot find the scripts, it is in the wrong directory.

For an unattended run after reviewing the script, add `--yes`. If Apple Command Line Tools are missing, the script stops with instructions. Run `xcode-select --install`, complete Apple's installer/license prompts, and rerun. The launcher never handles your password or disables Gatekeeper.

The setup checks or installs Node 22 LTS, Rust and Ollama. Node archives are checked against the official SHA-256 list; the Ollama application signature is checked before copying it to `~/Applications`. Rust's official HTTPS installer is used. npm dependencies and model pulls run sequentially. Existing system installations are reused where compatible.

```sh
bash setup-macos.sh --check
```

`--check` performs prerequisite checks only. Model definition checks without downloads:

```sh
node scripts/setup-models.mjs --profile balanced --check
node scripts/setup-models.mjs --profile 13b --check
node scripts/setup-models.mjs --profile 14b --check
```

## Model Choices

| Profile | Organizer / Coder | Context | Intended Use |
| --- | --- | --- | --- |
| `balanced` | Qwen 2.5 7B / Qwen 2.5 Coder 7B, Q4_K_M | 8K maximum, lower under pressure | Default for M1 Pro 16 GB |
| `13b` | Llama 2 Chat 13B / Code Llama 13B, Q4_K_M | 4K | Optional exact-13B comparison; older models, not automatically better |
| `14b` | Qwen 2.5 14B / Qwen 2.5 Coder 14B, Q4_K_M | 4K | Optional larger-model experiment; less memory headroom |

Shared workers: Qwen 2.5 1.5B summarizer and LLaVA 7B vision. They are optional in app settings. Modelfiles are under `model_files/macos` and use the standard `iris-organizer`, `iris-coder`, `iris-summarizer` and `iris-vision` aliases.

The verified Ollama catalog sizes are approximately 7.9 GB for each listed 13B model and 9.0 GB for the Qwen 14B organizer. These are model file sizes, **not** total RAM requirements. Context/KV cache, macOS, Iris and Godot need additional unified memory. A newer 7B model may be more useful than an older 13B model; size alone is not an upgrade guarantee.

To try a larger profile:

```sh
bash setup-macos.sh --dev --models 13b
# Alternative, not an additional required download:
bash setup-macos.sh --dev --models 14b
```

This replaces the Iris aliases and per-user modelfiles with the chosen profile. Existing downloaded base models remain in Ollama's cache. Use the matching Apple Silicon profile in Settings > LLMs if you later reapply profile defaults. Avoid reapplying a different profile unintentionally.

## Chat With 13B

After transferring the updated source to your Mac, run:

```sh
bash setup-macos.sh --dev --models 13b
bash launch-macos.command
```

Use the organizer for chat. A linked project, MCP connection, desktop permissions and iterative action approval are not required for conversation. Ask a question or start a roleplay directly; planning is available when useful, not a prerequisite to answering. Final answer text streams as it is generated, without showing private reasoning. The readiness checklist can be dismissed with its close button and restored in General settings.

For a first smoke test, ask who Iris is, start a short scene with a named character and distinctive item, then ask it to recall those details. Keep the same chat tab for story continuity. New chats can be added with the plus button after the tabs, up to the existing eight-chat limit.

The 13B profile currently uses Llama 2 Chat, not a hosted Gemini/Copilot-class model. Host improvements remove unnecessary planning delays but cannot guarantee the same reasoning quality or latency. The first response may include model-loading time. Avoid concurrent model workloads, leave optional workers unused for plain chat, and switch to `balanced` if the 16 GB Mac sustains memory pressure or swapping. Native 13B latency and long-roleplay quality still need testing on the Mac.

Iris-started Ollama servers use `OLLAMA_NUM_PARALLEL=1` and `OLLAMA_MAX_LOADED_MODELS=1`. If you already have the Ollama desktop app/server running, its existing configuration wins; quit/restart it with your chosen settings when evaluating memory usage. Close memory-heavy apps and monitor Activity Monitor's Memory Pressure. Return to `balanced` if you see sustained swap or poor responsiveness.

## Build an App and DMG

```sh
bash setup-macos.sh --build --models none
```

Outputs are `src-tauri/target/release/bundle/dmg/*.dmg` and `src-tauri/target/release/bundle/macos/iris-app.app`. Upload the `.dmg` to GitHub Releases for users; the `.app` is the installed application, not an installer. A local build uses the Mac's architecture by default, so build on Apple Silicon for the M1 Pro.

Install/build on the Mac; a Windows Cargo check cannot validate a native macOS bundle. The cross-platform workflow also builds macOS test artifacts. These builds are not configured with your Apple Developer ID or notarization credentials. Use System Settings > Privacy & Security to approve a trusted local build if macOS blocks it; do not disable security globally.

## Permissions and Data

- Screen Recording permission is required for screenshots.
- Accessibility/Automation permission may be required for window discovery and focus. During development, macOS may attribute requests to Terminal or the development executable. The packaged app has a separate identity.
- SSH uses the Mac's OpenSSH client. Configure a remote root, private key and known-hosts file; secrets stay outside prompts. First-time host-key trust must be established intentionally.
- Model files and the editable persona are copied into `~/Library/Application Support/com.pixelcrowgames.iris-app`, not written inside the signed app bundle.
- Chat memory, task notes and evidence use Iris's existing application memory directory. Task notes are available from the progress strip.

Start acceptance testing with the sequence in [AGENT_RUNTIME_REVIEW.md](AGENT_RUNTIME_REVIEW.md). Native Mac behavior, Godot edits, model quality and performance must be verified on your machine; they have not been certified by the Windows checks.

Model references: [Llama 2 13B Q4_K_M](https://ollama.com/library/llama2:13b-chat-q4_K_M), [Code Llama 13B Q4_K_M](https://ollama.com/library/codellama:13b-instruct-q4_K_M), [Qwen 2.5 14B Q4_K_M](https://ollama.com/library/qwen2.5:14b-instruct-q4_K_M).