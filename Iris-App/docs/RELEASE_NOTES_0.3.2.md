# Iris 0.3.2 Alpha

An updated Alpha in the V0.3 line focused on trustworthy project/tool behavior, a real brain-like long-term memory engine, snappier weather, softer light-mode aesthetics, and self-healing agent stats. Application version `0.3.2-alpha.0`; Windows MSI package version `0.3.2.0`.

## Integrity: no more fabricated project data

- Iris must no longer invent scene, node, or file names, connection/desktop state, or tool results. It states a specific project or desktop fact only when a successful tool observation this turn actually returned it.
- When a project has no enabled, linked MCP bridge or repository, project inspection now reports that plainly and tells you what to enable, instead of producing a plausible-but-fake inventory. "I could not inspect X" is treated as a correct answer.
- This directly addresses sessions where Iris confidently listed scenes that did not exist.

## Dataweb v2 — a brain-like 2D associative memory

The dataweb was a flat text blob read whole into context, which cannot scale. It is now backed by a structured, 2-dimensional associative memory engine:

- Entries become nodes placed in 2D so similar memories cluster together, linked by weighted "synapse" neighbors.
- Recall uses spreading activation: it lights up a few entry nodes by relevance, then walks the synapse graph outward with decaying energy and a bounded visit budget. Retrieval stays fast and small even when the store holds thousands of entries, because a query only traverses a local neighborhood rather than scanning everything.
- Frequently recalled pathways are reinforced; stale, low-value nodes are pruned (synaptic pruning) so the store stays bounded.
- Your human-editable dataweb notes now feed this web automatically, and recalled memory is offered to the agent as clearly-labeled, unverified background context (it must still verify with a tool before stating it as fact).
- New backend commands `dataweb_remember`, `dataweb_recall`, and `dataweb_stats` expose the engine. This is retrieval support and neural-memory emulation, not model training or guaranteed-correct fact storage.

## Weather

- Current-temperature requests reliably use the live weather tool and lead with the actual current reading and its observation time, keeping daily highs/lows clearly separate as forecasts.
- The lookup now copies the place you named verbatim and will not substitute a different city or reuse a location from earlier in the chat.
- Geocoding understands US state abbreviations (for example "Durham, NC" resolves to North Carolina, not a same-named town elsewhere).

## Agent stats and auto-learning

- Tool reliability tracking no longer builds up into a frustrating restriction. The only behavioral effect — a slightly tighter self-review cadence — now triggers on a short *recent* failure streak and clears immediately on the next success, so a tool recovering from a transient outage is not penalized by old history.
- Accumulated stats never block, disable, or refuse any tool. Agent Stats remains informational.

## Light mode

- Surfaces, panels, inputs, and tabs are now soft, theme-tinted off-whites instead of harsh pure white, and they shift with the active color theme. Text/control contrast remains within accessibility targets in every theme.

## Bridge consistency

- The chat "Bridge Servers" launch controls now respect the global "Enable MCP access" setting, so what you can launch from a chat matches what the agent is actually allowed to use.

## Verification and honest limits

Verified: 80 deterministic Rust tests (including 9 for the new memory engine), 29 frontend/setup tests, the production build, and a live check on the installed 3.2B organizer that answered a capital question directly and returned current Durham weather with the real temperature, time, and source in one ~2.4s lookup.

Not yet claimed: full end-to-end asset generation inside Godot over a live MCP bridge is improved (integrity, consistency, and the call path) but still needs on-device verification against a running editor; it is not asserted as solved here. Search/memory quality still depends on the local model and available sources. No model weights are bundled or replaced; an installed `iris-organizer` may still be a 3B model even when the bundled model file specifies 7B.

## Install

Use the installers attached to this release:

- Windows x64: `iris-app_0.3.2-alpha.0_x64-setup.exe` (recommended) or `iris-app_0.3.2-alpha.0_x64_en-US.msi`.
- Apple Silicon Mac: `iris-app_0.3.2-alpha.0_aarch64.dmg`.
- `SHA256SUMS.txt` and `release-artifacts.json` record the verified hashes, source revision, and CI run.

Close Iris before installing, then reopen your existing chat. This update does not require a memory reset; older messages are left intact. These Alpha builds are not publicly code-signed or notarized — approve only a build you trust through your OS security controls.
