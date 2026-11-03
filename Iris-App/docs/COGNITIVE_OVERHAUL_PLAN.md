# Cognitive Overhaul Plan (Planning Only)

Branch: feat/cognitive-overhaul-planning
Date: 2026-08-10
Status: Planning complete, implementation not started

## Objective
Deliver a major cognitive-runtime upgrade for Iris that prioritizes depth, autonomy, transparency, and persistent non-destructive memory over speed.

## Non-Negotiable Product Requirements
- Real-time progress feedback must always include the current active model name.
- Autonomous execution must have strict anti-loop and exit-criteria safeguards.
- Memory/documents are append-only and non-destructive for cognitive artifacts.
- Users can view/edit key planning state (Skeleton + active plan logs) from Settings.

## Target Architecture

### 1) Cognitive Runtime V2 Orchestrator
- Introduce long-running goal orchestration with explicit lifecycle:
  - idle -> planning -> executing -> qa -> paused -> completed | failed
- Add controls: pause, resume, stop, cancel.
- Add resource throttling for background execution.

### 2) Planning Pipeline (4 Phases)
- Phase 1: Goal + resource discovery.
- Phase 2: Exit criteria + constraints + mastery standards.
- Phase 3: Master plan + skeleton step graph.
- Phase 4: Execution loops with bounded retries and QA gates.

### 3) Non-Destructive Documentation Tree
- Append-only store for:
  - Skeleton master document
  - phase logs
  - execution notes
  - artifacts/evidence
- Keep retrieval by targeted slices, not whole-history loading.

### 4) Thinking Levels
- Fast, Advanced, Meticulous strategy packs.
- If unspecified, auto-select in Phase 1 based on complexity and resource constraints.

### 5) Transparency UI and Mini-Player
- Full-size settings panels for editable state.
- Tiny mini-player for ongoing autonomous progress.
- Milestone notifications (optional and configurable).

## Implementation Sequence (Recommended)
1. Data contracts and runtime state machine.
2. Anti-loop policy engine and execution guards.
3. Persistent document tree and retrieval primitives.
4. Phase planner pipeline and step executor.
5. Real-time telemetry stream with current model name.
6. Settings transparency views/editor controls.
7. Mini-player and notifications.
8. Stability hardening, metrics, and migration.

## To-Do Checklist
- [ ] Define schemas: GoalSpec, ExitCriteria, MasterStep, LoopState, MilestoneEvent, DocNode, DocVersion.
- [ ] Implement runtime lifecycle manager (start/pause/resume/stop).
- [ ] Implement anti-loop constraints (max iterations/retries/time budget).
- [ ] Add resource governor (CPU/GPU/background policy).
- [ ] Implement append-only cognitive document store.
- [ ] Implement condensing loop manager with source references.
- [ ] Implement planning phases 1-4 as explicit pipeline.
- [ ] Add thinking-level strategy packs + auto-select.
- [ ] Add live progress stream (phase, step, current model name, progress estimate).
- [ ] Add mini-player UI.
- [ ] Add notifications for milestone/blocker/fail/complete.
- [ ] Add settings transparency editor for skeleton + logs.
- [ ] Add robust tool wrappers (MCP/SSH/Desktop/Network) with normalized results.
- [ ] Add test matrix (loop trap, resume integrity, low-resource behavior).
- [ ] Add feature flags and rollout plan.

## Acceptance Criteria
- Autonomous runs can execute long goals without unbounded loops.
- Current model name is visible in real-time progress surfaces.
- Cognitive memory is non-destructive and auditable.
- User can inspect/edit planning state and safely recover.
- Mini-player and notifications are optional, stable, and non-intrusive.

## Out-of-Scope for First Implementation Slice
- Fully automatic cloud parity behavior.
- Full self-directed open-ended execution without explicit guardrail policies.
- Cross-device state sync.
