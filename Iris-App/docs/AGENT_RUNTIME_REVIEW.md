# Iris Runtime Review and Upgrade

## 0.3.2 Alpha Changes

- **Integrity:** the model is barred from inventing project facts (scene/node/file names, counts, contents), connection/desktop state, or tool results. It may state a specific project/desktop fact only when a successful tool observation this turn returned it. `project_connections` now reports whether the project is actually inspectable and, when no enabled+linked MCP bridge or repository exists, instructs the model to say it cannot inspect and name what to enable — rather than fabricating an inventory.
- **Dataweb v2 (`src-tauri/src/dataweb.rs`):** a brain-like 2D associative memory replaces the flat text blob as the engine. Entries are nodes placed in 2D so similar memories cluster, linked by weighted synapse neighbors. Recall is spreading activation over the synapse graph with per-hop decay and a bounded visit budget, so retrieval is local and cheap even with thousands of entries. Reinforcement strengthens walked pathways; low-value nodes are pruned to stay bounded. Human-editable dataweb notes ingest into the web; the executor injects a bounded, clearly-unverified recall digest into a turn (no-op when empty). Commands: `dataweb_remember`, `dataweb_recall`, `dataweb_stats`. This is retrieval support, not training or verified fact.
- **Weather:** the lookup copies the user's named place verbatim (no substituting a different city or reusing an earlier location), leads with the actual current reading and observation time, and geocoding expands US state abbreviations ("Durham, NC" → North Carolina).
- **Agent stats:** behavioral gating now keys on a short *recent* failure streak (`SkillStats.recentFailures`) that resets on any success, instead of permanent lifetime totals. The sole effect remains a tighter self-review cadence; stats never block or disable a tool.
- **Light mode:** surfaces/panels/inputs/tabs are theme-tinted off-whites instead of pure white and follow the active color; contrast stays within targets.
- **Bridge consistency:** the chat bridge-launch controls respect the global MCP-access toggle, matching agent availability.
- **Not claimed solved:** end-to-end Godot asset generation over a live MCP bridge is improved (integrity, path, consistency) but still needs on-device verification against a running editor.

## What Was Holding Iris Back

The largest regressions were in the agent harness, not React rendering:

| Area | Previous active path | Updated path |
| --- | --- | --- |
| Project routing | Keyword rules could stop at the first MCP result | One controller loop; real tool discovery, no inventory early return |
| Execution | One tool round, then tools removed for final synthesis | Bounded repeated actions, observations, checkpointing, and review |
| Small models | Some profiles disabled repositories and MCP | Same tools at every size; smaller context and steps, longer request timeouts |
| Planning | Much state lived in prompts or a disconnected frontend path | Serializable task/step state with nested refinement and acceptance criteria |
| Completion | Nonempty prose or a text marker could imply success | Evidence-backed steps and writes must be verified; ordinary conversation and reasoning-only plans finish with their answer |
| Continuity | UI selection and disk snapshots could disagree | Explicit per-turn project association, including unlink; saved unfinished tasks survive side conversation |
| Tool arguments | Guessed argument combinations and tool-name scoring | Advertised schemas, JSON Schema validation, and errors returned to the controller |
| Safety | Permission resolver was a stub; Stop only affected frontend requests | Awaited approval, task cancellation, feature policy checks, explicit unknown outcomes |
| Self-assessment | Mostly manually assigned scores | Persisted tool successes, failures, and verified checkpoints, separate from manual ratings |
| Memory | Large transcript prompts, discarded observations | Bounded context with on-demand notes and stored full observations |
| SSH | Payload quoting and automatic action replay could fail | Encoded payloads, root checks, interpreter probing before execution, no alternate-interpreter action replay |
| Stream parsing | Counted braces inside generated text | UTF-8 streaming NDJSON parsing with explicit error/truncation handling |
| Portability | Windows desktop commands and executable paths | Shared Tauri app with macOS adapters, launchers, resources and profiles |

## Execution Contract

React 19 remains the frontend. **ReAct** describes the agent's repeated reasoning/action/observation pattern; it is not the same thing as React's asynchronous rendering.

The default path is `submit_turn` -> `agent_executor` -> `agent_tools`. Each model response is either natural answer text or one structured action. Ordinary questions, conversation, creative writing and roleplay can finish in a single inference call. There is no keyword classifier, mandatory planning pass or JSON-only answer format. The model chooses whether tools or a tracked plan will help.

