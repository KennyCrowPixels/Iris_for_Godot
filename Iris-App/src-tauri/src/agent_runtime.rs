use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelBudget {
    pub context_tokens: usize,
    pub output_tokens: usize,
    pub max_actions: usize,
    pub review_interval: usize,
    pub observation_chars: usize,
    pub request_timeout_secs: u64,
    #[serde(default)]
    pub research: ResearchBudget,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchBudget {
    pub suggested_sources: usize,
    pub max_searches: usize,
    pub max_reads: usize,
    pub max_reviews: usize,
    pub notebook_chars: usize,
}

impl Default for ResearchBudget {
    fn default() -> Self { Self::for_capacity(7.0, 4096, 4) }
}

impl ResearchBudget {
    pub fn for_capacity(model_billions: f64, context_tokens: usize, available_gib: u64) -> Self {
        let model_scope = ((model_billions.max(1.0) / 7.0).sqrt() * 3.0).round() as usize;
        let context_scope = (context_tokens / 1800).clamp(2, 10);
        let memory_scope = if available_gib < 2 { 2 } else if available_gib < 6 { 4 } else { 10 };
        let suggested_sources = model_scope.clamp(2, 10).min(context_scope).min(memory_scope);
        Self {
            suggested_sources,
            max_searches: (suggested_sources * 2 + 2).min(20),
            max_reads: (suggested_sources * 3 + 2).min(30),
            max_reviews: (suggested_sources + 3).min(10),
            notebook_chars: (context_tokens / 2).clamp(900, 6000),
        }
    }
}

impl ModelBudget {
    pub fn apply_persistence_mode(&mut self, mode: &str) {
        let multiplier=if mode=="testing" {3} else {2};
        self.max_actions=self.max_actions.saturating_mul(multiplier).min(192);
        self.research.max_searches=self.research.max_searches.saturating_mul(multiplier).min(40);
        self.research.max_reads=self.research.max_reads.saturating_mul(multiplier).min(60);
        self.research.max_reviews=self.research.max_reviews.saturating_mul(multiplier).max(12).min(36);
    }

    pub fn for_capacity(memory_gib: u64, model_billions: f64, model_context: usize) -> Self {
        let constrained = memory_gib < 12 || (memory_gib <= 16 && model_billions >= 12.0);
        let context_tokens = if constrained { 4096 } else if memory_gib <= 16 { 8192 } else { 16384 }
            .min(model_context.max(2048));
        Self {
            context_tokens,
            output_tokens: if context_tokens <= 2048 { 512 } else if constrained { 1024 } else { 2048 },
            max_actions: if constrained || model_billions < 10.0 { 64 } else { 48 },
            review_interval: if constrained || model_billions < 10.0 { 3 } else { 5 },
            observation_chars: if constrained { 2400 } else { 5000 },
            request_timeout_secs: if constrained { 600 } else { 360 },
            research: ResearchBudget::for_capacity(model_billions, context_tokens, memory_gib.saturating_sub(4)),
        }
    }

    pub fn input_char_budget(&self) -> usize {
        self.context_tokens.saturating_sub(self.output_tokens + 256) * 3
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub title: String,
    pub acceptance: String,
    #[serde(default = "default_true")]
    pub requires_evidence: bool,
}

fn default_true() -> bool { true }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AgentAction {
    Plan { steps: Vec<PlanItem> },
    Refine { step_id: u32, steps: Vec<PlanItem> },
    Tool { tool: String, arguments: Value },
    Checkpoint { step_id: u32, summary: String, evidence: Vec<u64> },
    Review { summary: String },
    Resume,
    Finish { message: String },
    Pause { reason: String },
}

impl AgentAction {
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text.trim()).map_err(|err| format!("Invalid agent action: {err}"))
    }

    pub fn from_response(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() { return Err("The model returned an empty response.".into()); }
        let candidate = trimmed.strip_prefix("```json").or_else(|| trimmed.strip_prefix("```"))
            .and_then(|body| body.trim().strip_suffix("```"))
            .map(str::trim).unwrap_or(trimmed);
        if candidate.starts_with('{') {
            let first = serde_json::Deserializer::from_str(candidate).into_iter::<Value>().next()
                .ok_or("Empty JSON response")?.map_err(|err| format!("Incomplete or invalid JSON response: {err}"))?;
            if first.get("action").is_some() {
                return serde_json::from_value(first).map_err(|err| format!("Invalid agent action: {err}"));
            }
            let value: Value = serde_json::from_str(candidate).map_err(|err| format!("Incomplete or invalid JSON response: {err}"))?;
            if value.get("action").is_some() { return Self::parse(candidate); }
        }
        if text.match_indices('{').any(|(offset, _)| text[offset + 1..].trim_start().starts_with("\"action\"")) {
            return Err("A tool/control request was mixed with answer text. Return exactly one complete action object, without prose or predicted results.".into());
        }
        Ok(Self::Finish { message: text.to_string() })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus { Pending, Complete }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStep {
    pub id: u32,
    pub parent_id: Option<u32>,
    pub title: String,
    pub acceptance: String,
    pub requires_evidence: bool,
    pub status: StepStatus,
    pub notes: String,
    pub evidence: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub id: u64,
    pub tool: String,
    pub ok: bool,
    pub read_only: bool,
    #[serde(default)]
    pub scope: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus { Running, Paused, Blocked, Completed, Cancelled }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTask {
    pub id: String,
    pub tab_id: u32,
    pub project_id: Option<String>,
    pub goal: String,
    pub model: String,
    pub status: TaskStatus,
    pub steps: Vec<TaskStep>,
    pub observations: Vec<Observation>,
    pub review: String,
    pub actions_since_review: usize,
    pub total_actions: usize,
    pub pending_writes: Vec<u64>,
    pub last_message: String,
    #[serde(default)]
    pub reasoning_log: Vec<Value>,
}

impl AgentTask {
    pub fn new(id: String, tab_id: u32, project_id: Option<String>, goal: String, model: String) -> Self {
        Self {
            id, tab_id, project_id, goal, model,
            status: TaskStatus::Running,
            steps: Vec::new(), observations: Vec::new(), review: String::new(),
            actions_since_review: 0, total_actions: 0, pending_writes: Vec::new(),
            last_message: String::new(),
            reasoning_log: Vec::new(),
        }
    }

    pub fn record_reasoning(&mut self, text: &str) {
        self.reasoning_log.push(serde_json::json!({"ts":time::OffsetDateTime::now_utc().unix_timestamp()*1000,"text":excerpt(text,1000)}));
        let overflow=self.reasoning_log.len().saturating_sub(80);
        self.reasoning_log.drain(..overflow);
    }

    pub fn plan(&mut self, items: Vec<PlanItem>, parent_id: Option<u32>) -> Result<(), String> {
        if items.is_empty() || items.len() > 12 {
            return Err("Plan 1-12 small steps at a time. Refine a step to add another layer.".into());
        }
        if self.steps.len() + items.len() > 256 {
            return Err("Task reached 256 steps; checkpoint and continue as a new task.".into());
        }
        if items.iter().any(|item| item.title.trim().is_empty() || item.acceptance.trim().is_empty()) {
            return Err("Every step needs a title and a concrete acceptance criterion.".into());
        }
        if let Some(parent) = parent_id {
            if !self.steps.iter().any(|step| step.id == parent && step.status == StepStatus::Pending) {
                return Err("Only an unfinished step can be refined.".into());
            }
        } else if !self.steps.is_empty() {
            return Err("A plan already exists. Refine unfinished steps instead of discarding progress.".into());
        }
        let mut next_id = self.steps.iter().map(|step| step.id).max().unwrap_or(0) + 1;
        for item in items {
            self.steps.push(TaskStep {
                id: next_id, parent_id, title: excerpt(&item.title, 240),
                acceptance: excerpt(&item.acceptance, 800), requires_evidence: item.requires_evidence,
                status: StepStatus::Pending, notes: String::new(), evidence: Vec::new(),
            });
            next_id += 1;
        }
        Ok(())
    }

    pub fn observe(&mut self, tool: &str, ok: bool, read_only: bool, side_effect_attempted: bool, summary: &str) -> u64 {
        let id = self.observations.last().map(|entry| entry.id + 1).unwrap_or(1);
        self.observations.push(Observation {
            id, tool: tool.to_string(), ok, read_only, scope: "task".into(), summary: excerpt(summary, 1200),
        });
        if side_effect_attempted { self.pending_writes.push(id); }
        self.actions_since_review += 1;
        id
    }

    pub fn checkpoint(&mut self, step_id: u32, summary: &str, evidence: &[u64]) -> Result<(), String> {
        let Some(step) = self.steps.iter().find(|step| step.id == step_id) else {
            return Err("Unknown step id; read the task state and use its ids.".into());
        };
        if step.status == StepStatus::Complete {
            return Err("Step already complete; work on an unfinished step.".into());
        }
        if summary.trim().is_empty() {
            return Err("A checkpoint needs a concise outcome, not hidden reasoning.".into());
        }
        if self.steps.iter().any(|child| child.parent_id == Some(step_id) && child.status != StepStatus::Complete) {
            return Err("Complete the child steps before their parent.".into());
        }
        let valid = evidence.iter().all(|id| self.observations.iter().any(|entry| entry.id == *id && entry.ok));
        if !valid || (step.requires_evidence && evidence.is_empty()) {
            return Err("Completion requires ids from successful tool observations; never invent evidence.".into());
        }
        for write_id in &self.pending_writes {
            let Some(write) = self.observations.iter().find(|entry|entry.id==*write_id) else {return Err("Missing mutation evidence".into());};
            let verified = evidence.iter().any(|id| self.observations.iter().any(|entry| {
                entry.id == *id && entry.id > *write_id && entry.ok && entry.read_only && entry.scope==write.scope
            }));
            if !verified {return Err("Read back the changed resource scope after each mutation before checkpointing.".into());}
        }
        self.pending_writes.clear();
        let step = self.steps.iter_mut().find(|step| step.id == step_id).unwrap();
        step.status = StepStatus::Complete;
        step.notes = excerpt(summary, 1200);
        step.evidence = evidence.to_vec();
        Ok(())
    }

    pub fn review_due(&self, budget: &ModelBudget) -> bool {
        self.actions_since_review >= budget.review_interval
    }

    pub fn review(&mut self, summary: &str) -> Result<(), String> {
        if summary.trim().is_empty() { return Err("Review must record progress, remaining gaps, and the next adjustment.".into()); }
        self.review = excerpt(summary, 2000);
        self.actions_since_review = 0;
        Ok(())
    }

    pub fn finish(&mut self, message: &str) -> Result<(), String> {
        if message.trim().is_empty() { return Err("Final answer cannot be empty.".into()); }
        self.completion_ready()?;
        for step in self.steps.iter_mut().filter(|step| step.status == StepStatus::Pending && !step.requires_evidence) {
            step.status = StepStatus::Complete;
            step.notes = "Delivered in the final response.".into();
        }
        self.status = TaskStatus::Completed;
        self.last_message = message.to_string();
        Ok(())
    }

    pub fn completion_ready(&self) -> Result<(), String> {
        if self.steps.iter().any(|step| step.requires_evidence && step.status != StepStatus::Complete) {
            return Err("Unfinished steps remain. Continue, refine them, or pause with a precise blocker.".into());
        }
        if !self.pending_writes.is_empty() {
            return Err("Changes have not been read back and checkpointed; verify before finishing.".into());
        }
        if self.steps.iter().any(|step| step.requires_evidence) && (self.review.is_empty() || self.actions_since_review > 0) {
            return Err("Review the completed plan against the original goal before finishing.".into());
        }
        Ok(())
    }

    pub fn current_step(&self) -> Option<&TaskStep> {
        self.steps.iter().find(|step| step.status == StepStatus::Pending && !self.steps.iter().any(|child| {
            child.parent_id == Some(step.id) && child.status == StepStatus::Pending
        }))
    }

    pub fn compact_context(&self, max_chars: usize) -> String {
        let pending: Vec<_> = self.steps.iter().filter(|step| step.status == StepStatus::Pending).take(12).collect();
        let completed = self.steps.iter().filter(|step| step.status == StepStatus::Complete).count();
        let recent: Vec<_> = self.observations.iter().rev().take(3).collect();
        let value = serde_json::json!({
            "taskId": self.id, "completedSteps": completed, "totalSteps": self.steps.len(),
            "currentStep": self.current_step(), "pendingSteps": pending,
            "lastReview": self.review, "pendingWrites": self.pending_writes,
            "recentObservations": recent
        });
        excerpt(&value.to_string(), max_chars)
    }
}

pub fn excerpt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars { return text.to_string(); }
    if max_chars < 80 { return text.chars().take(max_chars).collect(); }
    let marker = "\n[truncated; read the stored evidence for more]\n";
    let room = max_chars.saturating_sub(marker.chars().count());
    let head: String = text.chars().take(room * 3 / 4).collect();
    let tail: String = text.chars().rev().take(room - room * 3 / 4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}{marker}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_log_is_bounded_and_older_task_files_remain_readable() {
        let mut task=AgentTask::new("log".into(),1,None,"Goal".into(),"model".into());
        for index in 0..100 {task.record_reasoning(&format!("Observed step {index}"));}
        assert_eq!(task.reasoning_log.len(),80);
        assert_eq!(task.reasoning_log.last().unwrap()["text"],"Observed step 99");
        let mut legacy=serde_json::to_value(&task).unwrap();
        legacy.as_object_mut().unwrap().remove("reasoningLog");
        assert!(serde_json::from_value::<AgentTask>(legacy).unwrap().reasoning_log.is_empty());
    }

    #[test]
    fn persistence_modes_expand_work_not_context_or_permissions() {
        let mut balanced=ModelBudget::for_capacity(16,7.0,8192);
        let mut testing=balanced.clone();
        balanced.apply_persistence_mode("balanced");testing.apply_persistence_mode("testing");
        assert!(testing.max_actions>balanced.max_actions);
        assert_eq!(testing.context_tokens,balanced.context_tokens);
        assert!(testing.max_actions<=192);
    }

    #[test]
    fn research_capacity_scales_without_forcing_a_search_count() {
        let local = ResearchBudget::for_capacity(7.0, 8192, 8);
        let large = ResearchBudget::for_capacity(70.0, 32768, 32);
        assert!(local.suggested_sources < large.suggested_sources);
        assert!(local.max_searches < large.max_searches);
        assert!(large.max_reads <= 30);
        assert!(local.max_reads > local.suggested_sources);
    }

    #[test]
    fn research_capacity_respects_context_and_memory_pressure() {
        let constrained = ResearchBudget::for_capacity(70.0, 4096, 1);
        assert_eq!(constrained.suggested_sources, 2);
        assert!(constrained.notebook_chars <= 2048);
        assert_eq!(ResearchBudget::for_capacity(70.0, 2048, 64).suggested_sources, 2);
    }

    fn task() -> AgentTask { AgentTask::new("test".into(), 1, None, "Design and verify a scene".into(), "local".into()) }
    fn step() -> PlanItem { PlanItem { title: "Inspect".into(), acceptance: "All paths accounted for".into(), requires_evidence: true } }

    #[test]
    fn larger_model_on_sixteen_gib_gets_smaller_context_and_more_time() {
        let small = ModelBudget::for_capacity(16, 7.0, 32768);
        let large = ModelBudget::for_capacity(16, 13.0, 32768);
        assert!(large.context_tokens < small.context_tokens);
        assert!(large.request_timeout_secs > small.request_timeout_secs);
        assert!(large.max_actions >= small.max_actions);
    }

    #[test]
    fn budgets_respect_model_context() {
        assert_eq!(ModelBudget::for_capacity(64, 32.0, 4096).context_tokens, 4096);
        assert!(ModelBudget::for_capacity(8, 3.0, 2048).input_char_budget() > 0);
    }

    #[test]
    fn cannot_replace_a_live_plan() {
        let mut state = task();
        state.plan(vec![step()], None).unwrap();
        assert!(state.plan(vec![step()], None).is_err());
        assert!(state.finish("Done").is_err());
    }

    #[test]
    fn refined_work_must_complete_before_parent() {
        let mut state = task();
        state.plan(vec![step()], None).unwrap();
        state.plan(vec![step()], Some(1)).unwrap();
        assert_eq!(state.current_step().unwrap().id, 2);
        let evidence = state.observe("list", true, true, false, "six scenes, complete inventory");
        assert!(state.checkpoint(1, "Done", &[evidence]).is_err());
        state.checkpoint(2, "Enumerated all pages", &[evidence]).unwrap();
        state.checkpoint(1, "Verified six scenes", &[evidence]).unwrap();
        assert!(state.finish("Six scenes").is_err());
        state.review("Goal met; inventory covered all pages").unwrap();
        state.finish("Six scenes").unwrap();
    }

    #[test]
    fn mutation_requires_later_readback_not_search_before_write() {
        let mut state = task();
        state.plan(vec![step()], None).unwrap();
        let before = state.observe("read", true, true, false, "before");
        let write = state.observe("write", true, false, true, "saved");
        assert!(state.checkpoint(1, "Saved", &[before, write]).is_err());
        let after = state.observe("read", true, true, false, "after");
        state.checkpoint(1, "Read back changed scene", &[write, after]).unwrap();
    }

    #[test]
    fn fabricated_or_failed_evidence_cannot_complete_work() {
        let mut state = task();
        state.plan(vec![step()], None).unwrap();
        let failure = state.observe("read", false, true, false, "transport error");
        assert!(state.checkpoint(1, "Done", &[99]).is_err());
        assert!(state.checkpoint(1, "Done", &[failure]).is_err());
        assert!(state.checkpoint(1, "Done", &[]).is_err());
    }

    #[test]
    fn failed_mutation_also_requires_readback() {
        let mut state = task();
        state.observe("write", false, false, true, "transport lost; outcome unknown");
        assert!(state.finish("Everything worked").is_err());
    }

    #[test]
    fn unrelated_tool_success_cannot_verify_a_mutation() {
        let mut state=task();
        state.plan(vec![step()],None).unwrap();
        state.observe("write",true,false,true,"saved");
        state.observations.last_mut().unwrap().scope="repo:project".into();
        let unrelated=state.observe("web",true,true,false,"search results");
        state.observations.last_mut().unwrap().scope="web".into();
        assert!(state.checkpoint(1,"Verified",&[unrelated]).is_err());
    }

    #[test]
    fn review_cadence_is_enforced_by_state() {
        let mut state = task();
        let budget = ModelBudget::for_capacity(16, 7.0, 8192);
        for _ in 0..budget.review_interval { state.observe("read", true, true, false, "evidence"); }
        assert!(state.review_due(&budget));
        state.review("Need a broader query; previous results were partial").unwrap();
        assert!(!state.review_due(&budget));
    }

    #[test]
    fn simple_answers_do_not_require_a_plan() {
        task().finish("Hello").unwrap();
    }

    #[test]
    fn reasoning_only_plans_finish_with_the_answer_without_external_checkpoints() {
        let mut state = task();
        state.plan(vec![PlanItem { title:"Draft a scene".into(), acceptance:"A playable opening".into(), requires_evidence:false }],None).unwrap();
        state.finish("The observatory lights up. What do you do?").unwrap();
        assert_eq!(state.status,TaskStatus::Completed);
        assert_eq!(state.steps[0].status,StepStatus::Complete);
    }

    #[test]
    fn conversational_replies_finish_without_action_wrappers() {
        for response in ["I'm Iris, your local AI assistant.", "The tavern door opens. What do you do?", r#"{"character":"wizard"}"#] {
            let AgentAction::Finish { message } = AgentAction::from_response(response).unwrap() else { panic!("Expected a conversational reply"); };
            let mut state = task();
            state.finish(&message).unwrap();
            assert_eq!(state.status, TaskStatus::Completed);
            assert_eq!(state.last_message, response);
            assert!(state.steps.is_empty());
        }
    }

    #[test]
    fn conversational_replies_preserve_structured_action_validation() {
        assert!(matches!(AgentAction::from_response("```json\n{\"action\":\"tool\",\"tool\":\"list_project_tools\",\"arguments\":{}}\n```").unwrap(), AgentAction::Tool { .. }));
        assert!(AgentAction::from_response(r#"{"action":"tool","tool":"write"}"#).is_err());
        assert!(AgentAction::from_response(r#"{"action":"unknown"}"#).is_err());
        assert!(AgentAction::from_response(r#"{"action":"tool""#).is_err());
        assert!(AgentAction::from_response("  ").is_err());
    }

    #[test]
    fn narrated_tool_requests_are_not_accepted_as_answers() {
        assert!(AgentAction::from_response("Let me search.\n{\"action\":\"tool\",\"tool\":\"wikipedia_search\",\"arguments\":{}}\nThis will find it.").is_err());
        assert!(AgentAction::from_response("Ordinary code: `const options = {color: 'blue'}`").is_ok());
    }

    #[test]
    fn conversational_replies_ignore_predicted_results_after_a_tool_request() {
        let response = "{\"action\":\"tool\",\"tool\":\"perform_arithmetic\",\"arguments\":{}}\n{\"result\":703}";
        let AgentAction::Tool {tool,arguments} = AgentAction::from_response(response).unwrap() else {panic!("Only the first action should execute");};
        assert_eq!(tool,"perform_arithmetic");
        assert_eq!(arguments,serde_json::json!({}));
    }

    #[test]
    fn action_protocol_does_not_depend_on_native_tool_support() {
        assert!(matches!(AgentAction::parse(r#"{"action":"tool","tool":"list_project_tools","arguments":{}}"#).unwrap(), AgentAction::Tool { .. }));
        assert!(AgentAction::parse("I will use a tool").is_err());
        assert!(AgentAction::parse(r#"{"action":"unknown"}"#).is_err());
    }

    #[test]
    fn state_roundtrips_without_losing_evidence() {
        let mut state = task();
        state.plan(vec![step()], None).unwrap();
        state.observe("read", true, true, false, "result");
        let saved = serde_json::to_string(&state).unwrap();
        let restored: AgentTask = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.steps[0].id, 1);
        assert_eq!(restored.observations[0].id, 1);
    }

    #[test]
    fn excerpt_preserves_unicode_and_marks_partial_results() {
        let text = "\u{1f3e0}".repeat(400);
        let shortened = excerpt(&text, 120);
        assert!(shortened.chars().count() <= 120);
        assert!(shortened.contains("truncated"));
        assert_eq!(excerpt("complete", 120), "complete");
    }
}