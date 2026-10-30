# Iris 0.3.5 Alpha

A deep-polish milestone focused on autonomy, conversational quality, and getting the core features working reliably at a solid everyday level. No breaking changes; existing chats, memory, projects and bridges are preserved. Application version `0.3.5-alpha.0`; Windows MSI package version `0.3.5.0`. (The version is intentionally held at 0.3.5 across this milestone.)

## Autopilot that actually keeps going

Testing Mode is meant to run like autopilot — work to completion on its own and only stop for a real permission or a genuine blocker. It now behaves that way:

- The agent protocol explicitly tells the model to **work autonomously to completion** and to **never pause to ask whether to continue**, to confirm an obvious next step, or just because several steps have passed. The only valid reasons to stop are a concrete external blocker it cannot work around, or an ambiguity only you can resolve. Permission prompts are handled separately and are not a reason to stop.
- In Testing Mode, a premature model-requested pause is **declined and converted into "keep working"** up to a bounded number of times, and the rejection/bookkeeping thresholds are raised, so the task pushes through instead of handing back to you.
- Autonomous (scheduled) tasks continue to auto-resume incomplete work, which now compounds with the above so a run reaches completion far more often without manual "Resume".

## Smarter, more honest conversation

- Fixed a glitch where asking Iris's name or purpose could return a bare label like "greetings"; the assistant now gives a real, complete answer and introduces itself properly.
- The request-intake guidance leans into a **web search whenever you ask to look something up, check, or find current info** (news, prices, reviews, releases, schedules, scores), and when a real-world factual request is borderline it prefers searching over relying on stale knowledge. It is reminded that it **can** search when network tools are enabled — no deterministic keyword routing was added; this is model-guidance only.

## Bridge launch feedback before any stall

- The **Launch** button now paints its "Launching…" spinner and disabled state **before** the connect call runs (two animation frames guarantee the paint), so the brief connect delay reads as intentional progress instead of a freeze-then-spinner.

## Clearer MCP / Godot diagnostics

- Connecting a bridge now reports **which layer failed**: starting/connecting the bridge process vs. discovering its tools. A Godot/GDAI bridge that connects but returns no tools now tells you to make sure the Godot editor is open with the MCP plugin active. This makes it possible to tell whether a failure is the command (e.g. `uv` not found), the bridge process, or the editor not running.

## Smoother startup

- A lightweight **loading splash** (logo + spinner + "Starting Iris…") now shows immediately while the webview and app initialize, instead of a blank/dark screen on slower cold starts.

## Android companion APK

- Added an EAS build profile (`eas.json`, `preview` → APK) and a **GitHub Actions workflow** that builds an installable debug-signed APK for the companion app on every change, uploaded as a build artifact. This gives a **non-terminal install path**: download the APK and tap to install (enable "install unknown apps" once). You can still build locally with `expo run:android` or via EAS.

## Installers (no terminal required)

- **Windows x64**: double-click `iris-app_0.3.5-alpha.0_x64-setup.exe` (or the `.msi`).
- **Apple Silicon Mac**: open `iris-app_0.3.5-alpha.0_aarch64.dmg` and drag to Applications (first launch: right-click → Open, since Alpha builds are unsigned).
- **Android**: install the `iris-companion` APK from the Android APK workflow artifact.

## Verification and honest limits

Verified: 82 Rust tests, 28 frontend tests, both type-checks, and the production build. The autonomy and search changes are prompt- and guard-level improvements for the local model; they make it far more persistent and better at recognizing when to search, but a small local model can still occasionally need a nudge. MCP/Godot bridge success still depends on your environment (a reachable bridge command such as `uv`, and the Godot editor running with the plugin) — the new diagnostics now tell you exactly which of those is missing. These Alpha builds are not code-signed/notarized.
