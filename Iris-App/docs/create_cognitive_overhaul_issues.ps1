param(
  [string]$Owner = "KennyCrowPixels",
  [string]$Repo = "Iris_for_Godot",
  [string]$MilestoneTitle = "Cognitive Runtime V2 Overhaul"
)

$gh = "C:\Program Files\GitHub CLI\gh.exe"
if (!(Test-Path $gh)) {
  throw "GitHub CLI not found at $gh"
}

Write-Host "Checking GitHub auth..."
& $gh auth status
if ($LASTEXITCODE -ne 0) {
  throw "Not authenticated. Run: `"$gh`" auth login"
}

$milestoneDesc = "Deliver Iris cognitive-runtime overhaul with autonomous phase planning, anti-loop safeguards, non-destructive memory tree, transparent state editing, and real-time progress including current model name."

Write-Host "Ensuring milestone exists: $MilestoneTitle"
$milestones = (& $gh api "repos/$Owner/$Repo/milestones?state=all" | ConvertFrom-Json)
$msCheck = $milestones | Where-Object { $_.title -eq $MilestoneTitle } | Select-Object -First 1
if (-not $msCheck) {
  & $gh api "repos/$Owner/$Repo/milestones" --method POST -f "title=$MilestoneTitle" -f "description=$milestoneDesc"
}

$issues = @(
  @{ Title = "feat(runtime): implement Cognitive Runtime V2 state machine"; Body = "Build long-running orchestrator lifecycle and controls for planning/execution/qa/pause/resume/stop.`n`nAcceptance:`n- Deterministic state transitions`n- Pause/resume preserves state`n- Explicit failed/completed terminal outcomes" },
  @{ Title = "feat(safety): add anti-loop and exit criteria policy engine"; Body = "Implement bounded retries/iterations/time budgets and mandatory success/failure criteria checks.`n`nAcceptance:`n- Infinite-loop trap prevented by policy`n- Failure reasons emitted with actionable diagnostics" },
  @{ Title = "feat(memory): create append-only cognitive document tree"; Body = "Add persistent non-destructive storage for skeleton, phase logs, execution notes, and artifacts.`n`nAcceptance:`n- No destructive overwrite of cognitive docs`n- Versioned, timestamped, auditable history" },
  @{ Title = "feat(planner): implement 4-phase planning pipeline"; Body = "Implement Phase 1-4 pipeline with persisted checkpoints and decomposition under context pressure.`n`nAcceptance:`n- Each phase emits structured output`n- Pipeline can resume from checkpoints" },
  @{ Title = "feat(context): implement condensing loops and selective retrieval"; Body = "Load only relevant context slices, summarize/store iteratively with references to raw evidence.`n`nAcceptance:`n- Context pressure handled without loading full history`n- Retrieval references traceable to source docs" },
  @{ Title = "feat(profiles): add Fast/Advanced/Meticulous thinking-level packs"; Body = "Introduce profile-based depth and QA settings; auto-select when user leaves level unspecified.`n`nAcceptance:`n- Profiles materially change execution depth`n- Auto-select path records rationale" },
  @{ Title = "feat(ui): add real-time execution telemetry with current model name"; Body = "Expose current phase, step, confidence/progress, and active model name in live UI feedback.`n`nAcceptance:`n- Current model name always visible while active`n- Real-time updates during long-running execution" },
  @{ Title = "feat(ui): add mini-player mode for background monitoring"; Body = "Create compact progress window with essential controls and condensed state.`n`nAcceptance:`n- Shows current step and model`n- Supports pause/resume/stop controls" },
  @{ Title = "feat(notify): add milestone/completion/blocker notification settings"; Body = "Configurable notifications for major autonomous events with non-intrusive defaults.`n`nAcceptance:`n- Toggleable event classes`n- Quiet-mode respected" },
  @{ Title = "feat(settings): add editable skeleton and active-state views"; Body = "Settings UI for opening/editing/saving/resetting skeleton and active planning logs.`n`nAcceptance:`n- Safe validation and rollback behavior`n- Changes reflected in runtime state" },
  @{ Title = "feat(tools): harden MCP/SSH/Desktop/Network execution adapters"; Body = "Normalize tool result handling, retries/timeouts, and evidence capture per action.`n`nAcceptance:`n- Reliable error handling paths`n- Structured action evidence for each operation" },
  @{ Title = "test(runtime): add stability and safety validation suite"; Body = "Add tests for loop prevention, pause/resume integrity, low-resource behavior, and telemetry consistency.`n`nAcceptance:`n- Coverage of core failure modes`n- CI-friendly runtime checks" }
)

foreach ($i in $issues) {
  $existing = & $gh issue list --repo "$Owner/$Repo" --state all --limit 200 --json title --jq ".[] | .title" |
    Where-Object { $_ -eq $i.Title }
  if ($existing) {
    Write-Host "Skipping existing issue: $($i.Title)"
    continue
  }
  Write-Host "Creating issue: $($i.Title)"
  & $gh issue create --repo "$Owner/$Repo" --title $i.Title --body $i.Body --milestone $MilestoneTitle
}

Write-Host "Done. Milestone and issue creation pass complete."