Actions are `plan`, `refine`, `tool`, `checkpoint`, `review`, `resume`, `finish`, and `pause`. Rust controls the state transitions and action budget. Tool actions are schema-checked against the enabled catalog; known function-name action aliases are normalized, but unknown tools cannot execute. If a model predicts a result after its request, that prediction is discarded. Only the validated action and the real host observation are passed back as the latest conversation messages. Argument failures include the exact schema for repair.

If the model has already selected a tool but supplies invalid arguments, the repair request uses that tool's response schema. Grammar constraints are not imposed on ordinary first-pass conversation. Rejected control responses are saved as `last_rejected_response.json` under that task for diagnosis; private reasoning is not recorded there.

Planning and review controls are introduced when there is tracked work or a review is due. Reasoning-only steps can finish with the delivered answer, without invented external evidence. Evidence-backed steps and pending writes still block unverified completion. Repeated bookkeeping is bounded so a model cannot spend dozens of turns reviewing an empty task.

Ollama responses stream as UTF-8-safe NDJSON. Only final content is shown: the separate thinking field and inline reasoning tags are filtered. Control JSON remains buffered and is not displayed. Natural answer text streams when completion is allowed; text claiming unfinished external work is withheld until the task passes its completion checks. Interrupted streams preserve any partial answer and report the error instead of claiming success.

Recent conversation retains user/assistant roles within the model's context budget instead of being flattened into four tiny excerpts. This supports follow-up questions and roleplay continuity, but is not unlimited memory.

Task state and observations live below the app's memory directory in `agent_tasks`. Each tool result has an evidence id and a stored full response. Reviews also feed the existing Cognitive Docs. The controller sees bounded excerpts and can request further evidence ranges. Interrupted work is retained separately from ordinary chat turns.

Budgets use installed model metadata, total RAM and available memory. Small/constrained models get more action opportunities, more frequent review and longer request timeouts, with smaller context/output limits. Inference is serialized within the new executor. Iris-started Ollama servers use one parallel request and one loaded model. An already-running external Ollama server keeps its own configuration.

Each work session is bounded to 48 or 64 controller actions. Plans have at most 12 steps per expansion and 256 stored steps. Exhaustion pauses with saved progress; it is not success. The scheduler can resume bounded sessions, and a user can resume saved work. This is deliberately not an unbounded background process.

Optional coder/summarizer workers return drafts. Vision inspects supplied images and approved desktop captures. Workers do not receive tool authority. Their outputs are not independent proof of correctness.

## Tools and Permissions

- MCP tools come from all advertised catalog pages. Exact input schemas are checked before calls. Server errors are not confused with source text containing words such as `error`.
- Unannotated MCP calls still require approval, including calls described as read-only. Tool annotations are metadata, not a security sandbox.
- Linked repositories are reference-only. Local writes/clones use the explicit project manipulation root and require approval. Paths cannot escape their root, including through existing symlinks.
- Local inventories report their scope, total matches, pagination and completeness. Generated directories and gitignored files are excluded. A search result is not a proof that no other files exist.
- SSH file operations require a configured remote root. Arbitrary SSH commands are not sandboxed by that root and always require approval. A timed-out remote command may have run: inspect state before retrying.
- Web observations retain source URLs and retrieval time. Empty/provider-failed results are failures, not negative evidence. Retrieved content remains untrusted input.
- Desktop support includes window discovery, app launch/focus and screenshot analysis. It is not yet a general pixel-click/type automation engine. More desktop capabilities can be supplied by an enabled MCP server.
- No approval mode bypasses mutation confirmation in the new executor. Legacy preference controls do not grant shell-wide or tool-wide trust. Cancellation cannot undo an external effect that already happened.

## Online Research

### 0.3.1 Alpha Corrections

The current question is now a separate user-role message during semantic intake, not buried inside old research status. Stable-knowledge answers use a lean conversational prompt without tool examples. Live-data requests select an enabled lookup using its exact schema. A single structured result receives a compact model-driven relevance check and, when supported, returns its real summary and source without a broad search or another synthesis call. The full evidence review remains available when that result is insufficient.

