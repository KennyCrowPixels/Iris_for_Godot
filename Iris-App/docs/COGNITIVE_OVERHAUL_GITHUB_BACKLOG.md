# GitHub Backlog: Cognitive Overhaul

This file maps the planning doc into a milestone + issue set.

Milestone title:
Cognitive Runtime V2 Overhaul

Milestone description:
Deliver Iris cognitive-runtime overhaul with autonomous phase planning, anti-loop safeguards, non-destructive memory tree, transparent state editing, and real-time progress including current model name.

## Issues

### 1) feat(runtime): implement Cognitive Runtime V2 state machine
Summary:
Build long-running orchestrator lifecycle and controls for planning/execution/qa/pause/resume/stop.
Acceptance:
- Deterministic state transitions
- Pause/resume preserves state
- Explicit failed/completed terminal outcomes

### 2) feat(safety): add anti-loop and exit criteria policy engine
Summary:
Implement bounded retries/iterations/time budgets and mandatory success/failure criteria checks.
Acceptance:
- Infinite-loop trap prevented by policy
- Failure reasons emitted with actionable diagnostics

### 3) feat(memory): create append-only cognitive document tree
Summary:
Add persistent non-destructive storage for skeleton, phase logs, execution notes, and artifacts.
Acceptance:
- No destructive overwrite of cognitive docs
- Versioned, timestamped, auditable history

### 4) feat(planner): implement 4-phase planning pipeline
Summary:
Implement Phase 1-4 pipeline with persisted checkpoints and decomposition under context pressure.
Acceptance:
- Each phase emits structured output
- Pipeline can resume from checkpoints

### 5) feat(context): implement condensing loops and selective retrieval
Summary:
Load only relevant context slices, summarize/store iteratively with references to raw evidence.
Acceptance:
- Context pressure handled without loading full history
- Retrieval references traceable to source docs

### 6) feat(profiles): add Fast/Advanced/Meticulous thinking-level packs
Summary:
Introduce profile-based depth and QA settings; auto-select when user leaves level unspecified.
Acceptance:
- Profiles materially change execution depth
- Auto-select path records rationale

### 7) feat(ui): add real-time execution telemetry with current model name
Summary:
Expose current phase, step, confidence/progress, and active model name in live UI feedback.
Acceptance:
- Current model name always visible while active
- Real-time updates during long-running execution

### 8) feat(ui): add mini-player mode for background monitoring
Summary:
Create compact progress window with essential controls and condensed state.
Acceptance:
- Shows current step and model
- Supports pause/resume/stop controls

### 9) feat(notify): add milestone/completion/blocker notification settings
Summary:
Configurable notifications for major autonomous events with non-intrusive defaults.
Acceptance:
- Toggleable event classes
- Quiet-mode respected

### 10) feat(settings): add editable skeleton and active-state views
Summary:
Settings UI for opening/editing/saving/resetting skeleton and active planning logs.
Acceptance:
- Safe validation and rollback behavior
- Changes reflected in runtime state

### 11) feat(tools): harden MCP/SSH/Desktop/Network execution adapters
Summary:
Normalize tool result handling, retries/timeouts, and evidence capture per action.
Acceptance:
- Reliable error handling paths
- Structured action evidence for each operation

### 12) test(runtime): add stability and safety validation suite
Summary:
Add tests for loop prevention, pause/resume integrity, low-resource behavior, and telemetry consistency.
Acceptance:
- Coverage of core failure modes
- CI-friendly runtime checks
