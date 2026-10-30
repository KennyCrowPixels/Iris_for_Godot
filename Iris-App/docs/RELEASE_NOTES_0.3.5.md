# Iris 0.3.5 Alpha

A deep-polish milestone focused on autonomy, conversational quality, and getting the core features working reliably at a solid everyday level. No breaking changes; existing chats, memory, projects and bridges are preserved. Application version `0.3.5-alpha.0`; Windows MSI package version `0.3.5.0`. (The version is intentionally held at 0.3.5 across this milestone.)

## Autopilot that actually keeps going

Testing Mode is meant to run like autopilot — work to completion on its own and only stop for a real permission or a genuine blocker. It is now **renamed "Autopilot"** (clearer label, same setting) and actually behaves that way:

- The agent protocol explicitly tells the model to **work autonomously to completion** and to **never pause to ask whether to continue**, to confirm an obvious next step, or just because several steps have passed. The only valid reasons to stop are a concrete external blocker it cannot work around, or an ambiguity only you can resolve. Permission prompts are handled separately and are not a reason to stop.
- In Autopilot, a premature model-requested pause is **declined and converted into "keep working"** up to a bounded number of times, and the rejection/bookkeeping thresholds are raised, so the task pushes through instead of handing back to you.
- Autonomous (scheduled) tasks continue to auto-resume incomplete work, which now compounds with the above so a run reaches completion far more often without manual "Resume".

## MCP / Godot bridge polish

The bridge connection works, but the agent loop around it was mishandling results. Fixed the three biggest failure modes from real testing:

- **No more invented scene/node names.** A project-inspection turn can no longer finish stating scenes, nodes, or files unless it actually called a real inspection tool (`call_project_tool`, `project_files`, or `ssh_action`). If it hasn't inspected, it must do so; if inspection fails, it says so honestly instead of fabricating a plausible list.
- **Repeated-action stalls now recover instead of pausing.** When the model tries the exact same tool call again, it is handed back **its own cached result** for those arguments and told to answer from that evidence (or try a different tool/args). This fixes both the "kept pausing / Resume loop" and the "the answer only showed up on a later prompt" symptom — the data was already retrieved; the loop just wasn't delivering it.
- Combined with Autopilot declining premature pauses, the delete/replace-a-node style task no longer gets stuck repeating "no evidence supports completion"; it is pushed to take the concrete bridge action.
- **The actual MCP bug, fixed.** Reasoning logs showed the model was calling the inspection tool with `mcp_id`, while the schema demanded `connection_id` — so **every inspection call failed schema validation** and got trapped in a resume loop. The bridge id aliases (`mcp_id` / `connectionId`) are now accepted, `call_project_tool` tolerates an omitted `arguments`/`effect` (defaults to a no-arg read), and `project_connections` now **eagerly hands the model each connected bridge's live tool list (or its connection error)** so it can read the project in one step instead of the error-prone multi-step dance.
- **Project turns stay on the project.** A project-inspection turn no longer has web tools, and the model is told the MCP bridge is its own built-in way to see the project — so it stops web-searching "what is an MCP bridge" (it once even read the Malayan Communist Party Wikipedia page). External research (games, companies, topics) no longer misroutes to project inspection just because a project is attached.

## Long-task reliability ("Trudge")

For tasks that exceed a small model's one-shot ability (deep research, multi-source essays), the loop now trudges to a real result instead of looping forever:

- **It always delivers.** When the research loop exhausts its budget, Iris now **synthesizes a best-effort answer from the sources it actually read** (clearly flagging what's uncertain), instead of the old empty "Research paused — 155 lookups, 0 verified." You get the essay built from real evidence.
- **It reads instead of re-searching.** After a search, if there are unread results, the model is now **required to open one** rather than re-running near-identical searches. This ends the runaway "dozens of identical lookups, zero pages read" behaviour.
- The exact-quote citation gate still guards factual claims, but it no longer prevents a usable answer from being delivered when the small model can't produce a verbatim quote for every sentence.

## Activity logs & mic

- **Copy activity logs.** Options ▾ → **Copy activity logs** copies every saved reasoning/activity log for the current chat to the clipboard in one go (handy for reporting issues). The per-message reasoning-log viewer also got a sticky, non-selectable header with its own Copy button.
- **Mic feedback.** The listening state now shows an animated indicator + a "Listening…" pill instead of a confusing muted-mic icon, and a failed desktop mic now explains clearly that the webview's speech service is unavailable (the Android companion uses native on-device speech). Reliable desktop dictation needs native capture and is planned separately.


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