Research discovery presents exactly the unread URLs its schema permits and retains fresh leads when the notebook is full. Citation schemas restrict findings to visible read-source ids. A valid inconclusive stop is honored rather than overridden with another search. Reviews distinguish essential missing facts from optional detail and check implied premises instead of requiring an exact positive keyword match. These are general execution changes, not topic-specific routing.

Recovery uses exact per-tool argument schemas and does not offer finish while evidence checkpoints remain. Validation-rejected calls no longer consume the executed-duplicate allowance. Supported research stays in context while reconciling saved plan checkpoints. Concise pause messages replace tool-catalog dumps; full rejected-action diagnostics remain in the task directory. Completion, write-readback and approval gates remain enforced.

Reasoning logs are bounded, saved executor decision/progress summaries, not raw hidden model reasoning. They are attached to assistant messages and survive snapshot updates such as renaming a tab. The UI restores saved pause status, provides labeled Pause/Resume controls, shows copy confirmation, and uses pointer/keyboard tab reordering. MCP access is a visible Bridges checkbox; its status follows the last selected chat when Settings is open.

Focused live checks on the existing Llama 3.2 3B organizer returned the capital of Lebanon directly, then current Durham conditions with an actual temperature, observation time and source in about two seconds with one lookup. An installed Qwen 7.6B model also passed the measured-weather check (about five seconds before the compact relevance optimization). Both routed the supplied game-news request to web discovery. These checks do not establish comprehensive Nivalis Nights/Cyberpunk research quality or every model's reliability. No installed model or existing chat was replaced by this update.

The model sees compact, typed tool signatures and the current UTC date. Network availability still follows the global setting and the turn's permission; there is no user-keyword router. When web search or a linked project is available, a short model-driven intake distinguishes conversation, project inspection, web research, clarification and resume. Project-specific questions are directed to attached references and bridges, not automatically to web search. Structured decisions use lower sampling variability than ordinary prose. Unknown tools receive a catalog-constrained repair attempt rather than being repeated as prose.

Web search preserves result snippets, decoded destination URLs and available publication dates. Providers include DuckDuckGo, Google News, Bing RSS and Wikipedia entity discovery. The model can choose providers and publication-recency filters. A generic whole-term relevance sanity check rejects obviously unrelated provider payloads; it does not decide the answer or route topics. RSS uses an XML parser, and fetched text is size- and time-bounded. Main content retains multiple articles on index pages and exposes followable source links. Providers can still rate-limit, challenge or return poor results; no paid search API is configured.

Research now uses separate discovery and evidence-review stages. Discovery chooses a real retrieved URL or a revised query. The evidence review records the subject, requested time scope, supported findings, remaining gaps, conflicts and next step. Findings must cite retrieved source ids and exact short quotes. A successful fetch alone cannot finish research. The model can change approach, clarify or stop with partial findings. Search/read/review ceilings scale with model size, usable context and available RAM; suggested source counts are guidance, not mandatory quotas. Direct free-VRAM telemetry is not currently used, and this policy still needs calibration on larger models and Apple unified memory.

Alpha.1 reduces the mandatory review output to findings, gaps and one next step. Malformed or inconclusive reviews can select another source/query rather than immediately ending the task. Balanced/Ask Often receive larger bounded work windows; Testing gets a larger allowance again, without weakening approvals or readback requirements. Explicit resume preserves the original task id, objective, project and evidence, and renews per-window budgets. Research-only tasks are now retained separately from ordinary chat, and recycled tab ids do not inherit unrelated saved tasks. Already-corrupted histories from older builds may need a fresh request with the original objective.

Alpha.2 fixes a deadlock between planned-task reviews and research tool selection. A due plan review is now scheduled explicitly, with a review-only response schema, before any queued research action is considered. The pending action and evidence are retained, including after resume. A final plan review is also scheduled after all evidence-backed steps are checkpointed. Review completion does not complete steps or bypass mutation readback; if the review itself fails, the task reports that specific failure instead of consuming repeated invalid tool attempts. Regression tests cover queued research, paused/resumed review state, final review and pending-write safeguards.

Read-only repository, SSH and MCP inspection can precede planning. Mutations still require a plan and approval; server-declared MCP mutations cannot bypass that rule by being labeled read-only by the model. Cached Godot version metadata is preserved as a hint, not treated as fresh proof. Generated Godot code still requires actual engine/version validation.

