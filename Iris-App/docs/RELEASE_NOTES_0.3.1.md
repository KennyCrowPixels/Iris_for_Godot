# Iris 0.3.1 Alpha

An updated Alpha in the V0.3 line, focused on reliable everyday lookups, clearer task recovery, and chat controls. The application version is `0.3.1-alpha.0`; the Windows MSI package version is `0.3.1.0`.

## Chat and Controls

- Reorder tabs with mouse, touch, or keyboard dragging while retaining tab selection, renaming, close buttons, and chat state.
- The collapsed Auto Tasks panel has a visible right-arrow reveal button.
- Single-line messages no longer show a spurious composer scrollbar; longer drafts grow to the bounded composer height before scrolling.
- Copying a message shows a checkmark and a short Copied confirmation on that bubble.
- Paused tasks show that progress is saved, with larger, labeled Pause and Resume controls. Saved pause status is restored when a chat is reopened.
- Reasoning logs consistently use one name and contain saved decision/progress summaries from the active executor. New logs survive tab snapshot updates; missing logs on older messages are not fabricated.
- Settings > Bridges exposes Enable MCP access. Context status follows the selected chat's linked project even while Settings is open and explains disabled or unlinked states.

## Research and Recovery

- Stable questions use a direct conversational path instead of inheriting earlier research failures.
- Live lookups select actual enabled tools and their exact argument schemas. A supported structured result can be returned with its measured values and source without unnecessary searches or synthesis.
- Discovery's visible source list and permitted URLs now match; fresh leads are retained instead of being discarded behind older results.
- Research findings can only cite visible read sources. Retrieved evidence, source quotations, approvals, checkpoints and mutation readback remain required where applicable.
- Inconclusive research can stop with saved findings and uncertainty instead of automatically repeating another search. Reviews assess meaning and implied premises, not an exact positive match to every word in the request.
- Recovery no longer offers premature completion with unfinished evidence steps, permits arbitrary cross-tool arguments, or counts schema-rejected calls as completed duplicate executions.
- User-facing pauses are concise and explicitly resumable. Detailed rejection diagnostics remain saved locally instead of filling the chat with the tool catalog.

## Verification and Limits

Focused live checks on the installed 3.2B organizer answered the capital question directly and returned current Durham conditions, temperature, observation time and source with one lookup in about two seconds. An existing Qwen 7.6B model also passed a current-measurement check. Game-news intake selected web research; this is not a claim that comprehensive Nivalis Nights or Cyberpunk research is solved.

Search quality still depends on the local model, source availability and whether a question's premise is supported. These are model-driven flows, not guaranteed factual engines. Reasoning logs contain concise execution summaries, not private chain-of-thought. No model weights are included or automatically replaced.

Windows x64 and Apple Silicon use the same source but separate native installers. Native macOS GUI behavior and model performance still need on-device testing. These Alpha builds are not publicly code-signed or notarized; approve only a build you trust through the operating system's normal security controls.

## Install

Use the installers attached to this release, not the older files attached to the V0.3 release:

- Windows x64: `iris-app_0.3.1-alpha.0_x64-setup.exe` (recommended), or `iris-app_0.3.1-alpha.0_x64_en-US.msi`.
- Apple Silicon Mac: `iris-app_0.3.1-alpha.0_aarch64.dmg`.
- `SHA256SUMS.txt` records the verified installer hashes; `release-artifacts.json` records their source revision and CI run.

Close Iris before installing. Reopen the existing chat and select Resume to continue saved work; this update does not require a memory reset. Older messages and their missing logs are left intact. Model weights remain separate; an existing `iris-organizer` name may still refer to an older 3B model even when the bundled model file specifies 7B.