Each research session saves its notebook alongside task evidence and a bounded report in `research_history.json` under app data. Agent Stats exposes the attempts, strategies, sources and outcome, with export and helpful/not-helpful feedback. Evidence-supported method sequences can inform later reviews; negative user feedback excludes them. These are untrusted method hints, not factual memory or fine-tuning. Factual relevance and entailment are still model judgments: an exact quote does not prove that the claimed conclusion follows.

Current weather requests include Open-Meteo's current temperature, current conditions, observation time and timezone. Daily highs, lows and forecast conditions live in a separate `forecast` object. Missing current readings fail explicitly; they never fall back to a daily maximum. This is a provider estimate, not necessarily a nearby physical weather-station reading.

Control JSON mixed into prose is rejected. Any provisional reply from that rejected action is removed from the current chat turn without touching earlier messages. Final research text streams after evidence permits completion; ordinary conversation retains its direct streaming path.

## Automation and Services

- Auto Tasks has scheduled and ongoing modes. Positive integer intervals support seconds, minutes, hours and days; custom schedules accept a date/time or a supported interval. Work runs while Iris is open, not as an installed background service.
- Ongoing work requires explicit completion criteria. Each cycle uses the same bounded agent runtime, retains its result and saved task evidence, then yields before another cycle. It stops on explicit completion, a real blocker, missing completion evaluation or repeated lack of progress. It does not guarantee a project becomes objectively "perfect."
- Pause cancels an active turn and preserves progress. Interrupted running tasks reopen paused. Missed intervals coalesce to one due run instead of a catch-up burst, and errors do not silently requeue.
- Automation uses its own chat and cannot take over an unrelated tab when the tab limit is reached. Per-task network restrictions do not flip the user's global Network preference. Foreground generation and drafts defer automatic runs.
- Server settings controls the opt-in external-controller REST API: localhost binding, session bearer tokens, separate read/control scopes, token revocation and active jobs. Remote turns require a desktop UI readiness handshake and retain the normal local approval flow. It is not an MCP protocol implementation or a public Internet endpoint. See [CONTROLLER_API.md](CONTROLLER_API.md).
- Ctrl/Cmd+A in the focused transcript selects that chat only; copying includes names, timestamps and message text, not surrounding controls. Text inputs keep normal selection behavior. Reading older messages disables follow-to-bottom, and the down-arrow returns to the latest message.

## Compared With Mature Agent Systems

A capable agent combines a model with a reliable harness: tool schemas, resource limits, persistent task state, retrieval, verification, permissions and meaningful evaluations. Iris now has a substantially stronger foundation in those areas.

It still differs from a proven production agent in important ways:

1. Decomposition does not create missing knowledge or guarantee frontier-level reasoning. Some models cannot reliably formulate valid subtasks or evaluate their own results.
2. Evidence gates enforce provenance and ordering, not full semantic correctness. A later read of the same scope can still be an inadequate test; the model and acceptance criteria matter.
3. Tool reliability statistics are not intelligence scores. A successful HTTP/MCP response does not necessarily mean the user's goal was met.
4. Small contexts use character-based estimates, not the exact tokenizer for every possible model. Extremely constrained models can still exhaust the protocol/context budget.
5. Godot quality needs real scene inspection, version-aware generation, parse/run checks and visual review. Neither a file write nor a search proves a scene is good.
6. Legacy frontend and backend routines remain for compatibility. The default chat/scheduler no longer uses their heuristic handoffs, but a later removal pass should follow on-device regression testing.
7. Persistent notes are retrieval support, not unlimited reliable memory. Their usefulness still needs long-session evaluation.

## Validation

Automated checks cover runtime budgets, nested plans, fabricated/failed evidence, mutation readback, same-scope verification, review gates, saved-state serialization, project unlink semantics, constrained prompts, exact tool schemas, six-scene inventory, path escapes, UTF-8 stream fragmentation, reply attribution, and every theme/mode's core text/control contrast.

Alpha.1 adds regressions for read-before-plan, explicit resume identity/project preservation, renewed work-window budgets, persistence-mode limits and schema-based live-tool choice. Its targeted local-model intake check passed ordinary-purpose and attached-repository routing, but the explicit ambiguity case still returned the answer lane after three trials. That opt-in model-quality test remains a known failure, not a passing release claim. The user requested a budget-conscious best-effort release; these changes do not establish reliable comprehensive research or automatic clarification for every local model.

Runtime notices now participate in the layout above the composer instead of covering it. Windows bridge/model background launches use CREATE_NO_WINDOW while preserving stdio transport. macOS and Windows still share the same source/version but require separate native installers.

Browser checks exercise theme switching, desktop and narrow viewports, persistent readiness-checklist dismissal and the tab-bar add button. The Development menu is retained behind `ENABLE_DEVELOPER_TOOLS = false`; Agent Stats is independently available under Options.

The opt-in live model check exercises the actual prompt and stream transport with an installed organizer:

```sh
cargo test --manifest-path src-tauri/Cargo.toml live_ollama_conversation_streams_without_planning -- --ignored --nocapture
```

It asks an identity question, starts a roleplay scene and checks recall of the character's name, job and item. `IRIS_TEST_MODEL` can select another installed model; the test does not download one. Timings depend on the model, hardware and whether it is already loaded.

On Windows, [desktop-smoke.mjs](../tests/desktop-smoke.mjs) checks real Tauri/Ollama events in a disposable tab. For this opt-in test only, start WebView2 with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9225`, then run `node tests/desktop-smoke.mjs --tab <test-tab-id>`. Add `--tool` for arithmetic, `--network` for web research, `--weather` for current conditions or `--services` for direct service/search/weather diagnostics. Network checks require the Network setting to be enabled. `--fresh` creates and closes a separate empty test snapshot instead of adding to existing conversation history. The script sends messages and saves test history. Close the test app and remove the debug environment variable afterward; do not enable remote debugging in distributed builds.

Observed on the Windows host with the installed Llama 3.2 3B organizer: identity questions completed in one action across Balanced/Testing permissions with iteration on/off; final text updated visibly before Done; roleplay and a short memory follow-up passed; a fresh tool turn executed arithmetic and returned the verified result. A conversation full of earlier malformed test replies could still bias this small model into inventing a result rather than calling a tool. These checks are smoke tests, not a guarantee of tool compliance or long-session quality on every model.

An earlier network acceptance pass completed the Adventure Time search/read/citation path and distinguished a 40.7 F current reading from a 74.4 F daily high. Broader tests of current-officeholder news and an unfamiliar game exposed additional failures: skipped lookups, weak small-model query reformulation, unrelated provider results and blocked pages. The new stages address those failure classes, but these broader tests have not established reliable 3B research quality. Failed sessions remain visible and are not promoted to successful method memory. Do not equate transport success or unit tests with research intelligence.

The native `--controller` smoke test passed authenticated HTTP access, missing/wrong-token rejection, browser-origin rejection, read-only enforcement, old-token revocation and a complete remote greeting turn through the desktop UI. It stopped its listener and removed its test chat afterward. `--network --prompt "..."` can exercise arbitrary research questions rather than a fixed topic.

Not verified on the Windows development host: native macOS app/DMG execution, macOS permission dialogs, real SSH hosts, live Godot scene mutation, throughput/thermal behavior on the M1 Pro, and the quality of 7B/13B/14B plans. The macOS workflow is added but has not been run here. Signing/notarization and automatic app updates still need release credentials and end-to-end release testing.

## On-Device Acceptance Sequence

Use a disposable Godot project or a version-controlled copy, not your only copy.

1. Describe the linked project, count all scene files and list the evidence paths. Compare with a manual recursive inventory, accounting for the reported exclusions.
2. Ask for a small house blockout in a named scene. Expect discovery, a plan, approval before writes, readback and final review. A file search alone must not be reported as a completed house.
3. Ask for a scene with a deliberately invalid property. Confirm the error is recorded, the model repairs or pauses, and no successful-completion claim hides the failure.
4. Pause mid-task, ask an unrelated question, restart Iris, then continue the saved task. Confirm already-completed work is not blindly replayed.
5. Switch tabs while a task runs. Check that messages, permissions and progress remain associated with the originating tab.
6. Deny a mutation, disable network, and disconnect a bridge. Verify each restriction is honored and failures remain visible.
7. On SSH, test a filename with spaces/quotes, a symlink outside the root, and a command returning a nonzero exit status. Do not use a destructive command for this test.
8. Capture a known desktop window after granting OS permissions. Confirm the answer describes the actual capture and reports missing vision models honestly.
9. Repeat the same task on 7B and the optional larger profile; record completion, retries, wall time, peak memory and independently verified quality. Keep the smaller model if the larger one is worse or causes sustained swap.