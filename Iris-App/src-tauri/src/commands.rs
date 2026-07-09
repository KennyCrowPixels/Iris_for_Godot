use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use sysinfo::System;
use regex::Regex;


use reqwest;
use scraper::{Html, Selector};
use serde_json;
use reqwest::blocking::Client;
use serde_json::Value;
use std::time::Duration;
use std::collections::{HashMap, HashSet, VecDeque};
use std::collections::hash_map::DefaultHasher;
use serde::{Deserialize, Serialize};
use tauri::{Manager, Emitter};
use std::hash::{Hash, Hasher};

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct UserTurnPayload {
  pub tab_id: u32,
  pub input_text: String,
  pub images: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct OllamaToolCallFunction {
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub arguments: serde_json::Value,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct OllamaToolCall {
  #[serde(default)]
  pub function: OllamaToolCallFunction,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OllamaMessage {
  pub role: String,
  pub content: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub images: Option<Vec<String>>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AgentCompiledContext {
  pub system_prompt: String,
  pub messages: Vec<OllamaMessage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecommendedToolPolicy {
  AllowAll,
  ReadOnly,
  NoTools,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RedirectStyle {
  Neutral,
  GentleRedirect,
  ProtectiveRedirect,
}

#[derive(Clone, Debug)]
struct IntegrityAssessment {
  integrity_score: f32,
  confidence: f32,
  tags: Vec<String>,
  recommended_tool_policy: RecommendedToolPolicy,
  redirect_style: RedirectStyle,
  momentum_score: f32,
}

#[derive(Clone, Debug, Default)]
struct IntegrityMomentumState {
  score: f32,
  updated_at_secs: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OllamaChatRequest {
  pub model: String,
  pub messages: Vec<OllamaMessage>,
  pub stream: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub keep_alive: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub tools: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct OllamaChatStreamMessage {
  #[serde(default)]
  pub content: String,
  #[serde(default)]
  pub tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct OllamaChatStreamChunk {
  #[serde(default)]
  pub message: Option<OllamaChatStreamMessage>,
  #[serde(default)]
  pub done: bool,
  #[serde(default)]
  pub error: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", content = "payload")]
#[allow(dead_code)]
pub enum IrisEvent {
  Status(String),
  Delta(String),
  GodotError(String),
  PermissionRequest {
    req_id: String,
    action: String,
    target: String,
    risk_level: String, // "safe", "warn", "critical"
  },
  ToolResult {
    tool_name: String,
    status: String,
  },
  Error(String),
  Done,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ResolvePermissionPayload {
  pub req_id: String,
  pub approved: bool,
}

// Extended ChatMessage with a unix timestamp (defaults to 0 when missing on disk)
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub time: i64, // unix seconds
}



use std::{fs, path::PathBuf};
use std::io::{Write, BufRead, BufReader};
use std::time::{SystemTime, UNIX_EPOCH};

static OLLAMA_START_ATTEMPTED: AtomicBool = AtomicBool::new(false);
static GODOT_WATCHERS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
static INTEGRITY_MOMENTUM: OnceLock<Mutex<HashMap<u32, IntegrityMomentumState>>> = OnceLock::new();
static INTEGRITY_BAND_NORMAL: AtomicU64 = AtomicU64::new(0);
static INTEGRITY_BAND_CAUTION: AtomicU64 = AtomicU64::new(0);
static INTEGRITY_BAND_PROTECTIVE: AtomicU64 = AtomicU64::new(0);
static INTEGRITY_POLICY_ALLOW_ALL: AtomicU64 = AtomicU64::new(0);
static INTEGRITY_POLICY_READ_ONLY: AtomicU64 = AtomicU64::new(0);
static INTEGRITY_POLICY_NO_TOOLS: AtomicU64 = AtomicU64::new(0);
static INTEGRITY_BRIDGE_APPLIED: AtomicU64 = AtomicU64::new(0);

const OLLAMA_BASE: &str = "http://127.0.0.1:11434";
// <-- your custom tag created via `ollama create iris-organizer -f ...`
const MODEL_TAG: &str = "iris-organizer:latest";
const INTEGRITY_NORMAL_BAND: f32 = 0.80;
const INTEGRITY_CAUTION_BAND: f32 = 0.60;

#[tauri::command]
pub async fn submit_turn(
    app: tauri::AppHandle,
    payload: UserTurnPayload,
    window: tauri::Window,
) -> Result<(), String> {
    let event_name = format!("iris_event_{}", payload.tab_id);

    // MCP project handoff (non-Godot requests on a project tab with an active MCP)
    if try_external_handoff(&app, &payload, &window, &event_name) {
      println!("[Routing] MCP Handoff Payload");
      return Ok(());
    }

    // Standard chat path — attach tools only when the user's request actually needs them.
    println!("[Routing] Standard Chat Payload (intent-gated tools)");

    match compile_agent_context(&app, &payload).await {
      Ok(context) => {
        let selected_model = resolve_submit_model(&app, &payload);
        let historical_count = context.messages.len().saturating_sub(1);
        let msg = format!(
          "Rust Context Built: {} historical messages loaded. System Prompt length: {}. Model: {}",
          historical_count,
          context.system_prompt.len(),
          selected_model
        );
        let _ = window.emit(&event_name, IrisEvent::Status(msg));

        let session_summary = format!(
          "tab={} history_messages={} prompt_chars={} model={}",
          payload.tab_id,
          historical_count,
          context.system_prompt.chars().count(),
          selected_model
        );
        let _ = export_shared_session_markdown(&app, &context, &session_summary);

        // Integrity is prompt-specific; momentum only shapes redirect tone and never hard-locks chat.
        let mut integrity = assess_integrity_for_prompt(&payload.input_text);
        apply_integrity_momentum(payload.tab_id, &mut integrity);
        let attach_tools_by_intent = request_warrants_tools(&payload.input_text, !payload.images.is_empty());
        let tool_policy = resolve_turn_tool_policy(attach_tools_by_intent, &integrity);
        record_integrity_metrics(&integrity, &tool_policy);
        let bridge_note = build_ephemeral_bridge_note(&integrity);

        let _ = window.emit(&event_name, IrisEvent::Status(format!(
          "Integrity {:.2} (momentum {:.2}, confidence {:.2}) | policy {} | tags {}",
          integrity.integrity_score,
          integrity.momentum_score,
          integrity.confidence,
          tool_policy_label(&tool_policy),
          if integrity.tags.is_empty() { "none".to_string() } else { integrity.tags.join(",") }
        )));

        let request = build_ollama_chat_request(context, selected_model, tool_policy, bridge_note);
        match execute_ollama_chat_stream(&window, &event_name, &request).await {
          Ok(reply) => {
            let _ = persist_rust_turn_snapshot(&app, payload.tab_id, &payload.input_text, &reply);
          }
          Err(_) => {
            let _ = persist_rust_turn_snapshot(&app, payload.tab_id, &payload.input_text, "");
          }
        }
      }
      Err(e) => {
        let _ = window.emit(&event_name, IrisEvent::Error(e));
        let _ = window.emit(&event_name, IrisEvent::Done);
      }
    }

    Ok(())
}

fn build_agent_tool_section(flags: &SetupFlags) -> String {
  let mut features = Vec::new();
  features.push(format!("ollama_verified={}", flags.ollama_verified));
  features.push(format!("models_verified={}", flags.models_verified));
  features.push(format!("repos={}", flags.repos_enabled));
  features.push(format!("mcp={}", flags.mcp_enabled));
  features.push(format!("desktop_tools={}", flags.desktop_tools_enabled));
  features.push(format!("network={}", flags.network_enabled));
  features.push(format!("universal_dataweb={}", flags.universal_dataweb_enabled));
  format!(
    "Available backend capabilities: {}. When current weather, breaking news, recent headlines, or live web facts are requested and the matching tool exists, you must invoke the appropriate tool instead of claiming that you lack real-time access.",
    features.join(", ")
  )
}

pub async fn compile_agent_context(
  app_handle: &tauri::AppHandle,
  payload: &UserTurnPayload,
) -> Result<AgentCompiledContext, String> {
  let flags = read_setup_flags(app_handle);
  let persona = load_persona_prompt();
  let snapshot = load_tab(app_handle, payload.tab_id)?;

  let mut messages: Vec<OllamaMessage> = snapshot
    .messages
    .into_iter()
    .filter_map(|message| match message.role.as_str() {
      "user" => Some(OllamaMessage { role: "user".to_string(), content: message.text, images: None, tool_calls: None }),
      "llm" => Some(OllamaMessage { role: "assistant".to_string(), content: message.text, images: None, tool_calls: None }),
      _ => None,
    })
    .collect();

  const MAX_HISTORY_MESSAGES: usize = 40;
  if messages.len() > MAX_HISTORY_MESSAGES {
    let keep_from = messages.len().saturating_sub(MAX_HISTORY_MESSAGES);
    messages = messages.split_off(keep_from);
  }

  messages.push(OllamaMessage {
    role: "user".to_string(),
    content: payload.input_text.clone(),
    images: {
      let normalized = normalize_ollama_images(&payload.images);
      if normalized.is_empty() { None } else { Some(normalized) }
    },
    tool_calls: None,
  });

  let assistant_name = if flags.assistant_name.trim().is_empty() {
    "Iris".to_string()
  } else {
    flags.assistant_name.clone()
  };

  let system_prompt = format!(
    "{}\n\nRuntime identity:\n- Your display name is {}.\n- You are a local companion: loyal, practical, and supportive.\n- Keep responses concise, clear, and emotionally grounded.\n- Protect the user and their workspace through constructive alternatives, not hard shutdown language.\n- Never expose raw tool invocation JSON, parameter objects, or internal payloads to the user.\n- For simple arithmetic, provide the plain numeric result and only add explanation if it helps the user.\n- If a screenshot has already been captured for the current request, do not ask whether you should proceed with capture; continue with the existing image unless the user explicitly says it is wrong or insufficient.\n\nStyle directives:\n- Use warm, direct language and concrete next steps.\n- When a request is risky, redirect smoothly to safe, useful options in Iris' wheelhouse.\n- Avoid corporate boilerplate or detached policy-jargon phrasing.\n\n{}",
    persona,
    assistant_name,
    build_agent_tool_section(&flags)
  );

  Ok(AgentCompiledContext { system_prompt, messages })
}

fn tool_policy_label(policy: &RecommendedToolPolicy) -> &'static str {
  match policy {
    RecommendedToolPolicy::AllowAll => "allow_all",
    RecommendedToolPolicy::ReadOnly => "read_only",
    RecommendedToolPolicy::NoTools => "no_tools",
  }
}

fn clamp01(v: f32) -> f32 {
  if v < 0.0 {
    0.0
  } else if v > 1.0 {
    1.0
  } else {
    v
  }
}

fn now_unix_secs() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_secs() as i64)
    .unwrap_or(0)
}

fn destructive_ops_regex() -> &'static Regex {
  static RE: OnceLock<Regex> = OnceLock::new();
  RE.get_or_init(|| {
    Regex::new(
      r"(?i)\b(rm\s+-rf|del\s+/[sqf]|format\s+[a-z]:|wipe|delete\s+(everything|all files|root|system32)|drop\s+database|truncate\s+table)\b"
    ).expect("valid destructive regex")
  })
}

fn malware_or_abuse_regex() -> &'static Regex {
  static RE: OnceLock<Regex> = OnceLock::new();
  RE.get_or_init(|| {
    Regex::new(
      r"(?i)\b(keylogger|ransomware|payload|backdoor|credential\s+stealer|token\s+stealer|phishing|malware)\b"
    ).expect("valid malware regex")
  })
}

fn deception_regex() -> &'static Regex {
  static RE: OnceLock<Regex> = OnceLock::new();
  RE.get_or_init(|| {
    Regex::new(
      r"(?i)\b(hide\s+this|undetected|bypass|evade|spoof|impersonate|stealth|without\s+them\s+knowing)\b"
    ).expect("valid deception regex")
  })
}

fn credential_abuse_regex() -> &'static Regex {
  static RE: OnceLock<Regex> = OnceLock::new();
  RE.get_or_init(|| {
    Regex::new(
      r"(?i)\b(exfiltrate|dump\s+passwords|steal\s+credentials|harvest\s+tokens|session\s+hijack)\b"
    ).expect("valid credential regex")
  })
}

fn benign_guardrail_regex() -> &'static Regex {
  static RE: OnceLock<Regex> = OnceLock::new();
  RE.get_or_init(|| {
    Regex::new(
      r"(?i)\b(dry\s+run|simulation|sandbox|safe\s+mode|backup|restore\s+plan|explain\s+risks)\b"
    ).expect("valid benign regex")
  })
}

fn assess_integrity_for_prompt(user_text: &str) -> IntegrityAssessment {
  let text = user_text.to_lowercase();
  let trimmed = text.trim();

  if trimmed.is_empty() {
    return IntegrityAssessment {
      integrity_score: 1.0,
      confidence: 0.40,
      tags: vec!["empty_prompt".to_string()],
      recommended_tool_policy: RecommendedToolPolicy::AllowAll,
      redirect_style: RedirectStyle::Neutral,
      momentum_score: 1.0,
    };
  }

  let mut score = 1.0_f32;
  let mut tags: Vec<String> = Vec::new();
  let mut matches = 0_u32;

  if destructive_ops_regex().is_match(trimmed) {
    score -= 0.45;
    tags.push("destructive_ops".to_string());
    matches += 1;
  }
  if malware_or_abuse_regex().is_match(trimmed) {
    score -= 0.40;
    tags.push("malware_or_abuse".to_string());
    matches += 1;
  }
  if deception_regex().is_match(trimmed) {
    score -= 0.22;
    tags.push("deception".to_string());
    matches += 1;
  }
  if credential_abuse_regex().is_match(trimmed) {
    score -= 0.35;
    tags.push("credential_abuse".to_string());
    matches += 1;
  }
  if benign_guardrail_regex().is_match(trimmed) {
    score += 0.10;
    tags.push("benign_guardrails".to_string());
    matches += 1;
  }

  score = clamp01(score);

  let policy = if score < INTEGRITY_CAUTION_BAND {
    RecommendedToolPolicy::NoTools
  } else if score < INTEGRITY_NORMAL_BAND {
    RecommendedToolPolicy::ReadOnly
  } else {
    RecommendedToolPolicy::AllowAll
  };

  let redirect_style = if score < INTEGRITY_CAUTION_BAND {
    RedirectStyle::ProtectiveRedirect
  } else if score < INTEGRITY_NORMAL_BAND {
    RedirectStyle::GentleRedirect
  } else {
    RedirectStyle::Neutral
  };

  IntegrityAssessment {
    integrity_score: score,
    confidence: (0.45 + (matches as f32 * 0.15)).min(0.95),
    tags,
    recommended_tool_policy: policy,
    redirect_style,
    momentum_score: score,
  }
}

fn apply_integrity_momentum(tab_id: u32, assessment: &mut IntegrityAssessment) {
  let now = now_unix_secs();
  let momentum_map = INTEGRITY_MOMENTUM.get_or_init(|| Mutex::new(HashMap::new()));
  let mut map = match momentum_map.lock() {
    Ok(guard) => guard,
    Err(_) => return,
  };

  let prev = map.get(&tab_id).cloned().unwrap_or_default();
  let elapsed = (now - prev.updated_at_secs).max(0) as f32;
  let decay = if prev.updated_at_secs <= 0 {
    0.0
  } else {
    (-(elapsed / 180.0)).exp()
  };
  let decayed = if prev.updated_at_secs <= 0 {
    assessment.integrity_score
  } else {
    prev.score * decay
  };

  // Momentum influences only redirection tone, not hard policy lockout.
  let blended = clamp01(assessment.integrity_score * 0.8 + decayed * 0.2);
  assessment.momentum_score = blended;
  if assessment.integrity_score >= INTEGRITY_CAUTION_BAND && blended < INTEGRITY_NORMAL_BAND {
    assessment.redirect_style = RedirectStyle::GentleRedirect;
  }

  map.insert(
    tab_id,
    IntegrityMomentumState {
      score: blended,
      updated_at_secs: now,
    },
  );
}

fn resolve_turn_tool_policy(
  attach_tools_by_intent: bool,
  assessment: &IntegrityAssessment,
) -> RecommendedToolPolicy {
  if !attach_tools_by_intent {
    RecommendedToolPolicy::NoTools
  } else {
    assessment.recommended_tool_policy.clone()
  }
}

fn build_ephemeral_bridge_note(assessment: &IntegrityAssessment) -> Option<String> {
  if assessment.integrity_score >= INTEGRITY_NORMAL_BAND {
    return None;
  }

  INTEGRITY_BRIDGE_APPLIED.fetch_add(1, Ordering::Relaxed);
  let note = match assessment.redirect_style {
    RedirectStyle::ProtectiveRedirect => {
      "[System Integrity Note: The current prompt carries potentially destructive or deceptive intent. Do not use corporate disclaimers. Stay warm and loyal; redirect the user toward safe, practical alternatives that protect their files, environment, and goals.]"
    }
    RedirectStyle::GentleRedirect => {
      "[System Integrity Note: The current prompt appears risky or ambiguous. Keep a supportive tone and steer toward safer, concrete options while preserving user momentum.]"
    }
    RedirectStyle::Neutral => {
      "[System Integrity Note: Maintain constructive and grounded guidance.]"
    }
  };
  Some(note.to_string())
}

fn record_integrity_metrics(assessment: &IntegrityAssessment, policy: &RecommendedToolPolicy) {
  if assessment.integrity_score >= INTEGRITY_NORMAL_BAND {
    INTEGRITY_BAND_NORMAL.fetch_add(1, Ordering::Relaxed);
  } else if assessment.integrity_score >= INTEGRITY_CAUTION_BAND {
    INTEGRITY_BAND_CAUTION.fetch_add(1, Ordering::Relaxed);
  } else {
    INTEGRITY_BAND_PROTECTIVE.fetch_add(1, Ordering::Relaxed);
  }

  match policy {
    RecommendedToolPolicy::AllowAll => {
      INTEGRITY_POLICY_ALLOW_ALL.fetch_add(1, Ordering::Relaxed);
    }
    RecommendedToolPolicy::ReadOnly => {
      INTEGRITY_POLICY_READ_ONLY.fetch_add(1, Ordering::Relaxed);
    }
    RecommendedToolPolicy::NoTools => {
      INTEGRITY_POLICY_NO_TOOLS.fetch_add(1, Ordering::Relaxed);
    }
  }
}

fn request_warrants_tools(input_text: &str, has_images: bool) -> bool {
  if has_images {
    return true;
  }

  let lower = input_text.to_lowercase();
  if contains_any(&lower, &[
    "help me think",
    "think through",
    "brainstorm",
    "ethical",
    "ethics",
    "laws of robotics",
    "robotics laws",
    "principles",
    "alignment",
  ]) {
    return false;
  }

  contains_any(&lower, &[
    "screenshot",
    "screen shot",
    "weather",
    "forecast",
    "temperature",
    "current",
    "latest",
    "news",
    "search",
    "look up",
    "find on the web",
    "calculate",
    "math",
    "compute",
    "what is 2 +",
    "what is 3 +",
    "what is 4 +",
    "what is 5 +",
    "how much is",
    "mcp",
    "bridge",
    "inspect",
    "scan",
    "refresh repo",
    "pull from remote",
    "fetch updates",
    "update repo",
    "open devtools",
  ])
}

fn is_read_only_tool_schema(tool: &serde_json::Value) -> bool {
  let name = tool
    .pointer("/function/name")
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_lowercase();

  match name.as_str() {
    "universal_web_search" | "get_current_weather" | "perform_arithmetic" => true,
    _ => false,
  }
}

fn filter_tools_by_policy(
  all_tools: Vec<serde_json::Value>,
  policy: &RecommendedToolPolicy,
) -> Option<Vec<serde_json::Value>> {
  match policy {
    RecommendedToolPolicy::AllowAll => Some(all_tools),
    RecommendedToolPolicy::ReadOnly => {
      let filtered: Vec<serde_json::Value> = all_tools
        .into_iter()
        .filter(|tool| is_read_only_tool_schema(tool))
        .collect();
      if filtered.is_empty() { None } else { Some(filtered) }
    }
    RecommendedToolPolicy::NoTools => None,
  }
}

fn build_ollama_chat_request(
  context: AgentCompiledContext,
  model: String,
  tool_policy: RecommendedToolPolicy,
  bridge_note: Option<String>,
) -> OllamaChatRequest {
  let mut messages = Vec::with_capacity(context.messages.len() + 1);
  messages.push(OllamaMessage {
    role: "system".to_string(),
    content: context.system_prompt,
    images: None,
    tool_calls: None,
  });
  if let Some(note) = bridge_note {
    messages.push(OllamaMessage {
      role: "system".to_string(),
      content: note,
      images: None,
      tool_calls: None,
    });
  }
  messages.extend(context.messages);

  OllamaChatRequest {
    model,
    messages,
    stream: true,
    keep_alive: Some("90s".to_string()),
    tools: filter_tools_by_policy(build_default_tools(), &tool_policy),
  }
}

fn normalize_ollama_image_data(input: &str) -> Option<String> {
  let trimmed = input.trim();
  if trimmed.is_empty() {
    return None;
  }
  if let Some(rest) = trimmed.strip_prefix("data:") {
    if let Some(idx) = rest.find(',') {
      let data = rest[idx + 1..].trim();
      if !data.is_empty() {
        return Some(data.to_string());
      }
    }
  }
  Some(trimmed.to_string())
}

fn normalize_ollama_images(images: &[String]) -> Vec<String> {
  images.iter().filter_map(|v| normalize_ollama_image_data(v)).collect()
}

fn looks_like_tool_payload_text(text: &str) -> bool {
  let trimmed = text.trim();
  let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
    return false;
  };
  let Some(obj) = value.as_object() else {
    return false;
  };
  obj.contains_key("tool_calls")
    || obj.contains_key("name")
    || obj.contains_key("parameters")
    || (obj.contains_key("type") && obj.contains_key("properties"))
    || (obj.contains_key("operation") && obj.contains_key("x") && obj.contains_key("y"))
}

fn is_screenshot_intent_text(input: &str) -> bool {
  let lower = input.to_lowercase();
  contains_any(&lower, &[
    "screenshot",
    "screen shot",
    "capture screen",
    "screen capture",
    "take a screenshot",
    "look at my screen",
    "analyze screenshot",
    "what is on my screen",
    "what's on my screen",
  ])
}

fn drain_ndjson_lines(buffer: &mut String) -> Vec<String> {
  let mut lines = Vec::new();
  while let Some(pos) = buffer.find('\n') {
    let line = buffer[..pos].trim().to_string();
    let remainder = buffer[pos + 1..].to_string();
    *buffer = remainder;
    if !line.is_empty() {
      lines.push(line);
    }
  }
  lines
}

async fn execute_ollama_chat_stream_inner(
  window: &tauri::Window,
  event_name: &str,
  request: &OllamaChatRequest,
) -> Result<String, String> {
  let client = reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(5))
    .timeout(Duration::from_secs(180))
    .build()
    .map_err(|e| format!("HTTP client build failed: {}", e))?;

  // ---------------------------------------------------------------------------
  // Pass 1: Initial chat request — detect tool_calls or stream direct response
  // ---------------------------------------------------------------------------
  let mut resp = client
    .post(format!("{}/api/chat", OLLAMA_BASE))
    .json(request)
    .send()
    .await
    .map_err(|e| format!("HTTP request failed: {}", e))?;

  if !resp.status().is_success() {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    return Err(format!("HTTP status {} from /api/chat: {}", status, body));
  }

  let mut ndjson_buffer = String::new();
  let mut full_text = String::new();
  let mut intercepted_tool_calls: Vec<OllamaToolCall> = Vec::new();
  let mut assistant_tool_call_content = String::new();

  'pass1: loop {
    let maybe_chunk = resp
      .chunk()
      .await
      .map_err(|e| format!("Byte stream read failed: {}", e))?;

    let chunk = match maybe_chunk {
      Some(c) => c,
      None => break 'pass1,
    };

    ndjson_buffer.push_str(&String::from_utf8_lossy(&chunk));

    for line in drain_ndjson_lines(&mut ndjson_buffer) {
      let parsed: OllamaChatStreamChunk = serde_json::from_str(&line)
        .map_err(|e| format!("JSON parsing failed: {} | line: {}", e, line))?;

      if let Some(err) = parsed.error {
        return Err(format!("Remote error: {}", err));
      }

      if let Some(ref msg) = parsed.message {
        if let Some(ref calls) = msg.tool_calls {
          if !calls.is_empty() {
            // Intercept tool call payload — pause stream emission for synthesis
            println!("[ToolCall] Intercepted {} tool call(s) from model", calls.len());
            intercepted_tool_calls = calls.clone();
            assistant_tool_call_content = msg.content.clone();
          }
        } else if !msg.content.is_empty() {
          full_text.push_str(&msg.content);
        }
      }

      if parsed.done {
        break 'pass1;
      }
    }
  }

  // Drain any remaining buffered NDJSON tail
  let tail = ndjson_buffer.trim().to_string();
  if !tail.is_empty() {
    if let Ok(parsed) = serde_json::from_str::<OllamaChatStreamChunk>(&tail) {
      if let Some(err) = parsed.error {
        return Err(format!("Remote error: {}", err));
      }
      if let Some(ref msg) = parsed.message {
        if let Some(ref calls) = msg.tool_calls {
          if !calls.is_empty() && intercepted_tool_calls.is_empty() {
            intercepted_tool_calls = calls.clone();
            assistant_tool_call_content = msg.content.clone();
          }
        } else if !msg.content.is_empty() {
          full_text.push_str(&msg.content);
        }
      }
    }
  }

  // If no tool_calls were intercepted, optionally synthesize a compatibility fallback
  // for models that answer with a real-time/network disclaimer instead of emitting tools.
  if intercepted_tool_calls.is_empty() {
    if let Some(fallback_tool_call) = derive_fallback_tool_call(request, &full_text) {
      println!("[ToolCall] Compatibility fallback triggered for {}", fallback_tool_call.function.name);
      intercepted_tool_calls.push(fallback_tool_call);
    } else {
      if !full_text.is_empty() {
        if looks_like_tool_payload_text(&full_text) {
          let safe_text = "I hit an internal tool-formatting issue while generating that reply. Please ask again, and I will respond in plain language.".to_string();
          let _ = window.emit(event_name, IrisEvent::Delta(safe_text.clone()));
          return Ok(safe_text);
        }
        let _ = window.emit(event_name, IrisEvent::Delta(full_text.clone()));
      }
      return Ok(full_text);
    }
  }

  // ---------------------------------------------------------------------------
  // Pass 2: Execute intercepted tool calls, append results, re-submit for synthesis
  // ---------------------------------------------------------------------------
  println!("[ToolCall] Executing {} intercepted tool call(s)", intercepted_tool_calls.len());
  let _ = window.emit(event_name, IrisEvent::Status(
    format!("Executing tool: {}...", intercepted_tool_calls[0].function.name)
  ));

  // Build follow-up message context
  let mut follow_up_messages = request.messages.clone();

  // Append the assistant message that contained the tool_calls
  follow_up_messages.push(OllamaMessage {
    role: "assistant".to_string(),
    content: assistant_tool_call_content,
    images: None,
    tool_calls: Some(intercepted_tool_calls.clone()),
  });

  // Execute each tool and append result as a tool role message
  let latest_user_text = latest_user_message(&request.messages).unwrap_or("").to_string();
  for tool_call in &intercepted_tool_calls {
    let tool_result = execute_tool_call(&tool_call.function.name, &tool_call.function.arguments, &latest_user_text).await;
    let normalized_tool_images = normalize_ollama_images(&tool_result.images);
    follow_up_messages.push(OllamaMessage {
      role: "tool".to_string(),
      content: tool_result.content,
      images: if normalized_tool_images.is_empty() { None } else { Some(normalized_tool_images) },
      tool_calls: None,
    });
  }

  // Second POST: synthesis request without tools to prevent recursion
  let follow_up_request = OllamaChatRequest {
    model: request.model.clone(),
    messages: follow_up_messages,
    stream: true,
    keep_alive: request.keep_alive.clone(),
    tools: None,
  };

  let mut synth_resp = client
    .post(format!("{}/api/chat", OLLAMA_BASE))
    .json(&follow_up_request)
    .send()
    .await
    .map_err(|e| format!("Tool synthesis HTTP request failed: {}", e))?;

  if !synth_resp.status().is_success() {
    let status = synth_resp.status();
    let body = synth_resp.text().await.unwrap_or_default();
    return Err(format!("Tool synthesis HTTP status {} from /api/chat: {}", status, body));
  }

  let mut synth_buffer = String::new();
  let mut synthesis_text = String::new();

  'pass2: loop {
    let maybe_chunk = synth_resp
      .chunk()
      .await
      .map_err(|e| format!("Synthesis stream read failed: {}", e))?;

    let chunk = match maybe_chunk {
      Some(c) => c,
      None => break 'pass2,
    };

    synth_buffer.push_str(&String::from_utf8_lossy(&chunk));

    for line in drain_ndjson_lines(&mut synth_buffer) {
      let parsed: OllamaChatStreamChunk = serde_json::from_str(&line)
        .map_err(|e| format!("Synthesis JSON parsing failed: {} | line: {}", e, line))?;

      if let Some(err) = parsed.error {
        return Err(format!("Synthesis remote error: {}", err));
      }

      if let Some(ref msg) = parsed.message {
        if !msg.content.is_empty() {
          synthesis_text.push_str(&msg.content);
          let _ = window.emit(event_name, IrisEvent::Delta(msg.content.clone()));
        }
      }

      if parsed.done {
        return Ok(synthesis_text);
      }
    }
  }

  Ok(synthesis_text)
}

async fn execute_ollama_chat_stream(
  window: &tauri::Window,
  event_name: &str,
  request: &OllamaChatRequest,
) -> Result<String, String> {
  let result = execute_ollama_chat_stream_inner(window, event_name, request).await;

  if let Err(err) = &result {
    if request.tools.is_some() {
      let retry_request = build_plain_text_retry_request(request);
      let _ = window.emit(event_name, IrisEvent::Status("Tool path failed; retrying in plain language...".to_string()));
      let retry_result = execute_ollama_chat_stream_inner(window, event_name, &retry_request).await;
      if retry_result.is_ok() {
        return retry_result;
      }
      let fallback = "I hit an internal reply-formatting issue, but I can still help. Please ask again in plain language and I will answer directly.".to_string();
      let _ = window.emit(event_name, IrisEvent::Delta(fallback.clone()));
      let _ = window.emit(event_name, IrisEvent::Done);
      return Ok(fallback);
    }

    let _ = window.emit(event_name, IrisEvent::Error(err.clone()));
  }

  let _ = window.emit(event_name, IrisEvent::Done);
  result
}

fn build_plain_text_retry_request(request: &OllamaChatRequest) -> OllamaChatRequest {
  let mut messages = Vec::with_capacity(request.messages.len() + 1);
  messages.push(OllamaMessage {
    role: "system".to_string(),
    content: "Answer in plain language only. Do not produce tool calls, JSON, or schema-like output.".to_string(),
    images: None,
    tool_calls: None,
  });
  messages.extend(request.messages.clone());

  OllamaChatRequest {
    model: request.model.clone(),
    messages,
    stream: request.stream,
    keep_alive: request.keep_alive.clone(),
    tools: None,
  }
}

// ---------------------------------------------------------------------------
// Ollama tool schema + tool call execution
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct ToolExecutionResult {
  content: String,
  images: Vec<String>,
}

fn build_default_tools() -> Vec<serde_json::Value> {
  vec![
    serde_json::json!({
      "type": "function",
      "function": {
        "name": "universal_web_search",
        "description": "Searches the web for current information, recent news, and factual data not available in training context. Use depth=shallow for quick 1-2 result lookups; depth=deep for broader 3-5 result news queries.",
        "parameters": {
          "type": "object",
          "properties": {
            "query": {
              "type": "string",
              "description": "The search query string to execute"
            },
            "depth": {
              "type": "string",
              "enum": ["shallow", "deep"],
              "description": "shallow: 1-2 results via DDG HTML scrape; deep: 3-5 results using RSS fallback for news queries"
            }
          },
          "required": ["query", "depth"]
        }
      }
    }),
    serde_json::json!({
      "type": "function",
      "function": {
        "name": "get_current_weather",
        "description": "Returns real-time weather forecast data for a city or region using the Open-Meteo API. Use when the user asks about weather, temperature, or forecast.",
        "parameters": {
          "type": "object",
          "properties": {
            "location": {
              "type": "string",
              "description": "City and optionally state or country, e.g. 'Durham, NC' or 'London, UK'"
            }
          },
          "required": ["location"]
        }
      }
    }),
    serde_json::json!({
      "type": "function",
      "function": {
        "name": "perform_arithmetic",
        "description": "Computes a basic arithmetic result and returns the numeric answer. Use for addition, subtraction, multiplication, or division.",
        "parameters": {
          "type": "object",
          "properties": {
            "operation": {
              "type": "string",
              "enum": ["add", "subtract", "multiply", "divide"]
            },
            "x": {
              "type": "number"
            },
            "y": {
              "type": "number"
            }
          },
          "required": ["operation", "x", "y"]
        }
      }
    }),
    serde_json::json!({
      "type": "function",
      "function": {
        "name": "capture_desktop_screenshot",
        "description": "Captures a screenshot for the current request. Use when the user asks you to inspect the screen, take a screenshot, or analyze visible UI state.",
        "parameters": {
          "type": "object",
          "properties": {
            "target_hint": {
              "type": "string",
              "description": "Optional window or application hint, e.g. 'iris-app' or 'Godot'"
            }
          },
          "required": []
        }
      }
    }),
  ]
}

fn choose_window_for_hint(target_hint: &str, windows: &[WindowInfo]) -> Option<WindowInfo> {
  let hint = target_hint.to_lowercase();
  let tokens = hint.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| t.len() >= 3);
  windows.iter().cloned().max_by_key(|w| {
    let title = w.title.to_lowercase();
    let proc = w.process_name.to_lowercase();
    let mut score = 0i32;
    if title.contains(&hint) || proc.contains(&hint) { score += 8; }
    for t in tokens.clone() {
      if title.contains(t) { score += 2; }
      if proc.contains(t) { score += 1; }
    }
    score
  }).filter(|w| {
    let title = w.title.to_lowercase();
    let proc = w.process_name.to_lowercase();
    title.contains(&hint) || proc.contains(&hint) || hint.split_whitespace().any(|t| title.contains(t) || proc.contains(t))
  })
}

fn capture_screenshot_for_hint(target_hint: Option<&str>) -> Result<(String, String), String> {
  if let Some(hint) = target_hint.map(|s| s.trim()).filter(|s| !s.is_empty()) {
    if let Ok(windows) = list_windows() {
      if let Some(window) = choose_window_for_hint(hint, &windows) {
        let base64 = take_window_screenshot(window.id)?;
        return Ok((base64, format!("Captured window screenshot for {} ({})", window.title, window.process_name)));
      }
    }
  }
  let base64 = take_screenshot()?;
  Ok((base64, "Captured full-screen screenshot".to_string()))
}

async fn execute_universal_search(query: &str, depth: &str) -> String {
  let client = match reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(3))
    .timeout(Duration::from_secs(8))
    .build()
  {
    Ok(c) => c,
    Err(_) => return "Search HTTP client initialization failed.".to_string(),
  };

  match depth {
    "shallow" => {
      let resp = client
        .get("https://lite.duckduckgo.com/lite/")
        .header("User-Agent", "iris-app-network-search/1.0")
        .query(&[("q", query)])
        .send()
        .await;
      if let Ok(r) = resp {
        if let Ok(body) = r.text().await {
          let hits = parse_duckduckgo_lite_hits(&body, 2);
          if !hits.is_empty() {
            return hits
              .iter()
              .map(|h| format!("{}: {}", h.title, h.snippet))
              .collect::<Vec<_>>()
              .join("\n");
          }
        }
      }
      "No shallow search results found.".to_string()
    }
    _ => {
      // deep: try DDG JSON then RSS fallback
      let ddg_resp = client
        .get("https://api.duckduckgo.com/")
        .header("User-Agent", "iris-app-network-search/1.0")
        .query(&[("q", query), ("format", "json"), ("no_html", "1"), ("skip_disambig", "1")])
        .send()
        .await;
      let mut hits: Vec<NetworkHit> = Vec::new();
      if let Ok(r) = ddg_resp {
        if let Ok(json) = r.json::<Value>().await {
          let abstract_text = json.get("AbstractText").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
          let abstract_url = json.get("AbstractURL").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
          if !abstract_text.is_empty() {
            hits.push(NetworkHit { title: "Result".to_string(), url: abstract_url, snippet: abstract_text, score: 1.0 });
          }
          if let Some(related) = json.get("RelatedTopics") {
            add_related_topics_hits(related, &mut hits);
          }
        }
      }
      if hits.is_empty() {
        let rss_resp = client
          .get("https://news.google.com/rss/search")
          .header("User-Agent", "iris-app-network-search/1.0")
          .query(&[("q", query), ("hl", "en-US"), ("gl", "US"), ("ceid", "US:en")])
          .send()
          .await;
        if let Ok(r) = rss_resp {
          if let Ok(body) = r.text().await {
            hits = parse_news_rss_hits(&body, 5);
          }
        }
      }
      if hits.is_empty() {
        return "No deep search results found.".to_string();
      }
      hits.iter()
        .take(5)
        .map(|h| format!("{}: {}", h.title, h.snippet))
        .collect::<Vec<_>>()
        .join("\n")
    }
  }
}

async fn execute_tool_call(name: &str, args: &serde_json::Value, latest_user_text: &str) -> ToolExecutionResult {
  println!("[ToolCall] Dispatching tool: {}", name);
  match name {
    "universal_web_search" => {
      let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
      let depth = args.get("depth").and_then(|v| v.as_str()).unwrap_or("shallow");
      if query.is_empty() {
        return ToolExecutionResult { content: "No query provided to universal_web_search.".to_string(), images: Vec::new() };
      }
      ToolExecutionResult { content: execute_universal_search(&query, depth).await, images: Vec::new() }
    }
    "get_current_weather" => {
      let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("").to_string();
      if location.is_empty() {
        return ToolExecutionResult { content: "No location provided to get_current_weather.".to_string(), images: Vec::new() };
      }
      match weather_lookup(location, None).await {
        Ok(result) => ToolExecutionResult {
          content: format!(
            "Weather for {}, {} ({}): {}\nSource: {}",
            result.city, result.region, result.day_label, result.summary, result.source_url
          ),
          images: Vec::new(),
        },
        Err(e) => ToolExecutionResult { content: format!("Weather lookup failed: {}", e), images: Vec::new() },
      }
    }
    "perform_arithmetic" => {
      let operation = args.get("operation").and_then(|v| v.as_str()).unwrap_or("");
      let x = parse_numeric_arg(args.get("x"));
      let y = parse_numeric_arg(args.get("y"));
      let (Some(x), Some(y)) = (x, y) else {
        return ToolExecutionResult { content: "Arithmetic invocation failed: numeric operands are required.".to_string(), images: Vec::new() };
      };
      let content = match operation {
        "add" => format_number_result(x + y),
        "subtract" => format_number_result(x - y),
        "multiply" => format_number_result(x * y),
        "divide" => {
          if y == 0.0 {
            "Division is undefined because the divisor is zero.".to_string()
          } else {
            format_number_result(x / y)
          }
        }
        _ => format!("Unsupported arithmetic operation: {}", operation),
      };
      ToolExecutionResult { content, images: Vec::new() }
    }
    "capture_desktop_screenshot" => {
      if !is_screenshot_intent_text(latest_user_text) {
        return ToolExecutionResult {
          content: "Screenshot tool skipped because the current user request did not ask for screenshot analysis.".to_string(),
          images: Vec::new(),
        };
      }
      let hint = args.get("target_hint").and_then(|v| v.as_str());
      match capture_screenshot_for_hint(hint) {
        Ok((base64, status)) => ToolExecutionResult {
          content: status,
          images: vec![base64],
        },
        Err(e) => ToolExecutionResult { content: format!("Screenshot capture failed: {}", e), images: Vec::new() },
      }
    }
    _ => ToolExecutionResult { content: format!("Unknown tool invocation: {}", name), images: Vec::new() },
  }
}

fn parse_numeric_arg(value: Option<&serde_json::Value>) -> Option<f64> {
  match value {
    Some(serde_json::Value::Number(n)) => n.as_f64(),
    Some(serde_json::Value::String(s)) => s.trim().parse::<f64>().ok(),
    _ => None,
  }
}

fn format_number_result(value: f64) -> String {
  if (value.fract()).abs() < 1e-9 {
    format!("{}", value.round() as i64)
  } else {
    let mut out = format!("{:.6}", value);
    while out.contains('.') && out.ends_with('0') {
      out.pop();
    }
    if out.ends_with('.') {
      out.pop();
    }
    out
  }
}

fn extract_weather_location_for_fallback(input: &str) -> Option<String> {
  if let Ok(re) = Regex::new(r"(?i)\b(?:in|for|at)\s+([a-z0-9\s,.'\-]+?)(?:\s+now)?\??\s*$") {
    if let Some(cap) = re.captures(input.trim()) {
      if let Some(m) = cap.get(1) {
        let location = m.as_str().trim().trim_matches(',').trim().to_string();
        if !location.is_empty() {
          return Some(location);
        }
      }
    }
  }
  None
}

fn latest_user_message(messages: &[OllamaMessage]) -> Option<&str> {
  messages
    .iter()
    .rev()
    .find(|msg| msg.role == "user")
    .map(|msg| msg.content.as_str())
}

fn derive_fallback_tool_call(request: &OllamaChatRequest, model_text: &str) -> Option<OllamaToolCall> {
  let user_text = latest_user_message(&request.messages)?;
  let user_lower = user_text.to_lowercase();
  let model_lower = model_text.to_lowercase();

  if let Ok(raw_json) = serde_json::from_str::<serde_json::Value>(model_text.trim()) {
    if let Some(name) = raw_json.get("name").and_then(|v| v.as_str()) {
      let parameters = raw_json.get("parameters").cloned().unwrap_or(serde_json::Value::Null);
      let normalized_name = match name {
        "add" | "subtract" | "multiply" | "divide" => "perform_arithmetic",
        other => other,
      };
      let normalized_args = match name {
        "add" | "subtract" | "multiply" | "divide" => serde_json::json!({
          "operation": name,
          "x": parameters.get("x").cloned().unwrap_or(serde_json::Value::Null),
          "y": parameters.get("y").cloned().unwrap_or(serde_json::Value::Null),
        }),
        _ => parameters,
      };
      return Some(OllamaToolCall {
        function: OllamaToolCallFunction {
          name: normalized_name.to_string(),
          arguments: normalized_args,
        },
      });
    }
  }

  let network_enabled = request
    .messages
    .first()
    .map(|msg| msg.content.to_lowercase().contains("network=true"))
    .unwrap_or(false);

  if !network_enabled {
    return None;
  }

  let realtime_disclaimer = model_lower.contains("don't have real-time")
    || model_lower.contains("do not have real-time")
    || model_lower.contains("not able to perform a network search")
    || model_lower.contains("check a reliable weather website")
    || model_lower.contains("weather.com")
    || model_lower.contains("accuweather");

  if !realtime_disclaimer {
    return None;
  }

  if user_lower.contains("weather") || user_lower.contains("forecast") || user_lower.contains("temperature") {
    if let Some(location) = extract_weather_location_for_fallback(user_text) {
      return Some(OllamaToolCall {
        function: OllamaToolCallFunction {
          name: "get_current_weather".to_string(),
          arguments: serde_json::json!({ "location": location }),
        },
      });
    }
  }

  if user_lower.contains("news") || user_lower.contains("headline") || user_lower.contains("current events") {
    let depth = if user_lower.contains("latest") || user_lower.contains("recent") || user_lower.contains("breaking") {
      "deep"
    } else {
      "shallow"
    };
    return Some(OllamaToolCall {
      function: OllamaToolCallFunction {
        name: "universal_web_search".to_string(),
        arguments: serde_json::json!({ "query": user_text, "depth": depth }),
      },
    });
  }

  None
}

fn persist_rust_turn_snapshot(
  app: &tauri::AppHandle,
  tab_id: u32,
  user_text: &str,
  assistant_text: &str,
) -> Result<(), String> {
  let path = tab_file(app, tab_id)?;
  let mut snap = if path.exists() {
    iris_read_snapshot_file(&path)?
  } else {
    Snapshot {
      tab_id: Some(tab_id),
      title: format!("Tab #{}", tab_id),
      ..Default::default()
    }
  };

  let ts = now_ts();
  if !user_text.trim().is_empty() {
    snap.messages.push(ChatMessage {
      role: "user".to_string(),
      text: user_text.to_string(),
      time: ts,
    });
  }
  if !assistant_text.trim().is_empty() {
    snap.messages.push(ChatMessage {
      role: "llm".to_string(),
      text: assistant_text.to_string(),
      time: ts,
    });
  }
  if snap.title.trim().is_empty() {
    snap.title = format!("Tab #{}", tab_id);
  }
  snap.tab_id = Some(tab_id);
  snap.last_updated = Some(ts);
  bound_snapshot_payload(&mut snap);
  iris_write_snapshot_file(&path, &snap)
}

fn godot_watchers() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
  GODOT_WATCHERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_godot_primary_request(input: &str) -> bool {
  let normalized = input.to_lowercase();
  let keywords = [
    "godot", "gdscript", "scene", "node", "shader", "tauri", "typescript", "rust", "repo", "build", "debug", "project",
  ];
  keywords.iter().any(|k| normalized.contains(k))
}

fn try_external_handoff(
  app: &tauri::AppHandle,
  payload: &UserTurnPayload,
  window: &tauri::Window,
  event_name: &str,
) -> bool {
  if is_godot_primary_request(&payload.input_text) {
    return false;
  }

  let snapshot_path = match tab_file(app, payload.tab_id) {
    Ok(p) => p,
    Err(_) => return false,
  };
  let snapshot = match iris_read_snapshot_file(&snapshot_path) {
    Ok(s) => s,
    Err(_) => return false,
  };
  let Some(project_id) = snapshot.associated_project_id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
    return false;
  };

  let store = read_repo_project_store(app);
  let Some(project) = store.projects.iter().find(|p| p.enabled && p.id == project_id) else {
    return false;
  };

  let Some(mcp) = store
    .mcps
    .iter()
    .find(|m| m.enabled && project.mcp_ids.iter().any(|id| id == &m.id))
    .cloned()
  else {
    return false;
  };

  let _ = window.emit(event_name, IrisEvent::Status(format!(
    "Out-of-scope request detected. Attempting MCP handoff via {}...",
    mcp.name
  )));

  if connect_mcp_server_inner(
    mcp.id.clone(),
    mcp.target.clone(),
    mcp.connection_type.clone(),
    mcp.launch_command.clone(),
    mcp.launch_args.clone(),
  ).is_err() {
    return false;
  }

  let tools = match mcp_list_tools_inner(
    mcp.id.clone(),
    mcp.target.clone(),
    mcp.connection_type.clone(),
    mcp.launch_command.clone(),
    mcp.launch_args.clone(),
  ) {
    Ok(t) => t,
    Err(_) => return false,
  };

  let preferred = ["chat", "ask", "query", "answer", "complete", "assistant"];
  let selected_tool = tools.iter().find_map(|tool| {
    let lower = tool.name.to_lowercase();
    if preferred.iter().any(|key| lower.contains(key)) {
      Some(tool.name.clone())
    } else {
      None
    }
  });

  let Some(tool_name) = selected_tool else {
    return false;
  };

  let args = serde_json::json!({
    "query": payload.input_text,
    "input": payload.input_text,
    "text": payload.input_text,
  });

  let response = match mcp_call_tool(
    mcp.id,
    mcp.target,
    mcp.connection_type,
    mcp.launch_command,
    mcp.launch_args,
    tool_name,
    args,
  ) {
    Ok(v) => v,
    Err(_) => return false,
  };

  let answer = response
    .get("text")
    .and_then(|v| v.as_str())
    .map(|s| s.to_string())
    .or_else(|| response.get("content").and_then(|v| v.as_str()).map(|s| s.to_string()))
    .or_else(|| response.get("result").and_then(|v| v.as_str()).map(|s| s.to_string()))
    .unwrap_or_else(|| response.to_string());

  let _ = window.emit(event_name, IrisEvent::Delta(answer));
  let _ = window.emit(event_name, IrisEvent::Done);
  true
}

fn resolve_submit_model(app: &tauri::AppHandle, payload: &UserTurnPayload) -> String {
  let flags = read_setup_flags(app);
  let profile = ModelProfile::parse(&flags.model_profile);
  let model_config = read_model_config(app);

  if !payload.images.is_empty() {
    if !model_config.vision_enabled {
      return "iris-organizer:latest".to_string();
    }
    return match profile {
      ModelProfile::Low | ModelProfile::Minimal => "moondream2:latest".to_string(),
      _ => "iris-vision:latest".to_string(),
    };
  }

  "iris-organizer:latest".to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackgroundQueueMode {
  Normal,
  Downgraded,
  Paused,
}

#[derive(Clone, Debug)]
struct BackgroundQueueGate {
  mode: BackgroundQueueMode,
  delay_ms: u64,
  network_hit_cap: usize,
  repo_entry_cap: usize,
}

fn evaluate_background_queue_gate() -> BackgroundQueueGate {
  let hw = match detect_hardware_profile() {
    Ok(h) => h,
    Err(_) => {
      return BackgroundQueueGate {
        mode: BackgroundQueueMode::Normal,
        delay_ms: 0,
        network_hit_cap: 8,
        repo_entry_cap: 2000,
      }
    }
  };

  if hw.detected_profile.eq_ignore_ascii_case("minimal") || hw.detected_profile.eq_ignore_ascii_case("low") || hw.vram_gb <= 4.0 {
    return BackgroundQueueGate {
      mode: BackgroundQueueMode::Paused,
      delay_ms: 450,
      network_hit_cap: 4,
      repo_entry_cap: 900,
    };
  }

  if hw.detected_profile.eq_ignore_ascii_case("medium") || hw.vram_gb < 8.0 {
    return BackgroundQueueGate {
      mode: BackgroundQueueMode::Downgraded,
      delay_ms: 150,
      network_hit_cap: 6,
      repo_entry_cap: 1400,
    };
  }

  BackgroundQueueGate {
    mode: BackgroundQueueMode::Normal,
    delay_ms: 0,
    network_hit_cap: 8,
    repo_entry_cap: 2000,
  }
}

fn unix_to_ymd(ts: i64) -> (i32, u32, u32) {
  let days = ts.div_euclid(86_400);
  let z = days + 719_468;
  let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
  let doe = z - era * 146_097;
  let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
  let y = yoe + era * 400;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let d = doy - (153 * mp + 2) / 5 + 1;
  let m = mp + if mp < 10 { 3 } else { -9 };
  let year = y + if m <= 2 { 1 } else { 0 };
  (year as i32, m as u32, d as u32)
}

fn export_shared_session_markdown(
  app: &tauri::AppHandle,
  context: &AgentCompiledContext,
  session_summary: &str,
) -> Result<PathBuf, String> {
  let base = memory_dir(app)?.join("shared_sync");
  create_dir_all(&base).map_err(|e| format!("Failed creating shared sync dir: {}", e))?;

  let ts = now_ts();
  let (year, month, day) = unix_to_ymd(ts);
  let file = base.join(format!("{:04}-{:02}-{:02}-session-log.md", year, month, day));

  let mut lines: Vec<String> = Vec::new();
  lines.push(format!("## Session {}", ts));
  lines.push(String::new());
  lines.push(format!("Summary: {}", session_summary));
  lines.push(String::new());
  lines.push("### System prompt".to_string());
  lines.push(context.system_prompt.clone());
  lines.push(String::new());
  lines.push("### Messages".to_string());
  for msg in &context.messages {
    lines.push(format!("- {}: {}", msg.role, msg.content.replace('\n', " ")));
  }
  lines.push(String::new());

  let mut out = if file.exists() {
    fs::read_to_string(&file).unwrap_or_default()
  } else {
    "# Iris Shared Session Sync\n\n".to_string()
  };
  out.push_str(&lines.join("\n"));
  atomic_write_text(&file, &out)?;
  Ok(file)
}

#[tauri::command]
pub fn start_godot_log_watcher(app: tauri::AppHandle, tab_id: u32, log_path: String) -> Result<String, String> {
  let path = PathBuf::from(log_path.trim());
  if !path.exists() || !path.is_file() {
    return Err("Godot log path is not a readable file".to_string());
  }

  let key = tab_id.to_string();
  let stop_flag = Arc::new(AtomicBool::new(false));
  {
    let mut map = godot_watchers().lock().map_err(|_| "Godot watcher lock poisoned".to_string())?;
    if let Some(existing) = map.remove(&key) {
      existing.store(true, Ordering::Relaxed);
    }
    map.insert(key.clone(), Arc::clone(&stop_flag));
  }

  let app_handle = app.clone();
  let event_name = format!("iris_event_{}", tab_id);
  thread::spawn(move || {
    let mut last_size: u64 = 0;
    let error_re = Regex::new(r"(?i)(error|exception|stack trace|fatal|failed)").ok();
    loop {
      if stop_flag.load(Ordering::Relaxed) {
        break;
      }

      if let Ok(meta) = fs::metadata(&path) {
        if meta.len() < last_size {
          last_size = 0;
        }
      }

      if let Ok(file) = fs::File::open(&path) {
        let mut reader = BufReader::new(file);
        use std::io::Seek;
        use std::io::SeekFrom;
        if reader.seek(SeekFrom::Start(last_size)).is_ok() {
          let mut line = String::new();
          loop {
            line.clear();
            let bytes = match reader.read_line(&mut line) {
              Ok(n) => n,
              Err(_) => 0,
            };
            if bytes == 0 {
              break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
              continue;
            }
            let looks_like_error = match &error_re {
              Some(re) => re.is_match(trimmed),
              None => trimmed.to_lowercase().contains("error"),
            };
            if looks_like_error {
              let _ = app_handle.emit(&event_name, IrisEvent::GodotError(trimmed.to_string()));
            }
          }
          if let Ok(pos) = reader.stream_position() {
            last_size = pos;
          }
        }
      }

      std::thread::sleep(Duration::from_millis(700));
    }
  });

  Ok("Godot log watcher started".to_string())
}

#[tauri::command]
pub fn stop_godot_log_watcher(tab_id: u32) -> Result<bool, String> {
  let key = tab_id.to_string();
  let mut map = godot_watchers().lock().map_err(|_| "Godot watcher lock poisoned".to_string())?;
  if let Some(flag) = map.remove(&key) {
    flag.store(true, Ordering::Relaxed);
    return Ok(true);
  }
  Ok(false)
}

#[tauri::command]
pub async fn resolve_permission(
    payload: ResolvePermissionPayload,
) -> Result<(), String> {
    // TEMPORARY STUB: Will interact with tokio::oneshot channels later
    println!("Permission resolved for {}: {}", payload.req_id, payload.approved);
    Ok(())
}

/// Which stdio framing protocol the MCP server speaks.
/// NDJSON = one JSON object per line (MCP Python SDK 1.x / official spec)
/// Lsp    = LSP Content-Length header framing (TypeScript MCP SDK / older servers)
#[derive(Debug, Clone, PartialEq)]
enum McpProtocol { Ndjson, Lsp }

struct McpStdioSession {
  child: std::process::Child,
  stdin: std::process::ChildStdin,
  stdout: BufReader<std::process::ChildStdout>,
  next_id: u64,
  initialized: bool,
  protocol: McpProtocol,
}

static MCP_STDIO_SESSIONS: OnceLock<Mutex<HashMap<String, McpStdioSession>>> = OnceLock::new();
static MCP_LAUNCHED_PROCS: OnceLock<Mutex<HashMap<String, std::process::Child>>> = OnceLock::new();
// Separate PID map so the kill-timer can kill a session process without acquiring the session lock.
static MCP_SESSION_PIDS: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();

fn mcp_sessions() -> &'static Mutex<HashMap<String, McpStdioSession>> {
  MCP_STDIO_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mcp_launched_procs() -> &'static Mutex<HashMap<String, std::process::Child>> {
  MCP_LAUNCHED_PROCS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mcp_session_pids() -> &'static Mutex<HashMap<String, u32>> {
  MCP_SESSION_PIDS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Kill a process by PID using the OS.  Does NOT require the mcp_sessions lock.
fn kill_pid_by_os(pid: u32) {
  #[cfg(target_os = "windows")]
  {
    let _ = Command::new("taskkill")
      .args(["/PID", &pid.to_string(), "/F"])
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn();
  }
  #[cfg(not(target_os = "windows"))]
  {
    let _ = Command::new("kill")
      .args(["-TERM", &pid.to_string()])
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn();
  }
}

/// Kill the stdio session process by PID (no sessions lock needed), then clean up the maps.
fn kill_mcp_by_pid_if_known(mcp_id: &str) {
  let pid = mcp_session_pids().lock().ok().and_then(|g| g.get(mcp_id).copied());
  if let Some(p) = pid {
    kill_pid_by_os(p);
  }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
  pub name: String,
  #[serde(default)]
  pub description: String,
  #[serde(default)]
  pub input_schema: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpConnectResult {
  pub connected: bool,
  pub pid: Option<u32>,
  /// Which stdio framing protocol was auto-detected ("ndjson" or "lsp"), or None for HTTP.
  pub protocol: Option<String>,
}

fn parse_mcp_target(raw: &str) -> (String, Option<String>, Vec<String>) {
  let s = raw.trim();
  if s.is_empty() {
    return ("url".to_string(), None, vec![]);
  }
  if s.starts_with('{') {
    if let Ok(parsed) = serde_json::from_str::<Value>(s) {
      let server = if parsed.get("command").is_some() {
        Some(parsed.clone())
      } else {
        parsed
          .get("mcpServers")
          .and_then(|m| m.as_object())
          .and_then(|o| o.values().next().cloned())
      };
      if let Some(server) = server {
        if let Some(cmd) = server.get("command").and_then(|v| v.as_str()) {
          let args = server
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_else(Vec::new);
          return ("stdio".to_string(), Some(cmd.to_string()), args);
        }
        if let Some(url) = server.get("url").and_then(|v| v.as_str()) {
          return ("url".to_string(), Some(url.to_string()), vec![]);
        }
      }
    }
  }
  if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("ws://") || s.starts_with("wss://") || s.starts_with("local://") {
    return ("url".to_string(), Some(s.to_string()), vec![]);
  }
  let parts: Vec<String> = s
    .split_whitespace()
    .map(|p| p.trim_matches('"').trim_matches('\'').to_string())
    .filter(|p| !p.is_empty())
    .collect();
  if parts.is_empty() {
    ("url".to_string(), None, vec![])
  } else {
    let mut args = parts[1..].to_vec();
    if parts[0].to_ascii_lowercase().ends_with("uv.exe") || parts[0].eq_ignore_ascii_case("uv") {
      if args.first().map(|a| a.eq_ignore_ascii_case("uv")).unwrap_or(false) {
        args = args.into_iter().skip(1).collect();
      }
    }
    ("stdio".to_string(), Some(parts[0].clone()), args)
  }
}

fn looks_like_http_url(s: &str) -> bool {
  let t = s.trim().to_ascii_lowercase();
  t.starts_with("http://") || t.starts_with("https://") || t.starts_with("ws://") || t.starts_with("wss://") || t.starts_with("local://")
}

fn resolve_executable(command: &str) -> Option<String> {
  let c = command.trim();
  if c.is_empty() {
    return None;
  }
  let looks_like_path = c.contains('/') || c.contains('\\') || c.contains(':');
  if looks_like_path && std::path::Path::new(c).exists() {
    return Some(c.to_string());
  }

  if cfg!(target_os = "windows") {
    if let Ok(out) = Command::new("where").arg(c).output() {
      if out.status.success() {
        let txt = String::from_utf8_lossy(&out.stdout);
        if let Some(first) = txt.lines().map(|l| l.trim()).find(|l| !l.is_empty()) {
          return Some(first.to_string());
        }
      }
    }
    if c.eq_ignore_ascii_case("uv") || c.eq_ignore_ascii_case("uv.exe") {
      let mut candidates: Vec<std::path::PathBuf> = vec![];
      if let Ok(local) = std::env::var("LOCALAPPDATA") {
        candidates.push(std::path::Path::new(&local).join("Programs").join("uv").join("uv.exe"));
        let winget_packages = std::path::Path::new(&local).join("Microsoft").join("WinGet").join("Packages");
        if let Ok(rd) = std::fs::read_dir(&winget_packages) {
          for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.to_ascii_lowercase().starts_with("astral-sh.uv_") {
              let uv = p.join("uv.exe");
              if uv.exists() {
                candidates.push(uv);
              }
            }
          }
        }
      }
      if let Ok(profile) = std::env::var("USERPROFILE") {
        candidates.push(std::path::Path::new(&profile).join(".local").join("bin").join("uv.exe"));
      }
      for p in candidates {
        if p.exists() {
          return Some(p.to_string_lossy().to_string());
        }
      }
    }
  }

  Some(c.to_string())
}

fn write_mcp_framed_json(stdin: &mut std::process::ChildStdin, value: &Value, protocol: &McpProtocol) -> Result<(), String> {
  match protocol {
    McpProtocol::Ndjson => {
      let mut payload = serde_json::to_vec(value).map_err(|e| format!("encode failed: {}", e))?;
      payload.push(b'\n');
      stdin.write_all(&payload).and_then(|_| stdin.flush()).map_err(|e| format!("write failed: {}", e))
    }
    McpProtocol::Lsp => {
      let payload = serde_json::to_vec(value).map_err(|e| format!("encode failed: {}", e))?;
      let header = format!("Content-Length: {}\r\n\r\n", payload.len());
      stdin.write_all(header.as_bytes())
        .and_then(|_| stdin.write_all(&payload))
        .and_then(|_| stdin.flush())
        .map_err(|e| format!("write failed: {}", e))
    }
  }
}

fn read_mcp_framed_json(stdout: &mut BufReader<std::process::ChildStdout>, protocol: &McpProtocol) -> Result<Value, String> {
  match protocol {
    McpProtocol::Ndjson => {
      loop {
        let mut line = String::new();
        let n = stdout.read_line(&mut line).map_err(|e| format!("read failed: {}", e))?;
        if n == 0 { return Err("MCP server closed stdout".to_string()); }
        let t = line.trim();
        if t.is_empty() { continue; }
        return serde_json::from_str(t)
          .map_err(|e| format!("NDJSON decode failed: {} (line: {})", e, &t[..t.len().min(120)]));
      }
    }
    McpProtocol::Lsp => {
      let mut content_length: Option<usize> = None;
      loop {
        let mut line = String::new();
        let n = stdout.read_line(&mut line).map_err(|e| format!("read header failed: {}", e))?;
        if n == 0 { return Err("MCP server closed stdout while reading LSP headers".to_string()); }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() { break; }
        if let Some((name, val)) = trimmed.split_once(':') {
          if name.trim().eq_ignore_ascii_case("Content-Length") {
            content_length = val.trim().parse::<usize>().ok();
          }
        }
      }
      let len = content_length.ok_or_else(|| "Missing Content-Length in LSP frame".to_string())?;
      let mut body = vec![0u8; len];
      use std::io::Read;
      stdout.read_exact(&mut body).map_err(|e| format!("LSP body read failed: {}", e))?;
      serde_json::from_slice::<Value>(&body).map_err(|e| format!("LSP JSON decode failed: {}", e))
    }
  }
}

fn mcp_stdio_request(session: &mut McpStdioSession, method: &str, params: Value) -> Result<Value, String> {
  let id = session.next_id;
  session.next_id = session.next_id.saturating_add(1);
  let req = serde_json::json!({
    "jsonrpc": "2.0",
    "id": id,
    "method": method,
    "params": params,
  });
  write_mcp_framed_json(&mut session.stdin, &req, &session.protocol)?;
  loop {
    let msg = read_mcp_framed_json(&mut session.stdout, &session.protocol)?;
    let mid = msg.get("id").and_then(|v| v.as_u64());
    if mid != Some(id) {
      continue;
    }
    if let Some(err) = msg.get("error") {
      return Err(format!("MCP {} failed: {}", method, err));
    }
    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
  }
}

fn mcp_stdio_notify(session: &mut McpStdioSession, method: &str, params: Value) -> Result<(), String> {
  let req = serde_json::json!({
    "jsonrpc": "2.0",
    "method": method,
    "params": params,
  });
  write_mcp_framed_json(&mut session.stdin, &req, &session.protocol)
}

/// Probe result: a successfully started + initialize-responded process ready to use as a session.
struct ProbeSuccess {
  pid: u32,
  protocol: McpProtocol,
  child: std::process::Child,
  stdin: std::process::ChildStdin,
  reader: BufReader<std::process::ChildStdout>,
}

/// Spawn a process, send an MCP `initialize` with the given protocol, wait up to `timeout` for a
/// JSON-RPC response.  If successful the LIVE process is returned (do not kill it).  If the probe
/// times-out or receives an error the process is killed and an Err is returned.
fn try_probe_protocol(
  command: &str,
  args: &[String],
  protocol: McpProtocol,
  timeout: std::time::Duration,
) -> Result<ProbeSuccess, String> {
  let resolved = resolve_executable(command).ok_or_else(|| format!("Command '{}' not found", command))?;

  let mut child = Command::new(&resolved)
    .args(args)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|e| {
      let is_uv = command.eq_ignore_ascii_case("uv") || command.eq_ignore_ascii_case("uv.exe");
      if is_uv { format!("Failed to launch '{}': {}. Install uv (https://docs.astral.sh/uv/).", command, e) }
      else { format!("Failed to launch '{}': {}", command, e) }
    })?;

  let pid = child.id();
  let mut stdin = child.stdin.take().ok_or_else(|| "stdin unavailable".to_string())?;
  let stdout = child.stdout.take().ok_or_else(|| "stdout unavailable".to_string())?;
  let mut reader = BufReader::new(stdout);

  // Send initialize probe.
  let init_msg = serde_json::json!({
    "jsonrpc": "2.0", "id": 1, "method": "initialize",
    "params": { "protocolVersion": "2024-11-05", "capabilities": {},
                 "clientInfo": { "name": "Iris", "version": "0.1.0" } }
  });
  if let Err(e) = write_mcp_framed_json(&mut stdin, &init_msg, &protocol) {
    let _ = child.kill(); let _ = child.wait();
    return Err(format!("{:?} probe write failed: {}", protocol, e));
  }

  // Read response with timeout via a thread + channel.
  // We move `reader` into the thread and get it back through the channel.
  let proto_t = protocol.clone();
  let (tx, rx) = std::sync::mpsc::channel::<(Result<Value, String>, BufReader<std::process::ChildStdout>)>();
  std::thread::spawn(move || {
    let result = read_mcp_framed_json(&mut reader, &proto_t);
    let _ = tx.send((result, reader)); // rx dropped on timeout; send error is ignored
  });

  match rx.recv_timeout(timeout) {
    Ok((Ok(_response), reader)) => {
      // Probe succeeded â€” return the live process for reuse as the real session.
      Ok(ProbeSuccess { pid, protocol, child, stdin, reader })
    }
    _ => {
      // Timeout or I/O error: kill the probe process and fall back to the next protocol.
      let _ = child.kill();
      let _ = child.wait();
      // Allow the detached reader thread time to see EOF and exit gracefully.
      std::thread::sleep(std::time::Duration::from_millis(50));
      Err(format!("{:?} protocol probe did not get a response within {:?}", protocol, timeout))
    }
  }
}

fn ensure_stdio_session(mcp_id: &str, command: &str, args: &[String]) -> Result<u32, String> {
  // Check for an existing live session â€” release the lock before probing.
  {
    let mut sessions = mcp_sessions().lock().map_err(|_| "MCP session lock poisoned".to_string())?;
    if let Some(existing) = sessions.get_mut(mcp_id) {
      if let Ok(None) = existing.child.try_wait() {
        return Ok(existing.child.id());
      }
      sessions.remove(mcp_id); // dead session: clean up
    }
  }

  let dangerous: &[char] = &[';', '&', '|', '`', '$', '>', '<', '\n', '\r'];
  if command.chars().any(|c| dangerous.contains(&c)) { return Err("Unsafe characters in command".to_string()); }
  for arg in args { if arg.chars().any(|c| dangerous.contains(&c)) { return Err("Unsafe characters in args".to_string()); } }

  // Auto-detect protocol: try NDJSON first (official MCP spec), then LSP framing (TypeScript SDK).
  // Each probe spawns a short-lived process and sends an `initialize` request with recv_timeout.
  let probe_timeout = std::time::Duration::from_secs(8);
  let probe = try_probe_protocol(command, args, McpProtocol::Ndjson, probe_timeout)
    .or_else(|ndjson_err| {
      try_probe_protocol(command, args, McpProtocol::Lsp, probe_timeout)
        .map_err(|lsp_err| format!(
          "MCP server did not respond to either protocol.\n  NDJSON: {}\n  LSP: {}",
          ndjson_err, lsp_err
        ))
    })?;

  let pid = probe.pid;
  let protocol = probe.protocol;

  // Store the live probe process as the real session (no second spawn needed).
  {
    let mut sessions = mcp_sessions().lock().map_err(|_| "MCP session lock poisoned".to_string())?;
    sessions.insert(mcp_id.to_string(), McpStdioSession {
      child: probe.child,
      stdin: probe.stdin,
      stdout: probe.reader,
      next_id: 2, // probe used id=1 for initialize
      initialized: false,
      protocol,
    });
  }
  if let Ok(mut pids) = mcp_session_pids().lock() {
    pids.insert(mcp_id.to_string(), pid);
  }
  Ok(pid)
}

fn kill_mcp_session_process(mcp_id: &str) {
  if let Ok(mut sessions) = mcp_sessions().lock() {
    if let Some(mut sess) = sessions.remove(mcp_id) {
      let _ = sess.child.kill();
      let _ = sess.child.wait();
    }
  }
  // Always clean up the pid map too.
  if let Ok(mut pids) = mcp_session_pids().lock() {
    pids.remove(mcp_id);
  }
}

fn ensure_stdio_initialized(mcp_id: &str, command: &str, args: &[String]) -> Result<u32, String> {
  // ensure_stdio_session already sent the `initialize` request as part of protocol detection.
  // We only need to send `notifications/initialized` to complete the handshake.
  let pid = ensure_stdio_session(mcp_id, command, args)?;
  let mut sessions = mcp_sessions().lock().map_err(|_| "MCP session lock poisoned".to_string())?;
  let session = sessions.get_mut(mcp_id).ok_or_else(|| "MCP session missing".to_string())?;
  if !session.initialized {
    let _ = mcp_stdio_notify(session, "notifications/initialized", serde_json::json!({}));
    session.initialized = true;
  }
  Ok(pid)
}

fn mcp_http_request(target_url: &str, method: &str, params: Value) -> Result<Value, String> {
  let client = Client::new();
  let req = serde_json::json!({
    "jsonrpc": "2.0",
    "id": 1,
    "method": method,
    "params": params,
  });
  let res = client
    .post(target_url)
    .json(&req)
    .send()
    .map_err(|e| format!("MCP HTTP request failed: {}", e))?;
  if !res.status().is_success() {
    return Err(format!("MCP HTTP status {}", res.status()));
  }
  let body: Value = res.json().map_err(|e| format!("MCP HTTP decode failed: {}", e))?;
  if let Some(err) = body.get("error") {
    return Err(format!("MCP {} failed: {}", method, err));
  }
  Ok(body.get("result").cloned().unwrap_or(Value::Null))
}

fn resolve_ollama_executable() -> String {
  if cfg!(target_os = "windows") {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
      let p = std::path::Path::new(&local)
        .join("Programs")
        .join("Ollama")
        .join("ollama.exe");
      if p.exists() {
        return p.to_string_lossy().to_string();
      }
    }
  }
  "ollama".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Artifact { pub lang: String, pub filename: Option<String>, pub content: String, pub ts: i64 }

#[derive(Serialize, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Msg { pub role: String, pub text: String, pub ts: i64 }

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TabMemory {
    pub tab_id: u32,
    pub title: String,
    pub messages: Vec<ChatMessage>,
    pub artifacts: Vec<Artifact>,
    pub micro_summary: String,
    pub dialogue_bullets: String,
    pub summary: String,
    pub is_closed: bool,
    pub last_updated: i64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CompiledContext {
    pub micro_summary: String,
    pub dialogue_bullets: String,
    pub recent_transcript: String,
    pub recent_artifacts: Vec<Artifact>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct Snapshot {
  #[serde(default)]
  pub tab_id: Option<u32>,
    pub title: String,
    pub messages: Vec<ChatMessage>,
    #[serde(rename = "associatedProjectId", alias = "associated_project_id")]
    #[serde(default)]
    pub associated_project_id: Option<String>,
    #[serde(rename = "microSummary", alias = "micro_summary")]
    #[serde(default)]
    pub micro_summary: String,
    #[serde(rename = "dialogueBullets", alias = "dialogue_bullets")]
    #[serde(default)]
    pub dialogue_bullets: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(rename = "promptHistory", alias = "prompt_history")]
    #[serde(default)]
    pub prompt_history: Vec<String>,
    #[serde(default)]
    pub last_updated: Option<i64>,
}

#[derive(Deserialize)]
pub struct UpdateTabMemoryArgs {
#[serde(alias="tab_id", alias="tabId")]
    pub tab_id: u32,
    pub summary: String,
    #[serde(alias="micro_summary", alias="microSummary")]
    pub micro_summary: String,
    #[serde(alias="dialogue_bullets", alias="dialogueBullets")]
    pub dialogue_bullets: String,
    #[serde(alias="new_message", alias="newMessage")]
    pub new_message: String,
    pub artifacts: Vec<Artifact>,
}

#[derive(Deserialize)]
pub struct UpdateSnapshotMemoryArgs {
  pub tab_id: u32,
  pub snapshot: Snapshot,
}

#[derive(Deserialize)]
pub struct ReadTabSnapshotArgs {
  pub key: String,
}

#[derive(Deserialize)]
pub struct RestoreFullTabMemoryArgs {
  #[serde(alias="tab_id", alias="tabId")]
  pub tab_id: u32,
  pub title: String,
  pub messages: Vec<ChatMessage>,
  pub artifacts: Vec<Artifact>,
  #[serde(alias="micro_summary", alias="microSummary")]
  pub micro_summary: String,
  #[serde(alias="dialogue_bullets", alias="dialogueBullets")]
  pub dialogue_bullets: String,
  pub summary: String,
  #[serde(alias="last_updated", alias="lastUpdated")]
  pub last_updated: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SetupFlags {
  #[serde(rename = "ollamaVerified")]
  pub ollama_verified: bool,
  #[serde(rename = "modelsVerified")]
  pub models_verified: bool,
  #[serde(rename = "interpretV2Enabled")]
  pub interpret_v2_enabled: bool,
  #[serde(rename = "modelProfile")]
  pub model_profile: String,
  #[serde(rename = "assistantName")]
  pub assistant_name: String,
  #[serde(rename = "themeColor")]
  pub theme_color: String,
  #[serde(rename = "themePreset")]
  pub theme_preset: String,
  #[serde(rename = "networkEnabled")]
  pub network_enabled: bool,
  #[serde(default)]
  #[serde(rename = "manualDownloadUrl")]
  pub manual_download_url: String,
  #[serde(default)]
  #[serde(rename = "releaseNotesUrl")]
  pub release_notes_url: String,
  #[serde(default)]
  #[serde(rename = "updateFeedUrl")]
  pub update_feed_url: String,
  #[serde(default)]
  #[serde(rename = "autoUpdatesEnabled")]
  pub auto_updates_enabled: bool,
  #[serde(default)]
  #[serde(rename = "reposEnabled")]
  pub repos_enabled: bool,
  #[serde(default)]
  #[serde(rename = "mcpEnabled")]
  pub mcp_enabled: bool,
  #[serde(default)]
  #[serde(rename = "desktopToolsEnabled")]
  pub desktop_tools_enabled: bool,
  #[serde(default)]
  #[serde(rename = "universalDatawebEnabled")]
  pub universal_dataweb_enabled: bool,
  #[serde(default = "default_color_mode")]
  #[serde(rename = "colorMode")]
  pub color_mode: String,
}

fn default_color_mode() -> String { "dark".to_string() }

impl Default for SetupFlags {
  fn default() -> Self {
    Self {
      ollama_verified: false,
      models_verified: false,
      interpret_v2_enabled: true,
      model_profile: "Medium".to_string(),
      assistant_name: "Iris".to_string(),
      theme_color: "#232323".to_string(),
      theme_preset: "Black".to_string(),
      network_enabled: false,
      manual_download_url: "".to_string(),
      release_notes_url: "".to_string(),
      update_feed_url: "".to_string(),
      auto_updates_enabled: false,
      repos_enabled: true,
      mcp_enabled: true,
      desktop_tools_enabled: false,
      universal_dataweb_enabled: true,
      color_mode: "dark".to_string(),
    }
  }
}

#[derive(Deserialize, Debug)]
pub struct SetupFlagsArgs {
  #[serde(default, alias = "ollamaVerified")]
  pub ollama_verified: Option<bool>,
  #[serde(default, alias = "modelsVerified")]
  pub models_verified: Option<bool>,
  #[serde(default, alias = "interpretV2Enabled")]
  pub interpret_v2_enabled: Option<bool>,
  #[serde(default, alias = "modelProfile")]
  pub model_profile: Option<String>,
  #[serde(default, alias = "assistantName")]
  pub assistant_name: Option<String>,
  #[serde(default, alias = "themeColor")]
  pub theme_color: Option<String>,
  #[serde(default, alias = "themePreset")]
  pub theme_preset: Option<String>,
  #[serde(default, alias = "networkEnabled")]
  pub network_enabled: Option<bool>,
  #[serde(default, alias = "manualDownloadUrl")]
  pub manual_download_url: Option<String>,
  #[serde(default, alias = "releaseNotesUrl")]
  pub release_notes_url: Option<String>,
  #[serde(default, alias = "updateFeedUrl")]
  pub update_feed_url: Option<String>,
  #[serde(default, alias = "autoUpdatesEnabled")]
  pub auto_updates_enabled: Option<bool>,
  #[serde(default, alias = "reposEnabled")]
  pub repos_enabled: Option<bool>,
  #[serde(default, alias = "mcpEnabled")]
  pub mcp_enabled: Option<bool>,
  #[serde(default, alias = "desktopToolsEnabled")]
  pub desktop_tools_enabled: Option<bool>,
  #[serde(default, alias = "universalDatawebEnabled")]
  pub universal_dataweb_enabled: Option<bool>,
  #[serde(default, alias = "colorMode")]
  pub color_mode: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryLanes {
  pub project: String,
  pub coding: String,
  pub recall: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct IntentScores {
  pub dev_task: f32,
  pub code_edit_followup: f32,
  pub math_units: f32,
  pub reference_recall: f32,
  pub general_knowledge: f32,
  pub banter_roleplay: f32,
  pub meta_identity: f32,
  pub clarification_repair: f32,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct LaneWeights {
  pub project: f32,
  pub coding: f32,
  pub recall: f32,
}

#[derive(Deserialize, Debug)]
pub struct InterpretTurnArgs {
  #[serde(alias = "tab_id", alias = "tabId")]
  pub tab_id: u32,
  #[serde(alias = "user_text", alias = "userText")]
  pub user_text: String,
  #[serde(default, alias = "token_budget", alias = "tokenBudget")]
  pub token_budget: Option<usize>,
  #[serde(default, alias = "use_coder", alias = "useCoder")]
  pub use_coder: Option<bool>,
  #[serde(default, alias = "coder_enabled", alias = "coderEnabled")]
  pub coder_enabled: Option<bool>,
  #[serde(default, alias = "vision_enabled", alias = "visionEnabled")]
  pub vision_enabled: Option<bool>,
  #[serde(default, alias = "summarizer_enabled", alias = "summarizerEnabled")]
  pub summarizer_enabled: Option<bool>,
  #[serde(default, alias = "custom_enabled_models", alias = "customEnabledModels")]
  pub custom_enabled_models: Option<Vec<String>>,
  #[serde(default, alias = "organizer_dispatch_note", alias = "organizerDispatchNote")]
  pub organizer_dispatch_note: Option<String>,
  #[serde(default, alias = "assistant_name", alias = "assistantName")]
  pub assistant_name: Option<String>,
  #[serde(default, alias = "model_profile", alias = "modelProfile")]
  pub model_profile: Option<String>,
  #[serde(default, alias = "network_enabled", alias = "networkEnabled")]
  pub network_enabled: Option<bool>,
  #[serde(default, alias = "repos_enabled", alias = "reposEnabled")]
  pub repos_enabled: Option<bool>,
  #[serde(default, alias = "mcp_enabled", alias = "mcpEnabled")]
  pub mcp_enabled: Option<bool>,
  #[serde(default, alias = "desktop_tools_enabled", alias = "desktopToolsEnabled")]
  pub desktop_tools_enabled: Option<bool>,
  #[serde(default, alias = "selected_project_name", alias = "selectedProjectName")]
  pub selected_project_name: Option<String>,
  #[serde(default, alias = "project_context", alias = "projectContext")]
  pub project_context: Option<String>,
  #[serde(default, alias = "project_dataweb", alias = "projectDataweb")]
  pub project_dataweb: Option<String>,
  #[serde(default, alias = "universal_dataweb", alias = "universalDataweb")]
  pub universal_dataweb: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct InterpretPlanV2 {
  pub primary_intent: String,
  pub secondary_intent: String,
  pub strategy: String,
  pub should_use_coder: bool,
  pub model: String,
  pub pressure_score: f32,
  pub lane_weights: LaneWeights,
  pub intent_scores: IntentScores,
  pub memory_lanes: MemoryLanes,
  pub compiled_context: CompiledContext,
  pub prompt: String,
  pub deterministic_reply: String,
  pub resolver_used: String,
  pub bridge_note: String,
  pub suggested_godot_version: String,
  pub routed_models: Vec<String>,
  pub route_summary: String,
  pub status_hint: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub routine_plan: Option<RoutinePlan>,
}


// ========== helpers ==========

fn memory_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    // Use Tauri 2.0's built-in path resolver
    let base = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("failed to resolve app data dir: {}", e))?;

    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    Ok(base)
}

fn tab_file(app: &tauri::AppHandle, tab_id: u32) -> Result<PathBuf, String> {
    let dir = iris_open_tabs_dir(app)?;
    Ok(dir.join(format!("tab_{}.json", tab_id)))
}

fn setup_flags_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  let dir = memory_dir(app)?;
  Ok(dir.join("setup_flags.json"))
}

fn read_setup_flags(app: &tauri::AppHandle) -> SetupFlags {
  let path = match setup_flags_file(app) {
    Ok(p) => p,
    Err(_) => return SetupFlags::default(),
  };
  if !path.exists() {
    return SetupFlags::default();
  }
  let raw = match fs::read_to_string(&path) {
    Ok(s) => s,
    Err(_) => return SetupFlags::default(),
  };
  serde_json::from_str::<SetupFlags>(&raw).unwrap_or_default()
}

fn write_setup_flags(app: &tauri::AppHandle, flags: &SetupFlags) -> Result<(), String> {
  let path = setup_flags_file(app)?;
  let json = serde_json::to_string_pretty(flags).map_err(|e| e.to_string())?;
  atomic_write_json(&path, &json)
}


fn atomic_write_text(path: &PathBuf, content: &str) -> Result<(), String> {
    let mut tmp = path.clone();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    tmp.set_extension(format!("tmp.{nanos}"));

    {
        let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().ok();
    }

    match fs::rename(&tmp, path) {
      Ok(_) => {}
      Err(rename_err) => {
        fs::copy(&tmp, path).map_err(|copy_err| {
          format!("rename failed: {}; copy fallback failed: {}", rename_err, copy_err)
        })?;
        let _ = fs::remove_file(&tmp);
      }
    }
    Ok(())
}


fn atomic_write_json(path: &PathBuf, json: &str) -> Result<(), String> {
    atomic_write_text(path, json)
}

fn now_ts() -> i64 {
  SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
  if input.chars().count() <= max_chars {
    return input.to_string();
  }
  input.chars().take(max_chars).collect()
}

fn bound_snapshot_payload(snapshot: &mut Snapshot) {
  const MAX_MESSAGES: usize = 140;
  const MAX_MESSAGE_CHARS: usize = 16_000;
  const MAX_ARTIFACTS: usize = 40;
  const MAX_ARTIFACT_CHARS: usize = 40_000;
  const MAX_PROMPT_HISTORY: usize = 60;
  const MAX_PROMPT_CHARS: usize = 800;

  if snapshot.messages.len() > MAX_MESSAGES {
    let keep_from = snapshot.messages.len().saturating_sub(MAX_MESSAGES);
    snapshot.messages = snapshot.messages.split_off(keep_from);
  }

  for msg in &mut snapshot.messages {
    if msg.text.chars().count() > MAX_MESSAGE_CHARS {
      msg.text = truncate_chars(&msg.text, MAX_MESSAGE_CHARS);
    }
  }

  if snapshot.artifacts.len() > MAX_ARTIFACTS {
    let keep_from = snapshot.artifacts.len().saturating_sub(MAX_ARTIFACTS);
    snapshot.artifacts = snapshot.artifacts.split_off(keep_from);
  }

  for artifact in &mut snapshot.artifacts {
    if artifact.content.chars().count() > MAX_ARTIFACT_CHARS {
      artifact.content = truncate_chars(&artifact.content, MAX_ARTIFACT_CHARS);
    }
  }

  if snapshot.prompt_history.len() > MAX_PROMPT_HISTORY {
    let keep_from = snapshot.prompt_history.len().saturating_sub(MAX_PROMPT_HISTORY);
    snapshot.prompt_history = snapshot.prompt_history.split_off(keep_from);
  }
  for prompt in &mut snapshot.prompt_history {
    if prompt.chars().count() > MAX_PROMPT_CHARS {
      *prompt = truncate_chars(prompt, MAX_PROMPT_CHARS);
    }
  }
}

fn compact_text(s: &str, max: usize) -> String {
  let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
  if collapsed.len() <= max {
    return collapsed;
  }
  format!("{}â€¦", &collapsed[..max.saturating_sub(1)])
}

fn contains_any(hay: &str, needles: &[&str]) -> bool {
  needles.iter().any(|n| hay.contains(n))
}

fn parse_lane_block(summary: &str, lane: &str) -> String {
  let key = format!("[{}]", lane);
  if let Some(start) = summary.find(&key) {
    let tail = &summary[start + key.len()..];
    let mut end = tail.len();
    for marker in ["[PROJECT]", "[CODING]", "[RECALL]"] {
      if let Some(i) = tail.find(marker) {
        if i < end { end = i; }
      }
    }
    return tail[..end].trim().to_string();
  }
  String::new()
}

fn build_memory_lanes(mem: &TabMemory, compiled: &CompiledContext) -> MemoryLanes {
  let prior_project = parse_lane_block(&mem.summary, "PROJECT");
  let prior_coding = parse_lane_block(&mem.summary, "CODING");
  let prior_recall = parse_lane_block(&mem.summary, "RECALL");

  let project = if !prior_project.is_empty() {
    prior_project
  } else if !mem.micro_summary.is_empty() {
    compact_text(&mem.micro_summary, 280)
  } else {
    "No active project facts captured yet.".to_string()
  };

  let coding = if !prior_coding.is_empty() {
    prior_coding
  } else if let Some(last_art) = mem.artifacts.iter().max_by_key(|a| a.ts) {
    let file = last_art.filename.clone().unwrap_or_else(|| "(unsaved snippet)".to_string());
    let lang = if last_art.lang.is_empty() { "".to_string() } else { format!(" [{}]", last_art.lang) };
    compact_text(&format!("Active artifact: {}{}. Preserve this for edit follow-ups.", file, lang), 280)
  } else {
    compact_text(&compiled.dialogue_bullets, 280)
  };

  let recall = if !prior_recall.is_empty() {
    prior_recall
  } else {
    compact_text(&compiled.recent_transcript, 280)
  };

  MemoryLanes { project, coding, recall }
}

fn extract_last_assistant_number(transcript: &str) -> Option<f64> {
  for line in transcript.lines().rev() {
    let line = line.trim();
    if !line.starts_with("Iris:") { continue; }
    let mut current = String::new();
    let mut vals: Vec<f64> = Vec::new();
    for ch in line.chars() {
      if ch.is_ascii_digit() || ch == '.' || ch == '-' {
        current.push(ch);
      } else if !current.is_empty() {
        if let Ok(v) = current.parse::<f64>() { vals.push(v); }
        current.clear();
      }
    }
    if !current.is_empty() {
      if let Ok(v) = current.parse::<f64>() { vals.push(v); }
    }
    if let Some(v) = vals.last() {
      return Some(*v);
    }
  }
  None
}

fn format_num(v: f64) -> String {
  let rounded = (v * 1_000_000.0).round() / 1_000_000.0;
  if (rounded.fract()).abs() < 1e-9 { format!("{}", rounded as i64) } else { format!("{}", rounded) }
}

fn try_resolve_math_reply(user_text: &str, transcript: &str) -> Option<String> {
  let text = user_text.to_lowercase();
  if !contains_any(&text, &["that", "it", "last number", "previous number", "number you just gave me", "you just gave me"]) {
    return None;
  }
  let base = extract_last_assistant_number(transcript)?;

  if text.contains("multiply") || text.contains("times") {
    if let Some(n) = first_number_in_text(&text) {
      return Some(format_num(base * n));
    }
  }
  if text.contains("divide") {
    if let Some(n) = first_number_in_text(&text) {
      if n == 0.0 { return Some("Cannot divide by zero.".to_string()); }
      return Some(format_num(base / n));
    }
  }
  if text.contains("add") || text.contains("plus") {
    if let Some(n) = first_number_in_text(&text) {
      return Some(format_num(base + n));
    }
  }
  if text.contains("subtract") || text.contains("minus") {
    if let Some(n) = first_number_in_text(&text) {
      return Some(format_num(base - n));
    }
  }
  if contains_any(&text, &["what was", "repeat", "say again"]) && contains_any(&text, &["last number", "previous number", "that number"]) {
    return Some(format_num(base));
  }
  None
}

fn first_number_in_text(text: &str) -> Option<f64> {
  let mut current = String::new();
  for ch in text.chars() {
    if ch.is_ascii_digit() || ch == '.' || ch == '-' {
      current.push(ch);
    } else if !current.is_empty() {
      if let Ok(v) = current.parse::<f64>() { return Some(v); }
      current.clear();
    }
  }
  if !current.is_empty() {
    if let Ok(v) = current.parse::<f64>() { return Some(v); }
  }
  None
}

fn is_recall_intent(user_text: &str) -> bool {
  let t = user_text.to_lowercase();
  contains_any(&t, &[
    "what was", "what is", "what joke", "first joke", "last joke", "just told", "earlier",
    "previous", "remember", "topic", "joke did you tell", "product", "result", "answer",
    "last number", "previous number"
  ])
}

fn is_numeric_recall_query(user_text: &str) -> bool {
  let text = user_text.to_lowercase();
  let asks_for_prior_value = contains_any(&text, &[
    "what was", "what is", "repeat", "say again", "remember", "earlier", "previous", "last"
  ]);
  let asks_for_numeric_result = contains_any(&text, &[
    "product", "result", "answer", "total", "number", "value"
  ]);
  asks_for_prior_value && asks_for_numeric_result
}

fn try_resolve_numeric_recall_reply(user_text: &str, transcript: &str) -> Option<String> {
  if !is_numeric_recall_query(user_text) {
    return None;
  }
  extract_last_assistant_number(transcript).map(format_num)
}

fn extract_joke_replies(messages: &[ChatMessage]) -> Vec<String> {
  let mut out = Vec::new();
  for i in 1..messages.len() {
    let prev = &messages[i - 1];
    let cur = &messages[i];
    if prev.role != "user" || cur.role != "llm" { continue; }
    let p = prev.text.to_lowercase();
    if contains_any(&p, &["joke", "funny", "humor", "humour", "another joke", "make me laugh"]) {
      let reply = cur.text.trim();
      if !reply.is_empty() { out.push(reply.to_string()); }
    }
  }
  out
}

fn infer_joke_topic(joke: &str) -> String {
  let lower = joke.to_lowercase();
  if let Some(idx) = lower.find("why ") {
    let slice = &lower[idx + 4..];
    let tokens: Vec<&str> = slice.split_whitespace().collect();
    if let Some(last) = tokens.last() {
      let clean = last.chars().filter(|c| c.is_alphanumeric()).collect::<String>();
      if !clean.is_empty() { return clean; }
    }
  }
  "the previous joke".to_string()
}

fn try_resolve_recall_reply(user_text: &str, messages: &[ChatMessage], transcript: &str) -> Option<String> {
  if !is_recall_intent(user_text) { return None; }
  let text = user_text.to_lowercase();

  if let Some(number) = try_resolve_numeric_recall_reply(user_text, transcript) {
    return Some(number);
  }

  let assistant_messages: Vec<String> = messages.iter()
    .filter(|m| m.role == "llm")
    .map(|m| m.text.trim().to_string())
    .filter(|t| !t.is_empty())
    .collect();

  let assistant_messages = if assistant_messages.is_empty() {
    transcript.lines()
      .filter(|l| l.trim().starts_with("Iris:"))
      .map(|l| l.trim().trim_start_matches("Iris:").trim().to_string())
      .collect::<Vec<_>>()
  } else {
    assistant_messages
  };

  if assistant_messages.is_empty() { return None; }

  let first_assistant = assistant_messages.first().cloned().unwrap_or_default();
  let last_assistant = assistant_messages.last().cloned().unwrap_or_default();
  let jokes = extract_joke_replies(messages);
  let first_joke = jokes.first().cloned();
  let last_joke = jokes.last().cloned();

  if contains_any(&text, &["first joke", "joke did you tell me first", "what was the first joke you told me"]) {
    return Some(first_joke.unwrap_or_else(|| "I do not see a prior joke in our conversation memory.".to_string()));
  }
  if contains_any(&text, &["what joke did you just tell", "joke you just told", "last joke", "what was the joke you just told me"]) {
    let joke = match last_joke {
      Some(v) => v,
      None => return Some("I do not see a recent joke in our conversation memory.".to_string()),
    };
    if contains_any(&text, &["topic", "about"]) {
      return Some(format!("The topic is {}.", infer_joke_topic(&joke)));
    }
    return Some(joke);
  }
  if contains_any(&text, &["what did you just tell me", "what was your last response", "what did you just say", "what did you just write", "what did you write for me", "repeat what you wrote", "what did you just write for me"]) {
    return Some(last_assistant);
  }
  if contains_any(&text, &["first thing you told me", "first response"]) {
    return Some(first_assistant);
  }

  None
}

fn score_intents(user_text: &str, mem: &TabMemory, lanes: &MemoryLanes) -> IntentScores {
  let t = user_text.to_lowercase();
  let dev_kw = contains_any(&t, &["project", "plan", "roadmap", "engine", "tool", "ide", "debug", "build", "workflow"]) as i32 as f32;
  let code_kw = contains_any(&t, &["code", "script", "function", "class", "gdscript", "typescript", "python", "c#", "refactor"]) as i32 as f32;
  let edit_kw = contains_any(&t, &["modify", "change", "edit", "update", "refactor", "add one more", "now modify"]) as i32 as f32;
  let pronoun_ref = contains_any(&t, &["that", "it", "last number", "previous", "you just gave me"]) as i32 as f32;
  let math_kw = contains_any(&t, &["multiply", "divide", "plus", "minus", "w", "wh", "kwh", "solar", "watt", "hours"]) as i32 as f32;
  let gk_kw = contains_any(&t, &["tell me about", "what is", "explain", "sun", "history", "science", "planet"]) as i32 as f32;
  let banter_kw = contains_any(&t, &["roleplay", "how was your day", "joke", "pretend", "banter", "chat"]) as i32 as f32;
  let meta_kw = contains_any(&t, &["what is your name", "purpose", "who are you", "designed for", "best at"]) as i32 as f32;
  let repair_kw = contains_any(&t, &["huh", "try again", "i mean", "that's wrong", "rephrase"]) as i32 as f32;
  let roleplay_kw = contains_any(&t, &["roleplay", "pretend", "act as", "make something up"]) as i32 as f32;
  let project_momentum = ((!lanes.project.is_empty() && lanes.project != "No active project facts captured yet.") as i32) as f32;
  let artifact_presence = (!mem.artifacts.is_empty() as i32) as f32;
  let has_numeric_ref = (extract_last_assistant_number(&mem.messages.iter().rev().take(20)
    .map(|m| format!("{}: {}", if m.role == "user" { "User" } else { "Iris" }, m.text))
    .collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")).is_some()) as i32 as f32;

  IntentScores {
    dev_task: 0.35 * dev_kw + 0.20 * code_kw + 0.20 * project_momentum + 0.25 * artifact_presence,
    code_edit_followup: 0.45 * edit_kw + 0.30 * pronoun_ref + 0.25 * artifact_presence,
    math_units: 0.50 * math_kw + 0.35 * pronoun_ref + 0.15 * has_numeric_ref,
    reference_recall: 0.40 * pronoun_ref + 0.30 * contains_any(&t, &["remember", "what was", "just told", "first", "last", "that age"]) as i32 as f32 + 0.15 * repair_kw + 0.15 * has_numeric_ref,
    general_knowledge: 0.70 * gk_kw + 0.20 * (1.0 - project_momentum) + 0.10 * (1.0 - code_kw),
    banter_roleplay: 0.55 * banter_kw + 0.30 * contains_any(&t, &["how was your day", "pretend", "act", "chat with me"]) as i32 as f32 + 0.15 * roleplay_kw,
    meta_identity: 0.85 * meta_kw,
    clarification_repair: 0.80 * repair_kw,
  }
}

fn top_two_intents(scores: &IntentScores) -> (String, String) {
  let mut pairs = vec![
    ("dev_task", scores.dev_task),
    ("code_edit_followup", scores.code_edit_followup),
    ("math_units", scores.math_units),
    ("reference_recall", scores.reference_recall),
    ("general_knowledge", scores.general_knowledge),
    ("banter_roleplay", scores.banter_roleplay),
    ("meta_identity", scores.meta_identity),
    ("clarification_repair", scores.clarification_repair),
  ];
  pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
  (
    pairs.get(0).map(|p| p.0.to_string()).unwrap_or_else(|| "dev_task".to_string()),
    pairs.get(1).map(|p| p.0.to_string()).unwrap_or_else(|| "general_knowledge".to_string()),
  )
}

fn compute_lane_weights(primary: &str) -> LaneWeights {
  match primary {
    "general_knowledge" => LaneWeights { project: 0.45, coding: 0.20, recall: 0.35 },
    "banter_roleplay" => LaneWeights { project: 0.35, coding: 0.20, recall: 0.45 },
    "math_units" => LaneWeights { project: 0.35, coding: 0.20, recall: 0.45 },
    "code_edit_followup" => LaneWeights { project: 0.25, coding: 0.60, recall: 0.15 },
    _ => LaneWeights { project: 0.45, coding: 0.35, recall: 0.20 },
  }
}

fn banter_streak(mem: &TabMemory) -> usize {
  let mut count = 0usize;
  for m in mem.messages.iter().rev() {
    if m.role != "user" { continue; }
    let t = m.text.to_lowercase();
    if contains_any(&t, &["joke", "roleplay", "pretend", "how was your day", "chat"]) {
      count += 1;
    } else {
      break;
    }
  }
  count
}

fn unresolved_count(mem: &TabMemory) -> usize {
  mem.messages.iter().rev().take(8)
    .filter(|m| m.role == "user" && m.text.contains("?"))
    .count()
}

fn pressure_score(mem: &TabMemory, lanes: &MemoryLanes) -> f32 {
  let momentum = if !lanes.project.is_empty() && lanes.project != "No active project facts captured yet." { 0.9 } else { 0.35 };
  let open_work = (unresolved_count(mem) as f32 / 4.0).clamp(0.0, 1.0);
  let drift = (banter_streak(mem) as f32 / 4.0).clamp(0.0, 1.0);
  0.45 * momentum + 0.35 * open_work + 0.20 * drift
}

fn has_concrete_project_context(mem: &TabMemory, lanes: &MemoryLanes) -> bool {
  if !mem.artifacts.is_empty() {
    return true;
  }
  let p = lanes.project.trim();
  !p.is_empty() && p != "No active project facts captured yet."
}

fn turns_since_last_project_bridge(mem: &TabMemory) -> usize {
  let mut user_turns = 0usize;
  for m in mem.messages.iter().rev() {
    if m.role == "llm" {
      let t = m.text.to_lowercase();
      if contains_any(&t, &["back to our project", "back to the project", "coding front", "next step", "get back to"]) {
        return user_turns;
      }
      continue;
    }
    if m.role == "user" {
      user_turns += 1;
    }
  }
  usize::MAX
}

fn bridge_interval(mem: &TabMemory) -> usize {
  // Semi-randomized cadence per tab/session shape (2-4 user turns).
  2 + ((mem.tab_id as usize + mem.messages.len()) % 3)
}

fn explicitly_requests_long_term_memory(user_text: &str) -> bool {
  let t = user_text.to_lowercase();
  contains_any(&t, &[
    "earlier in our chats",
    "past session",
    "last session",
    "our notes",
    "project notes",
    "what did we decide",
    "where did we leave off",
  ])
}

fn transcript_insufficient_for_long_term(transcript: &str, user_text: &str) -> bool {
  let chars = transcript.trim().chars().count();
  if chars < 180 {
    return true;
  }
  let t = user_text.to_lowercase();
  let recallish = contains_any(&t, &[
    "status", "todo", "next step", "summary", "continue", "as discussed", "from earlier",
  ]);
  recallish && chars < 520
}

fn should_emit_bridge(primary_intent: &str, pressure: f32, mem: &TabMemory) -> bool {
  if primary_intent != "banter_roleplay" || pressure <= 0.52 {
    return false;
  }
  let streak = banter_streak(mem);
  if streak < 2 {
    return false;
  }
  let since_last = turns_since_last_project_bridge(mem);
  since_last >= bridge_interval(mem)
}

fn is_coder_intent_text(text: &str) -> bool {
  let t = text.to_lowercase();
  contains_any(&t, &[
    "function", "class", "def", "const", "let", "var", "import", "export", "traceback", "typeerror",
    "gdscript", "godot", "typescript", "python", "c#", "regex", "sql", ".ts", ".tsx", ".py", ".gd", ".cs", ".rs"
  ])
}

fn is_edit_followup_text(text: &str) -> bool {
  let t = text.to_lowercase();
  contains_any(&t, &["modify", "change", "update", "edit", "refactor", "adjust", "tweak", "replace", "now modify", "change the script"])
}

fn resolve_primary_strategy(primary_intent: &str, deterministic_reply: &str) -> String {
  if !deterministic_reply.is_empty() {
    if primary_intent == "math_units" { return "deterministic_math".to_string(); }
    return "deterministic_recall".to_string();
  }
  match primary_intent {
    "clarification_repair" => "clarify_once".to_string(),
    "banter_roleplay" => "banter_then_bridge".to_string(),
    "meta_identity" => "identity_concise".to_string(),
    "general_knowledge" => "knowledge_answer".to_string(),
    _ => "direct_task".to_string(),
  }
}

fn detect_godot_version_hint(user_text: &str, transcript: &str) -> String {
  let combined = format!("{}\n{}", user_text.to_lowercase(), transcript.to_lowercase());

  let godot4_hits = [
    "node3d", "characterbody3d", "characterbody2d", "@export", "@onready",
    "await ", "scene tree", "godot 4", "tilemaplayer", "navigationagent3d"
  ]
  .iter()
  .filter(|kw| combined.contains(**kw))
  .count();

  let godot3_hits = [
    "spatial", "kinematicbody", "kinematicbody2d", "poolstringarray", "yield(",
    "export var", "onready var", "godot 3", "tilemap", "navigation"
  ]
  .iter()
  .filter(|kw| combined.contains(**kw))
  .count();

  if godot4_hits > godot3_hits && godot4_hits > 0 {
    "godot4".to_string()
  } else if godot3_hits > godot4_hits && godot3_hits > 0 {
    "godot3".to_string()
  } else {
    "unspecified".to_string()
  }
}

fn load_persona_prompt() -> String {
  let core = load_core_persona_prompt().unwrap_or_else(|| {
    "You are Iris, an open-source local AI assistant for real project work. Interpret first, answer clearly, preserve project continuity, and help across implementation, planning, research, debugging, and general assistance.".to_string()
  });
  let user = load_user_persona_prompt().unwrap_or_default();
  if user.trim().is_empty() {
    return core;
  }
  format!("{}\n\nUser Persona Overlay:\n{}", core, user)
}

fn locate_core_prompts_dir(app: Option<&tauri::AppHandle>) -> Result<PathBuf, String> {
  let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
  let mut candidates: Vec<PathBuf> = vec![
    cwd.join("core_prompts"),
    cwd.join("..\\core_prompts"),
    cwd.join("..\\..\\core_prompts"),
  ];

  if let Ok(exe) = std::env::current_exe() {
    if let Some(exe_dir) = exe.parent() {
      candidates.push(exe_dir.join("core_prompts"));
      candidates.push(exe_dir.join("..\\..\\..\\core_prompts"));
    }
  }

  if let Some(a) = app {
    if let Ok(resource_dir) = a.path().resource_dir() {
      candidates.push(resource_dir.join("core_prompts"));
    }
  }

  candidates
    .into_iter()
    .find(|p| p.exists() && p.is_dir())
    .ok_or_else(|| "Could not locate core_prompts directory".to_string())
}

fn user_persona_path(app: Option<&tauri::AppHandle>) -> Result<PathBuf, String> {
  Ok(locate_core_prompts_dir(app)?.join("UserPersonaPrompt.txt"))
}

fn ensure_user_persona_prompt_file(app: Option<&tauri::AppHandle>) -> Result<PathBuf, String> {
  let p = user_persona_path(app)?;
  if !p.exists() {
    fs::write(
      &p,
      "# User Persona Prompt\n# Add your personal style preferences and optional behavior constraints here.\n# Keep this concise.\n"
    ).map_err(|e| format!("Failed to initialize {}: {}", p.display(), e))?;
  }
  Ok(p)
}

fn load_core_persona_prompt() -> Option<String> {
  let s = include_str!("core_persona_embedded.txt");
  let t = s.trim();
  if t.is_empty() { None } else { Some(t.to_string()) }
}

fn secure_core_persona_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  let mut dir = memory_dir(app)?;
  dir.push("secure");
  create_dir_all(&dir).map_err(|e| e.to_string())?;
  Ok(dir.join("core_persona.txt"))
}

fn ensure_secure_core_persona_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  let path = secure_core_persona_path(app)?;
  if !path.exists() {
    let text = load_core_persona_prompt().unwrap_or_default();
    fs::write(&path, text).map_err(|e| format!("Failed to initialize {}: {}", path.display(), e))?;
  }
  // Best-effort readonly attribute; source of truth still remains backend-controlled.
  if let Ok(meta) = fs::metadata(&path) {
    let mut perms = meta.permissions();
    if !perms.readonly() {
      perms.set_readonly(true);
      let _ = fs::set_permissions(&path, perms);
    }
  }
  Ok(path)
}

fn load_user_persona_prompt() -> Option<String> {
  let p = ensure_user_persona_prompt_file(None).ok()?;
  let s = fs::read_to_string(p).ok()?;
  let t = s.trim();
  if t.is_empty() { None } else { Some(t.to_string()) }
}

#[tauri::command]
pub fn get_core_persona_prompt(app: tauri::AppHandle) -> Result<String, String> {
  let path = ensure_secure_core_persona_file(&app)?;
  fs::read_to_string(&path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))
}

#[tauri::command]
pub fn get_user_persona_prompt(app: tauri::AppHandle) -> Result<String, String> {
  let path = ensure_user_persona_prompt_file(Some(&app))?;
  fs::read_to_string(&path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))
}

#[tauri::command]
pub fn save_user_persona_prompt(app: tauri::AppHandle, content: String) -> Result<(), String> {
  let path = ensure_user_persona_prompt_file(Some(&app))?;
  fs::write(&path, content).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

fn resolve_git_executable() -> Option<PathBuf> {
  if let Ok(out) = Command::new("git").arg("--version").output() {
    if out.status.success() {
      return Some(PathBuf::from("git"));
    }
  }

  let mut candidates: Vec<PathBuf> = Vec::new();
  if let Ok(pf) = std::env::var("ProgramFiles") {
    candidates.push(PathBuf::from(&pf).join("Git").join("cmd").join("git.exe"));
    candidates.push(PathBuf::from(&pf).join("Git").join("bin").join("git.exe"));
  }
  if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
    candidates.push(PathBuf::from(&pf86).join("Git").join("cmd").join("git.exe"));
    candidates.push(PathBuf::from(&pf86).join("Git").join("bin").join("git.exe"));
  }
  if let Ok(local) = std::env::var("LOCALAPPDATA") {
    candidates.push(PathBuf::from(&local).join("Programs").join("Git").join("cmd").join("git.exe"));
    candidates.push(PathBuf::from(&local).join("Programs").join("Git").join("bin").join("git.exe"));
  }

  candidates.into_iter().find(|p| p.exists() && p.is_file())
}

#[tauri::command]
pub fn open_user_persona_prompt(app: tauri::AppHandle) -> Result<(), String> {
  let file = ensure_user_persona_prompt_file(Some(&app))?;

  if cfg!(target_os = "windows") {
    Command::new("explorer")
      .arg(format!("/select,{}", file.display()))
      .spawn()
      .map_err(|e| e.to_string())?;
  } else if cfg!(target_os = "macos") {
    Command::new("open")
      .arg("-R")
      .arg(&file)
      .spawn()
      .map_err(|e| e.to_string())?;
  } else {
    let folder = file.parent().ok_or_else(|| "missing parent folder".to_string())?;
    Command::new("xdg-open")
      .arg(folder)
      .spawn()
      .map_err(|e| e.to_string())?;
  }

  Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RepoEntry {
  pub id: String,
  pub name: String,
  pub path: String,
  pub is_dir: bool,
  pub size_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RepoFolder {
  pub id: String,
  pub name: String,
  pub path: String,
  pub enabled: bool,
  pub entries: Vec<RepoEntry>,
  pub selected_entry_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpConnection {
  pub id: String,
  pub name: String,
  pub target: String,
  pub enabled: bool,
  #[serde(default)]
  pub connection_type: Option<String>,
  #[serde(default)]
  pub launch_command: Option<String>,
  #[serde(default)]
  pub launch_args: Option<Vec<String>>,
  #[serde(default)]
  pub notes: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SshConnection {
  pub id: String,
  pub name: String,
  pub host: String,
  #[serde(default = "default_ssh_port")]
  pub port: u16,
  #[serde(default)]
  pub username: String,
  #[serde(default)]
  pub private_key_path: String,
  #[serde(default)]
  pub known_hosts_path: String,
  #[serde(default)]
  pub remote_root: String,
  #[serde(default = "default_true_flag")]
  pub strict_host_key_checking: bool,
  #[serde(default)]
  pub extra_args: Vec<String>,
  #[serde(default)]
  pub enabled: bool,
  #[serde(default)]
  pub notes: String,
}

fn default_ssh_port() -> u16 { 22 }

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDef {
  pub id: String,
  pub name: String,
  pub enabled: bool,
  pub description: String,
  #[serde(default)]
  pub manipulation_root_path: String,
  #[serde(default = "default_true_flag")]
  pub dataweb_enabled: bool,
  pub repo_ids: Vec<String>,
  pub entry_ids: Vec<String>,
  #[serde(default)]
  pub mcp_ids: Vec<String>,
  #[serde(default)]
  pub ssh_ids: Vec<String>,
}

fn default_true_flag() -> bool { true }

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RepoProjectStore {
  pub repos: Vec<RepoFolder>,
  #[serde(default)]
  pub mcps: Vec<McpConnection>,
  #[serde(default)]
  pub sshs: Vec<SshConnection>,
  pub projects: Vec<ProjectDef>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCheckpointMeta {
  pub id: String,
  pub label: String,
  pub project_id: String,
  pub root_path: String,
  pub created_at: i64,
  pub file_count: usize,
  pub total_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCheckpointFile {
  pub path: String,
  pub size_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCheckpointManifest {
  pub meta: ProjectCheckpointMeta,
  pub files: Vec<ProjectCheckpointFile>,
}

fn checkpoint_store_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  let base = app
    .path()
    .app_data_dir()
    .map_err(|e| e.to_string())?
    .join("project_checkpoints");
  create_dir_all(&base).map_err(|e| e.to_string())?;
  Ok(base)
}

fn sanitize_checkpoint_component(value: &str) -> String {
  let mut out = String::with_capacity(value.len());
  for ch in value.chars() {
    if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
      out.push(ch);
    } else {
      out.push('_');
    }
  }
  let trimmed = out.trim_matches('_');
  if trimmed.is_empty() {
    "project".to_string()
  } else {
    trimmed.to_string()
  }
}

fn checkpoint_bucket_id(project_id: &str, root: &str) -> String {
  let pid = project_id.trim();
  if !pid.is_empty() {
    return sanitize_checkpoint_component(pid);
  }
  let mut hasher = DefaultHasher::new();
  root.hash(&mut hasher);
  format!("root_{:x}", hasher.finish())
}

fn should_skip_checkpoint_dir(name: &str) -> bool {
  matches!(
    name,
    ".git" | "node_modules" | "target" | "dist" | "build" | ".idea" | ".vs" | ".vscode" | ".venv" | "venv"
  )
}

fn collect_checkpoint_files(root: &PathBuf, max_files: usize) -> Vec<PathBuf> {
  let mut out: Vec<PathBuf> = Vec::new();
  let mut queue: VecDeque<PathBuf> = VecDeque::new();
  queue.push_back(root.clone());

  while let Some(dir) = queue.pop_front() {
    if out.len() >= max_files {
      break;
    }
    let rd = match read_dir(&dir) {
      Ok(v) => v,
      Err(_) => continue,
    };
    for entry in rd.flatten() {
      if out.len() >= max_files {
        break;
      }
      let p = entry.path();
      let meta = match entry.metadata() {
        Ok(m) => m,
        Err(_) => continue,
      };
      if meta.is_dir() {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if should_skip_checkpoint_dir(name) {
          continue;
        }
        queue.push_back(p);
        continue;
      }
      if !meta.is_file() {
        continue;
      }
      if !is_text_like_file(&p) {
        continue;
      }
      if meta.len() > 1_000_000 {
        continue;
      }
      out.push(p);
    }
  }

  out
}

fn checkpoint_manifest_path(checkpoint_dir: &PathBuf) -> PathBuf {
  checkpoint_dir.join("manifest.json")
}

fn read_checkpoint_manifest(path: &PathBuf) -> Result<ProjectCheckpointManifest, String> {
  let raw = fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
  serde_json::from_str::<ProjectCheckpointManifest>(&raw)
    .map_err(|e| format!("Failed to parse checkpoint manifest {}: {}", path.display(), e))
}

#[tauri::command]
pub fn capture_project_checkpoint(
  app: tauri::AppHandle,
  project_id: String,
  root: String,
  label: Option<String>,
) -> Result<ProjectCheckpointMeta, String> {
  let root_path = PathBuf::from(root.trim());
  if !root_path.exists() || !root_path.is_dir() {
    return Err("Project manipulation root is invalid".to_string());
  }
  let root_canon = fs::canonicalize(&root_path).map_err(|e| format!("Invalid project root: {}", e))?;

  let bucket = checkpoint_bucket_id(&project_id, &root_canon.to_string_lossy());
  let bucket_dir = checkpoint_store_root(&app)?.join(bucket);
  create_dir_all(&bucket_dir).map_err(|e| e.to_string())?;

  let checkpoint_id = format!("cp_{}", now_ts());
  let checkpoint_dir = bucket_dir.join(&checkpoint_id);
  let files_dir = checkpoint_dir.join("files");
  create_dir_all(&files_dir).map_err(|e| e.to_string())?;

  let mut file_records: Vec<ProjectCheckpointFile> = Vec::new();
  let mut total_bytes: u64 = 0;
  for src in collect_checkpoint_files(&root_canon, 4000) {
    let rel = match src.strip_prefix(&root_canon) {
      Ok(v) => v,
      Err(_) => continue,
    };
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    if rel_str.is_empty() {
      continue;
    }
    let dst = files_dir.join(rel);
    if let Some(parent) = dst.parent() {
      create_dir_all(parent).map_err(|e| format!("Failed creating checkpoint directory: {}", e))?;
    }
    fs::copy(&src, &dst).map_err(|e| format!("Failed checkpointing {}: {}", src.display(), e))?;
    let size = src.metadata().map(|m| m.len()).unwrap_or(0);
    total_bytes = total_bytes.saturating_add(size);
    file_records.push(ProjectCheckpointFile { path: rel_str, size_bytes: size });
  }

  let meta = ProjectCheckpointMeta {
    id: checkpoint_id.clone(),
    label: label.unwrap_or_default().trim().to_string(),
    project_id: project_id.trim().to_string(),
    root_path: root_canon.to_string_lossy().to_string(),
    created_at: now_ts(),
    file_count: file_records.len(),
    total_bytes,
  };
  let manifest = ProjectCheckpointManifest {
    meta: meta.clone(),
    files: file_records,
  };
  let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
  atomic_write_json(&checkpoint_manifest_path(&checkpoint_dir), &manifest_json)?;

  let mut checkpoints: Vec<(i64, PathBuf)> = Vec::new();
  if let Ok(rd) = read_dir(&bucket_dir) {
    for entry in rd.flatten() {
      let dir = entry.path();
      if !dir.is_dir() {
        continue;
      }
      let manifest_path = checkpoint_manifest_path(&dir);
      if !manifest_path.exists() {
        continue;
      }
      if let Ok(parsed) = read_checkpoint_manifest(&manifest_path) {
        checkpoints.push((parsed.meta.created_at, dir));
      }
    }
  }
  checkpoints.sort_by(|a, b| b.0.cmp(&a.0));
  for (_, stale_dir) in checkpoints.into_iter().skip(8) {
    let _ = fs::remove_dir_all(stale_dir);
  }

  Ok(meta)
}

#[tauri::command]
pub fn list_project_checkpoints(
  app: tauri::AppHandle,
  project_id: String,
  root: String,
) -> Result<Vec<ProjectCheckpointMeta>, String> {
  let bucket = checkpoint_bucket_id(&project_id, root.trim());
  let bucket_dir = checkpoint_store_root(&app)?.join(bucket);
  if !bucket_dir.exists() || !bucket_dir.is_dir() {
    return Ok(Vec::new());
  }

  let mut out: Vec<ProjectCheckpointMeta> = Vec::new();
  if let Ok(rd) = read_dir(&bucket_dir) {
    for entry in rd.flatten() {
      let dir = entry.path();
      if !dir.is_dir() {
        continue;
      }
      let manifest_path = checkpoint_manifest_path(&dir);
      if !manifest_path.exists() {
        continue;
      }
      if let Ok(parsed) = read_checkpoint_manifest(&manifest_path) {
        out.push(parsed.meta);
      }
    }
  }
  out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
  Ok(out)
}

#[tauri::command]
pub fn restore_project_checkpoint(
  app: tauri::AppHandle,
  project_id: String,
  root: String,
  checkpoint_id: String,
  clean: Option<bool>,
) -> Result<String, String> {
  let root_path = PathBuf::from(root.trim());
  if !root_path.exists() || !root_path.is_dir() {
    return Err("Project manipulation root is invalid".to_string());
  }
  let root_canon = fs::canonicalize(&root_path).map_err(|e| format!("Invalid project root: {}", e))?;
  let bucket = checkpoint_bucket_id(&project_id, &root_canon.to_string_lossy());
  let checkpoint_dir = checkpoint_store_root(&app)?.join(bucket).join(checkpoint_id.trim());
  let manifest_path = checkpoint_manifest_path(&checkpoint_dir);
  if !manifest_path.exists() {
    return Err("Checkpoint not found".to_string());
  }
  let manifest = read_checkpoint_manifest(&manifest_path)?;
  let files_dir = checkpoint_dir.join("files");

  let mut restored = 0usize;
  let mut allowed: HashSet<String> = HashSet::new();
  for item in &manifest.files {
    let rel = item.path.replace('\\', "/").trim_matches('/').to_string();
    if rel.is_empty() {
      continue;
    }
    allowed.insert(rel.clone());
    let src = files_dir.join(&rel);
    if !src.exists() || !src.is_file() {
      continue;
    }
    let dst = root_canon.join(&rel);
    if let Some(parent) = dst.parent() {
      create_dir_all(parent).map_err(|e| format!("Failed creating restore directory: {}", e))?;
    }
    fs::copy(&src, &dst).map_err(|e| format!("Failed restoring {}: {}", rel, e))?;
    restored += 1;
  }

  let mut removed = 0usize;
  if clean.unwrap_or(false) {
    for existing in collect_checkpoint_files(&root_canon, 10000) {
      let rel = match existing.strip_prefix(&root_canon) {
        Ok(v) => v.to_string_lossy().replace('\\', "/"),
        Err(_) => continue,
      };
      if rel.is_empty() || allowed.contains(&rel) {
        continue;
      }
      if fs::remove_file(&existing).is_ok() {
        removed += 1;
      }
    }
  }

  Ok(format!(
    "Restored checkpoint {} to {} ({} files restored{}).",
    manifest.meta.id,
    root_canon.display(),
    restored,
    if clean.unwrap_or(false) { format!(", {} extra text files removed", removed) } else { String::new() }
  ))
}

fn shell_single_quote(s: &str) -> String {
  let escaped = s.replace('"', "\\\"").replace('\\', "\\\\");
  format!("\"{}\"", escaped)
}

fn ssh_destination(conn: &SshConnection) -> Result<String, String> {
  let host = conn.host.trim();
  if host.is_empty() {
    return Err("SSH host is required".to_string());
  }
  let user = conn.username.trim();
  if user.is_empty() {
    Ok(host.to_string())
  } else {
    Ok(format!("{}@{}", user, host))
  }
}

fn ssh_base_args(conn: &SshConnection) -> Vec<String> {
  let mut args: Vec<String> = Vec::new();
  if conn.port != 0 && conn.port != 22 {
    args.push("-p".to_string());
    args.push(conn.port.to_string());
  }
  if !conn.private_key_path.trim().is_empty() {
    args.push("-i".to_string());
    args.push(conn.private_key_path.trim().to_string());
  }
  args.push("-o".to_string());
  args.push(format!("StrictHostKeyChecking={}", if conn.strict_host_key_checking { "yes" } else { "no" }));
  if !conn.known_hosts_path.trim().is_empty() {
    args.push("-o".to_string());
    args.push(format!("UserKnownHostsFile={}", conn.known_hosts_path.trim()));
  }
  for a in &conn.extra_args {
    let trimmed = a.trim();
    if !trimmed.is_empty() {
      args.push(trimmed.to_string());
    }
  }
  args
}

fn ssh_run_script_over_ssh(
  conn: &SshConnection,
  remote_program: &str,
  remote_args: &[String],
  script: &str,
) -> Result<(String, String), String> {
  let ssh_bin = resolve_executable("ssh").unwrap_or_else(|| "ssh".to_string());
  let destination = ssh_destination(conn)?;
  let mut cmd = Command::new(ssh_bin);
  for a in ssh_base_args(conn) {
    cmd.arg(a);
  }
  cmd.arg(destination).arg(remote_program);
  for a in remote_args {
    cmd.arg(a);
  }
  cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

  let mut child = cmd.spawn().map_err(|e| format!("Failed to launch ssh: {}", e))?;
  if let Some(mut stdin) = child.stdin.take() {
    stdin
      .write_all(script.as_bytes())
      .map_err(|e| format!("Failed writing remote script: {}", e))?;
  }

  let out = child.wait_with_output().map_err(|e| format!("SSH execution failed: {}", e))?;
  let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
  let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
  if !out.status.success() {
    let detail = if !stderr.is_empty() { stderr.clone() } else { stdout.clone() };
    return Err(if detail.is_empty() {
      format!("Remote {} execution failed", remote_program)
    } else {
      detail
    });
  }
  Ok((stdout, stderr))
}

fn parse_json_from_stdout(stdout: &str) -> Result<Value, String> {
  if let Ok(v) = serde_json::from_str::<Value>(stdout) {
    return Ok(v);
  }
  for line in stdout.lines().rev() {
    let trimmed = line.trim();
    if trimmed.is_empty() {
      continue;
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
      return Ok(v);
    }
  }
  Err("Invalid SSH response JSON".to_string())
}

fn ssh_remote_python_with(conn: &SshConnection, payload: Value, interpreter: &str) -> Result<Value, String> {
  let payload_json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
  let script = format!(r#"import json, os, sys, pathlib, shutil, subprocess
payload = json.loads({payload})
action = str(payload.get("action", "")).strip().lower()
root = str(payload.get("remote_root", "") or "").strip().replace('\\\\','/')
args = payload.get("arguments", {{}}) or {{}}

def fail(msg):
    print(json.dumps({{"ok": False, "error": msg}}))
    raise SystemExit(1)

def norm_rel(raw):
    s = str(raw or ".").replace('\\\\', '/').strip()
    if s == "":
        s = "."
    parts = [p for p in s.split('/') if p not in ('', '.')]
    if any(p == '..' for p in parts):
        fail("Path escapes remote root")
    return '/'.join(parts) if parts else "."

def abspath(rel):
    n = norm_rel(rel)
    if not root:
        return n
    base = root.rstrip('/')
    if n == '.':
        return base
    return f"{{base}}/{{n}}"

def ensure_parent(path):
    p = pathlib.Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)

if action == "list":
    target = abspath(args.get("path", "."))
    p = pathlib.Path(target)
    if not p.exists() or not p.is_dir():
        fail("Directory does not exist")
    out = []
    for idx, child in enumerate(sorted(p.iterdir(), key=lambda x: (not x.is_dir(), x.name.lower()))):
        name = child.name
        if name.startswith('.'):
            continue
        is_dir = child.is_dir()
        size = 0
        try:
            size = int(child.stat().st_size) if not is_dir else 0
        except Exception:
            size = 0
        rel = name if args.get("path") in (None, "", ".") else f"{{norm_rel(args.get('path'))}}/{{name}}"
        out.append({{"id": f"ssh_entry_{{idx+1}}", "name": name, "path": rel, "isDir": is_dir, "sizeBytes": size}})
    print(json.dumps({{"ok": True, "entries": out}}))
elif action == "read":
    target = abspath(args.get("path", ""))
    p = pathlib.Path(target)
    if not p.exists() or not p.is_file():
        fail("File does not exist")
    max_chars = int(args.get("maxChars", args.get("max_chars", 20000)) or 20000)
    max_chars = min(max(max_chars, 256), 300000)
    data = p.read_text(encoding='utf-8', errors='ignore')
    print(json.dumps({{"ok": True, "content": data[:max_chars]}}))
elif action == "write":
    target = abspath(args.get("path", ""))
    content = str(args.get("content", ""))
    overwrite = bool(args.get("overwrite", True))
    create_dirs = bool(args.get("createDirs", args.get("create_dirs", True)))
    p = pathlib.Path(target)
    if p.exists() and p.is_dir():
        fail("Target path is a directory")
    if p.exists() and not overwrite:
        fail("Target file exists and overwrite=false")
    if create_dirs:
        ensure_parent(p)
    p.write_text(content, encoding='utf-8')
    print(json.dumps({{"ok": True, "message": f"Wrote {{str(p)}}"}}))
elif action == "delete":
    target = abspath(args.get("path", ""))
    recursive = bool(args.get("recursive", False))
    p = pathlib.Path(target)
    if not p.exists():
        fail("Target path does not exist")
    if p.is_file():
        p.unlink()
        print(json.dumps({{"ok": True, "message": f"Deleted file {{str(p)}}"}}))
    else:
        if recursive:
            shutil.rmtree(p)
        else:
            p.rmdir()
        print(json.dumps({{"ok": True, "message": f"Deleted directory {{str(p)}}"}}))
elif action == "move":
    src = abspath(args.get("from", args.get("fromPath", "")))
    dst = abspath(args.get("to", args.get("toPath", "")))
    s = pathlib.Path(src)
    if not s.exists():
        fail("Source path does not exist")
    ensure_parent(dst)
    pathlib.Path(src).rename(dst)
    print(json.dumps({{"ok": True, "message": f"Moved {{src}} -> {{dst}}"}}))
elif action == "mkdir":
    target = abspath(args.get("path", ""))
    pathlib.Path(target).mkdir(parents=True, exist_ok=True)
    print(json.dumps({{"ok": True, "message": f"Created directory {{target}}"}}))
elif action == "exec":
    cmd = str(args.get("command", "")).strip()
    if not cmd:
        fail("exec requires 'command'")
    out = subprocess.check_output(cmd, shell=True, stderr=subprocess.STDOUT, text=True)
    print(json.dumps({{"ok": True, "output": out}}))
else:
    fail(f"Unsupported SSH action: {{action}}")
"#, payload = shell_single_quote(&payload_json));

  let (stdout, _) = ssh_run_script_over_ssh(conn, interpreter, &["-".to_string()], &script)?;
  parse_json_from_stdout(&stdout)
}

fn ps_single_quote(s: &str) -> String {
  format!("'{}'", s.replace("'", "''"))
}

fn ssh_remote_powershell_with(conn: &SshConnection, payload: Value, shell: &str) -> Result<Value, String> {
  let payload_json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
  let script = format!(r#"$ErrorActionPreference = 'Stop'
$payload = ConvertFrom-Json @'
{payload}
'@
$action = ([string]$payload.action).ToLowerInvariant()
$rootRaw = if ($null -eq $payload.remote_root) {{ '' }} else {{ [string]$payload.remote_root }}
$root = $rootRaw.Trim().Replace('\\','/')
$argsObj = $payload.arguments

function Fail([string]$msg) {{
  @{{ ok = $false; error = $msg }} | ConvertTo-Json -Compress -Depth 8
  exit 1
}}

function NormRel([string]$raw) {{
  $rawVal = if ($null -eq $raw) {{ '.' }} else {{ [string]$raw }}
  $s = $rawVal.Replace('\\','/').Trim()
  if ([string]::IsNullOrWhiteSpace($s)) {{ $s = '.' }}
  $segments = @($s -split '/' | Where-Object {{ $_ -and $_ -ne '.' }})
  if ($segments -contains '..') {{ Fail 'Path escapes remote root' }}
  if ($segments.Count -eq 0) {{ return '.' }}
  return [string]::Join('/', $segments)
}}

function AbsPath([string]$rel) {{
  $n = NormRel $rel
  if ([string]::IsNullOrWhiteSpace($root)) {{ return $n }}
  $base = $root.TrimEnd('/')
  if ($n -eq '.') {{ return $base }}
  return "$base/$n"
}}

if ($action -eq 'list') {{
  $target = AbsPath ([string]$argsObj.path)
  if (-not (Test-Path -LiteralPath $target -PathType Container)) {{ Fail 'Directory does not exist' }}
  $items = Get-ChildItem -LiteralPath $target -Force | Sort-Object @{{ Expression = {{ -not $_.PSIsContainer }} }}, Name
  $out = @()
  $idx = 0
  foreach ($child in $items) {{
    if ($child.Name.StartsWith('.')) {{ continue }}
    $idx++
    $isDir = [bool]$child.PSIsContainer
    $size = if ($isDir) {{ 0 }} else {{ [int64]$child.Length }}
    $relBase = NormRel ([string]$argsObj.path)
    $rel = if ($relBase -eq '.') {{ $child.Name }} else {{ "$relBase/$($child.Name)" }}
    $out += @{{ id = "ssh_entry_$idx"; name = $child.Name; path = $rel; isDir = $isDir; sizeBytes = $size }}
  }}
  @{{ ok = $true; entries = $out }} | ConvertTo-Json -Compress -Depth 8
  exit 0
}}

if ($action -eq 'read') {{
  $target = AbsPath ([string]$argsObj.path)
  if (-not (Test-Path -LiteralPath $target -PathType Leaf)) {{ Fail 'File does not exist' }}
  $maxChars = 20000
  if ($null -ne $argsObj.maxChars) {{ $maxChars = [int]$argsObj.maxChars }} elseif ($null -ne $argsObj.max_chars) {{ $maxChars = [int]$argsObj.max_chars }}
  if ($maxChars -lt 256) {{ $maxChars = 256 }}
  if ($maxChars -gt 300000) {{ $maxChars = 300000 }}
  $content = Get-Content -LiteralPath $target -Raw -Encoding UTF8
  if ($content.Length -gt $maxChars) {{ $content = $content.Substring(0, $maxChars) }}
  @{{ ok = $true; content = $content }} | ConvertTo-Json -Compress -Depth 8
  exit 0
}}

if ($action -eq 'write') {{
  $target = AbsPath ([string]$argsObj.path)
  $content = if ($null -eq $argsObj.content) {{ '' }} else {{ [string]$argsObj.content }}
  $overwrite = if ($null -eq $argsObj.overwrite) {{ $true }} else {{ [bool]$argsObj.overwrite }}
  $createDirs = if ($null -ne $argsObj.createDirs) {{ [bool]$argsObj.createDirs }} elseif ($null -ne $argsObj.create_dirs) {{ [bool]$argsObj.create_dirs }} else {{ $true }}
  if (Test-Path -LiteralPath $target -PathType Container) {{ Fail 'Target path is a directory' }}
  if ((Test-Path -LiteralPath $target -PathType Leaf) -and (-not $overwrite)) {{ Fail 'Target file exists and overwrite=false' }}
  if ($createDirs) {{
    $parent = Split-Path -Parent $target
    if (-not [string]::IsNullOrWhiteSpace($parent)) {{ New-Item -ItemType Directory -Path $parent -Force | Out-Null }}
  }}
  Set-Content -LiteralPath $target -Value $content -Encoding UTF8
  @{{ ok = $true; message = "Wrote $target" }} | ConvertTo-Json -Compress -Depth 8
  exit 0
}}

if ($action -eq 'delete') {{
  $target = AbsPath ([string]$argsObj.path)
  if (-not (Test-Path -LiteralPath $target)) {{ Fail 'Target path does not exist' }}
  $recursive = if ($null -eq $argsObj.recursive) {{ $false }} else {{ [bool]$argsObj.recursive }}
  if (Test-Path -LiteralPath $target -PathType Leaf) {{
    Remove-Item -LiteralPath $target -Force
    @{{ ok = $true; message = "Deleted file $target" }} | ConvertTo-Json -Compress -Depth 8
    exit 0
  }}
  Remove-Item -LiteralPath $target -Recurse:$recursive -Force
  @{{ ok = $true; message = "Deleted directory $target" }} | ConvertTo-Json -Compress -Depth 8
  exit 0
}}

if ($action -eq 'move') {{
  $fromRaw = if ($null -ne $argsObj.from) {{ [string]$argsObj.from }} else {{ [string]$argsObj.fromPath }}
  $toRaw = if ($null -ne $argsObj.to) {{ [string]$argsObj.to }} else {{ [string]$argsObj.toPath }}
  $src = AbsPath $fromRaw
  $dst = AbsPath $toRaw
  if (-not (Test-Path -LiteralPath $src)) {{ Fail 'Source path does not exist' }}
  $parent = Split-Path -Parent $dst
  if (-not [string]::IsNullOrWhiteSpace($parent)) {{ New-Item -ItemType Directory -Path $parent -Force | Out-Null }}
  Move-Item -LiteralPath $src -Destination $dst -Force
  @{{ ok = $true; message = "Moved $src -> $dst" }} | ConvertTo-Json -Compress -Depth 8
  exit 0
}}

if ($action -eq 'mkdir') {{
  $target = AbsPath ([string]$argsObj.path)
  New-Item -ItemType Directory -Path $target -Force | Out-Null
  @{{ ok = $true; message = "Created directory $target" }} | ConvertTo-Json -Compress -Depth 8
  exit 0
}}

if ($action -eq 'exec') {{
  $command = [string]$argsObj.command
  if ([string]::IsNullOrWhiteSpace($command)) {{ Fail "exec requires 'command'" }}
  try {{
    $execOut = (Invoke-Expression $command 2>&1 | Out-String)
    @{{ ok = $true; output = $execOut }} | ConvertTo-Json -Compress -Depth 8
    exit 0
  }} catch {{
    Fail $_.Exception.Message
  }}
}}

Fail "Unsupported SSH action: $action"
"#, payload = ps_single_quote(&payload_json));

  let remote_args = vec!["-NoProfile".to_string(), "-NonInteractive".to_string(), "-Command".to_string(), "-".to_string()];
  let (stdout, _) = ssh_run_script_over_ssh(conn, shell, &remote_args, &script)?;
  parse_json_from_stdout(&stdout)
}

#[tauri::command]
pub fn ssh_tool_call(connection: SshConnection, action: String, arguments: Value) -> Result<Value, String> {
  let payload = serde_json::json!({
    "action": action,
    "remote_root": connection.remote_root,
    "arguments": arguments,
  });
  let mut backend_errors: Vec<String> = Vec::new();

  for interp in ["python3", "python"] {
    match ssh_remote_python_with(&connection, payload.clone(), interp) {
      Ok(mut v) => {
        if let Some(obj) = v.as_object_mut() {
          obj.insert("backend".to_string(), Value::String(interp.to_string()));
        }
        return Ok(v);
      }
      Err(e) => backend_errors.push(format!("{}: {}", interp, e)),
    }
  }

  for shell in ["pwsh", "powershell"] {
    match ssh_remote_powershell_with(&connection, payload.clone(), shell) {
      Ok(mut v) => {
        if let Some(obj) = v.as_object_mut() {
          obj.insert("backend".to_string(), Value::String(shell.to_string()));
        }
        return Ok(v);
      }
      Err(e) => backend_errors.push(format!("{}: {}", shell, e)),
    }
  }

  Err(format!(
    "SSH call failed on all supported remote runtimes (python3/python/pwsh/powershell): {}",
    backend_errors.join(" | ")
  ))
}

fn repo_project_store_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  let dir = memory_dir(app)?;
  Ok(dir.join("repo_project_store.json"))
}

fn dataweb_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  let dir = memory_dir(app)?.join("dataweb");
  create_dir_all(&dir).map_err(|e| e.to_string())?;
  Ok(dir)
}

fn sanitize_dataweb_key(raw: &str) -> String {
  let mut out: String = raw
    .chars()
    .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
    .collect();
  if out.is_empty() {
    out = format!("project_{}", now_ts());
  }
  out
}

fn project_dataweb_file(app: &tauri::AppHandle, project_id: &str) -> Result<PathBuf, String> {
  Ok(dataweb_dir(app)?.join(format!("project_{}.txt", sanitize_dataweb_key(project_id))))
}

fn universal_dataweb_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  Ok(dataweb_dir(app)?.join("universal.txt"))
}

fn ensure_text_file(path: &PathBuf) -> Result<(), String> {
  if !path.exists() {
    fs::write(path, "").map_err(|e| format!("Failed to initialize {}: {}", path.display(), e))?;
  }
  Ok(())
}

fn open_file_portal(path: &PathBuf) -> Result<(), String> {
  if cfg!(target_os = "windows") {
    Command::new("explorer")
      .arg(format!("/select,{}", path.display()))
      .spawn()
      .map_err(|e| e.to_string())?;
  } else if cfg!(target_os = "macos") {
    Command::new("open")
      .arg("-R")
      .arg(path)
      .spawn()
      .map_err(|e| e.to_string())?;
  } else {
    let folder = path.parent().ok_or_else(|| "missing parent folder".to_string())?;
    Command::new("xdg-open")
      .arg(folder)
      .spawn()
      .map_err(|e| e.to_string())?;
  }
  Ok(())
}

#[tauri::command]
pub fn read_project_dataweb(app: tauri::AppHandle, project_id: String) -> Result<String, String> {
  let path = project_dataweb_file(&app, &project_id)?;
  ensure_text_file(&path)?;
  fs::read_to_string(&path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))
}

#[tauri::command]
pub fn write_project_dataweb(app: tauri::AppHandle, project_id: String, content: String) -> Result<(), String> {
  let path = project_dataweb_file(&app, &project_id)?;
  ensure_text_file(&path)?;
  fs::write(&path, content).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

#[tauri::command]
pub fn open_project_dataweb(app: tauri::AppHandle, project_id: String) -> Result<(), String> {
  let path = project_dataweb_file(&app, &project_id)?;
  ensure_text_file(&path)?;
  open_file_portal(&path)
}

#[tauri::command]
pub fn read_universal_dataweb(app: tauri::AppHandle) -> Result<String, String> {
  let path = universal_dataweb_file(&app)?;
  ensure_text_file(&path)?;
  fs::read_to_string(&path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))
}

#[tauri::command]
pub fn write_universal_dataweb(app: tauri::AppHandle, content: String) -> Result<(), String> {
  let path = universal_dataweb_file(&app)?;
  ensure_text_file(&path)?;
  fs::write(&path, content).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

#[tauri::command]
pub fn open_universal_dataweb(app: tauri::AppHandle) -> Result<(), String> {
  let path = universal_dataweb_file(&app)?;
  ensure_text_file(&path)?;
  open_file_portal(&path)
}

fn read_repo_project_store(app: &tauri::AppHandle) -> RepoProjectStore {
  let path = match repo_project_store_file(app) {
    Ok(p) => p,
    Err(_) => return RepoProjectStore::default(),
  };
  if !path.exists() {
    return RepoProjectStore::default();
  }
  let raw = match fs::read_to_string(&path) {
    Ok(s) => s,
    Err(_) => return RepoProjectStore::default(),
  };
  serde_json::from_str::<RepoProjectStore>(&raw).unwrap_or_default()
}

fn write_repo_project_store(app: &tauri::AppHandle, store: &RepoProjectStore) -> Result<(), String> {
  let path = repo_project_store_file(app)?;
  let json = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
  atomic_write_json(&path, &json)
}

fn is_text_like_file(path: &std::path::Path) -> bool {
  let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
  matches!(
    ext.as_str(),
    "txt" | "md" | "rst" | "csv" | "json" | "toml" | "yaml" | "yml" | "xml" | "ini" | "log" |
    "ts" | "tsx" | "js" | "jsx" | "py" | "gd" | "cs" | "rs" | "java" | "cpp" | "c" | "h" |
    "html" | "css" | "scss" | "sql" | "sh" | "bat" | "ps1"
  )
}

fn shallow_dir_size(path: &std::path::Path) -> u64 {
  let mut size = 0u64;
  if let Ok(rd) = read_dir(path) {
    for entry in rd.flatten() {
      if let Ok(m) = entry.metadata() {
        if m.is_file() {
          size = size.saturating_add(m.len());
        }
      }
    }
  }
  size
}

fn scan_repo_entries_internal(root: &std::path::Path, max_items: usize) -> Vec<RepoEntry> {
  let mut out: Vec<RepoEntry> = Vec::new();
  let mut queue: VecDeque<PathBuf> = VecDeque::new();
  queue.push_back(root.to_path_buf());

  while let Some(dir) = queue.pop_front() {
    if out.len() >= max_items {
      break;
    }
    let rd = match read_dir(&dir) {
      Ok(v) => v,
      Err(_) => continue,
    };
    for entry in rd.flatten() {
      if out.len() >= max_items {
        break;
      }
      let p = entry.path();
      let meta = match entry.metadata() {
        Ok(m) => m,
        Err(_) => continue,
      };
      let name = match p.file_name().and_then(|n| n.to_str()) {
        Some(n) => n.to_string(),
        None => continue,
      };
      if name.starts_with('.') {
        continue;
      }

      if meta.is_dir() {
        out.push(RepoEntry {
          id: p.to_string_lossy().to_string(),
          name,
          path: p.to_string_lossy().to_string(),
          is_dir: true,
          size_bytes: shallow_dir_size(&p),
        });
        queue.push_back(p);
      } else if meta.is_file() && is_text_like_file(&p) {
        out.push(RepoEntry {
          id: p.to_string_lossy().to_string(),
          name,
          path: p.to_string_lossy().to_string(),
          is_dir: false,
          size_bytes: meta.len(),
        });
      }
    }
  }

  out.sort_by(|a, b| {
    match (a.is_dir, b.is_dir) {
      (true, false) => std::cmp::Ordering::Less,
      (false, true) => std::cmp::Ordering::Greater,
      _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    }
  });
  out
}

#[tauri::command]
pub fn pick_repo_folder() -> Option<String> {
  rfd::FileDialog::new().pick_folder().map(|p| p.to_string_lossy().to_string())
}

#[tauri::command]
pub fn scan_repo_entries(path: String) -> Result<Vec<RepoEntry>, String> {
  let root = PathBuf::from(path);
  if !root.exists() || !root.is_dir() {
    return Err("Selected path is not a folder".to_string());
  }
  let gate = evaluate_background_queue_gate();
  if gate.delay_ms > 0 {
    std::thread::sleep(Duration::from_millis(gate.delay_ms));
  }
  Ok(scan_repo_entries_internal(&root, gate.repo_entry_cap))
}

#[tauri::command]
pub fn read_repo_entry_excerpt(path: String, max_chars: Option<usize>) -> Result<String, String> {
  let p = PathBuf::from(&path);
  if !p.exists() || !p.is_file() {
    return Err("Path is not a readable file".to_string());
  }
  if !is_text_like_file(&p) {
    return Err("Unsupported file format for text excerpt".to_string());
  }
  let raw = fs::read_to_string(&p).map_err(|e| format!("Failed to read {}: {}", p.display(), e))?;
  let cap = max_chars.unwrap_or(2400).max(200).min(20000);
  let out: String = raw.chars().take(cap).collect();
  Ok(out)
}

fn resolve_within_root(root: &str, rel_path: &str) -> Result<PathBuf, String> {
  let root_path = PathBuf::from(root);
  if !root_path.exists() || !root_path.is_dir() {
    return Err("Repository root is not a folder".to_string());
  }
  let root_canon = fs::canonicalize(&root_path).map_err(|e| format!("Invalid repo root: {}", e))?;
  let joined = root_path.join(rel_path);

  let mut out = if joined.exists() {
    fs::canonicalize(&joined).map_err(|e| format!("Invalid target path: {}", e))?
  } else {
    let parent = joined.parent().unwrap_or(&root_path);
    let parent_canon = fs::canonicalize(parent).map_err(|e| format!("Invalid target parent: {}", e))?;
    let leaf = joined.file_name().and_then(|n| n.to_str()).ok_or_else(|| "Invalid target leaf".to_string())?;
    parent_canon.join(leaf)
  };

  if !out.starts_with(&root_canon) {
    return Err("Path escapes repository root".to_string());
  }
  if out.as_os_str().is_empty() {
    out = root_canon;
  }
  Ok(out)
}

#[tauri::command]
pub fn fs_list_dir(root: String, path: String) -> Result<Vec<RepoEntry>, String> {
  let p = resolve_within_root(&root, path.trim())?;
  if !p.exists() || !p.is_dir() {
    return Err("Directory does not exist".to_string());
  }
  let mut out: Vec<RepoEntry> = Vec::new();
  let rd = read_dir(&p).map_err(|e| format!("Failed to read directory: {}", e))?;
  for entry in rd.flatten() {
    let ep = entry.path();
    let meta = match entry.metadata() {
      Ok(m) => m,
      Err(_) => continue,
    };
    let name = match ep.file_name().and_then(|n| n.to_str()) {
      Some(n) => n.to_string(),
      None => continue,
    };
    if name.starts_with('.') {
      continue;
    }
    let rel = ep
      .strip_prefix(&root)
      .ok()
      .map(|r| r.to_string_lossy().to_string())
      .unwrap_or_else(|| ep.to_string_lossy().to_string());
    out.push(RepoEntry {
      id: format!("repo_entry_{}_{}", now_ts(), out.len() + 1),
      name,
      path: rel.replace('\\', "/"),
      is_dir: meta.is_dir(),
      size_bytes: if meta.is_dir() { shallow_dir_size(&ep) } else { meta.len() },
    });
  }
  out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
    (true, false) => std::cmp::Ordering::Less,
    (false, true) => std::cmp::Ordering::Greater,
    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
  });
  Ok(out)
}

#[tauri::command]
pub fn fs_read_text(root: String, path: String, max_chars: Option<usize>) -> Result<String, String> {
  let p = resolve_within_root(&root, path.trim())?;
  if !p.exists() || !p.is_file() {
    return Err("File does not exist".to_string());
  }
  if !is_text_like_file(&p) {
    return Err("Unsupported file format for text read".to_string());
  }
  let raw = fs::read_to_string(&p).map_err(|e| format!("Failed to read {}: {}", p.display(), e))?;
  let cap = max_chars.unwrap_or(20000).max(256).min(300000);
  Ok(raw.chars().take(cap).collect())
}

#[tauri::command]
pub fn fs_write_text(
  root: String,
  path: String,
  content: String,
  create_dirs: Option<bool>,
  overwrite: Option<bool>,
) -> Result<String, String> {
  let p = resolve_within_root(&root, path.trim())?;
  if p.exists() && p.is_dir() {
    return Err("Target path is a directory".to_string());
  }
  if p.exists() && !overwrite.unwrap_or(true) {
    return Err("Target file exists and overwrite=false".to_string());
  }
  if create_dirs.unwrap_or(true) {
    if let Some(parent) = p.parent() {
      create_dir_all(parent).map_err(|e| format!("Failed creating directories: {}", e))?;
    }
  }
  fs::write(&p, content).map_err(|e| format!("Failed writing file: {}", e))?;
  Ok(format!("Wrote {}", p.display()))
}

#[tauri::command]
pub fn fs_delete_path(root: String, path: String, recursive: Option<bool>) -> Result<String, String> {
  let p = resolve_within_root(&root, path.trim())?;
  if !p.exists() {
    return Err("Target path does not exist".to_string());
  }
  if p.is_file() {
    fs::remove_file(&p).map_err(|e| format!("Failed deleting file: {}", e))?;
    return Ok(format!("Deleted file {}", p.display()));
  }
  if recursive.unwrap_or(false) {
    fs::remove_dir_all(&p).map_err(|e| format!("Failed deleting directory: {}", e))?;
    Ok(format!("Deleted directory {}", p.display()))
  } else {
    fs::remove_dir(&p).map_err(|e| format!("Failed deleting directory (use recursive=true for non-empty): {}", e))?;
    Ok(format!("Deleted directory {}", p.display()))
  }
}

#[tauri::command]
pub fn fs_move_path(root: String, from_path: String, to_path: String) -> Result<String, String> {
  let from = resolve_within_root(&root, from_path.trim())?;
  let to = resolve_within_root(&root, to_path.trim())?;
  if !from.exists() {
    return Err("Source path does not exist".to_string());
  }
  if let Some(parent) = to.parent() {
    create_dir_all(parent).map_err(|e| format!("Failed creating destination directories: {}", e))?;
  }
  fs::rename(&from, &to).map_err(|e| format!("Failed moving path: {}", e))?;
  Ok(format!("Moved {} -> {}", from.display(), to.display()))
}

#[tauri::command]
pub fn fs_make_dir(root: String, path: String) -> Result<String, String> {
  let p = resolve_within_root(&root, path.trim())?;
  create_dir_all(&p).map_err(|e| format!("Failed creating directory: {}", e))?;
  Ok(format!("Created directory {}", p.display()))
}

#[tauri::command]
pub fn get_repo_project_store(app: tauri::AppHandle) -> RepoProjectStore {
  read_repo_project_store(&app)
}

#[tauri::command]
pub fn save_repo_project_store(app: tauri::AppHandle, store: RepoProjectStore) -> Result<(), String> {
  write_repo_project_store(&app, &store)
}

#[tauri::command]
pub fn clone_repo_into_folder(repo_url: String, destination_root: String) -> Result<String, String> {
  let url = repo_url.trim();
  if url.is_empty() {
    return Err("Repository URL is required".to_string());
  }

  let git_exec = resolve_git_executable().ok_or_else(|| {
    "Git is not installed (or not discoverable yet). Install Git for Windows and restart Iris before using repo install.".to_string()
  })?;

  let root = PathBuf::from(destination_root.trim());
  if !root.exists() || !root.is_dir() {
    return Err("Destination folder is invalid".to_string());
  }

  let mut inferred = url
    .split('/')
    .last()
    .unwrap_or("repo")
    .trim_end_matches(".git")
    .trim()
    .to_string();
  if inferred.is_empty() {
    inferred = format!("repo_{}", now_ts());
  }
  let target = root.join(&inferred);
  if target.exists() {
    return Err(format!("Target folder already exists: {}", target.display()));
  }

  let output = Command::new(&git_exec)
    .arg("clone")
    .arg("--depth")
    .arg("1")
    .arg(url)
    .arg(&target)
    .output()
    .map_err(|e| {
      if e.kind() == std::io::ErrorKind::NotFound {
        "Git is not installed (or not in PATH). Install Git for Windows and restart Iris before using repo install.".to_string()
      } else {
        format!("Failed to run git clone: {}", e)
      }
    })?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    return Err(format!("git clone failed: {}", stderr.trim()));
  }

  Ok(format!("Installed repository to {}", target.display()))
}

#[tauri::command]
pub fn update_repo_from_remote(repo_path: String) -> Result<String, String> {
  let git_exec = resolve_git_executable().ok_or_else(|| {
    "Git is not installed (or not discoverable yet). Install Git for Windows and restart Iris before updating repos.".to_string()
  })?;

  let root = PathBuf::from(repo_path.trim());
  if !root.exists() || !root.is_dir() {
    return Err("Repository folder is invalid".to_string());
  }
  if !root.join(".git").exists() {
    return Err("Selected folder is not a git repository (.git missing)".to_string());
  }

  let fetch = Command::new(&git_exec)
    .arg("-C")
    .arg(&root)
    .arg("fetch")
    .arg("--all")
    .arg("--prune")
    .output()
    .map_err(|e| format!("Failed to run git fetch: {}", e))?;
  if !fetch.status.success() {
    let stderr = String::from_utf8_lossy(&fetch.stderr).trim().to_string();
    return Err(if stderr.is_empty() {
      "git fetch failed".to_string()
    } else {
      format!("git fetch failed: {}", stderr)
    });
  }

  let pull = Command::new(&git_exec)
    .arg("-C")
    .arg(&root)
    .arg("pull")
    .arg("--ff-only")
    .output()
    .map_err(|e| format!("Failed to run git pull: {}", e))?;
  if !pull.status.success() {
    let stderr = String::from_utf8_lossy(&pull.stderr).trim().to_string();
    return Err(if stderr.is_empty() {
      "git pull failed".to_string()
    } else {
      format!("git pull failed: {}", stderr)
    });
  }

  let output = String::from_utf8_lossy(&pull.stdout).trim().to_string();
  let final_msg = if output.is_empty() {
    format!("Repository updated: {}", root.display())
  } else {
    format!("{}\n({})", output, root.display())
  };
  Ok(final_msg)
 }

#[tauri::command]
pub fn install_repo_dependencies() -> Result<String, String> {
  // If Git is already present, no work needed.
  if resolve_git_executable().is_some() {
    return Ok("Git is already installed and available. You can retry repo installation now.".to_string());
  }

  if cfg!(target_os = "windows") {
    // Prefer winget for standard Windows installs.
    if let Ok(out) = Command::new("winget")
      .arg("install")
      .arg("--id")
      .arg("Git.Git")
      .arg("-e")
      .arg("--accept-source-agreements")
      .arg("--accept-package-agreements")
      .output()
    {
      if out.status.success() {
        return Ok("Git installation command completed via winget. Restart Iris, then try the repo install again.".to_string());
      }
    }

    // Fallback for users with Chocolatey.
    if let Ok(out) = Command::new("choco")
      .arg("install")
      .arg("git")
      .arg("-y")
      .output()
    {
      if out.status.success() {
        return Ok("Git installation command completed via Chocolatey. Restart Iris, then try the repo install again.".to_string());
      }
    }

    return Err("Automatic install could not run (winget/choco unavailable or blocked). Install Git for Windows manually, then restart Iris.".to_string());
  }

  Err("Automatic dependency install is currently implemented for Windows. Please install Git manually and restart Iris.".to_string())
}

#[tauri::command]
pub async fn network_lookup(query: String) -> Result<String, String> {
  let q = query.trim();
  if q.is_empty() {
    return Ok(String::new());
  }

  let client = reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(3))
    .timeout(Duration::from_secs(8))
    .build()
    .map_err(|e| e.to_string())?;

  let resp = client
    .get("https://api.duckduckgo.com/")
    .query(&[("q", q), ("format", "json"), ("no_html", "1"), ("skip_disambig", "1")])
    .send()
    .await
    .map_err(|e| format!("Network lookup failed: {}", e))?;

  let json: Value = resp.json().await.map_err(|e| format!("Invalid network response: {}", e))?;
  let heading = json.get("Heading").and_then(|v| v.as_str()).unwrap_or("");
  let abstract_text = json.get("AbstractText").and_then(|v| v.as_str()).unwrap_or("");

  let mut lines: Vec<String> = Vec::new();
  if !heading.is_empty() {
    lines.push(format!("Heading: {}", heading));
  }
  if !abstract_text.is_empty() {
    lines.push(format!("Summary: {}", abstract_text));
  }

  if let Some(arr) = json.get("RelatedTopics").and_then(|v| v.as_array()) {
    let mut count = 0usize;
    for item in arr {
      if count >= 4 {
        break;
      }
      if let Some(text) = item.get("Text").and_then(|v| v.as_str()) {
        if !text.trim().is_empty() {
          lines.push(format!("- {}", text.trim()));
          count += 1;
        }
      } else if let Some(nested) = item.get("Topics").and_then(|v| v.as_array()) {
        for sub in nested {
          if count >= 4 {
            break;
          }
          if let Some(text) = sub.get("Text").and_then(|v| v.as_str()) {
            if !text.trim().is_empty() {
              lines.push(format!("- {}", text.trim()));
              count += 1;
            }
          }
        }
      }
    }
  }

  if lines.is_empty() {
    return Ok("No reliable network snippets were returned for this query.".to_string());
  }
  Ok(lines.join("\n"))
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetworkHit {
  pub title: String,
  pub url: String,
  pub snippet: String,
  pub score: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSearchResponse {
  pub query: String,
  pub summary: String,
  pub hits: Vec<NetworkHit>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct WeatherLookupResponse {
  pub location: String,
  pub city: String,
  pub region: String,
  pub temperature_f: f64,
  pub weather_conditions: String,
  pub day_label: String,
  pub summary: String,
  pub source_url: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct SanitizedWeatherPayload {
  city: String,
  region: String,
  temperature_f: f64,
  weather_conditions: String,
}

#[derive(Clone, Debug)]
struct WeatherGeoTarget {
  lat: f64,
  lon: f64,
  city: String,
  region: String,
  country: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct UvLookupResponse {
  pub location: String,
  pub uv_index: f64,
  pub category: String,
  pub observed_at: String,
  pub source_url: String,
}

fn uv_index_category(value: f64) -> &'static str {
  if value < 3.0 {
    "low"
  } else if value < 6.0 {
    "moderate"
  } else if value < 8.0 {
    "high"
  } else if value < 11.0 {
    "very high"
  } else {
    "extreme"
  }
}

fn weather_code_label(code: i64) -> &'static str {
  match code {
    0 => "clear sky",
    1 | 2 => "partly cloudy",
    3 => "overcast",
    45 | 48 => "foggy",
    51 | 53 | 55 => "drizzle",
    56 | 57 => "freezing drizzle",
    61 | 63 | 65 => "rain",
    66 | 67 => "freezing rain",
    71 | 73 | 75 | 77 => "snow",
    80 | 81 | 82 => "rain showers",
    85 | 86 => "snow showers",
    95 => "thunderstorms",
    96 | 99 => "thunderstorms with hail",
    _ => "mixed conditions",
  }
}

fn score_geocode_result(result: &Value, raw_location: &str) -> f64 {
  let raw_lower = raw_location.trim().to_lowercase();
  let query_tokens = keyword_set(&raw_lower)
    .into_iter()
    .filter(|token| token.len() > 1)
    .collect::<HashSet<_>>();
  let name = result.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
  let admin1 = result.get("admin1").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
  let country = result.get("country").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
  let postcode = result.get("postcode").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
  let feature_code = result.get("feature_code").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
  let label = format!("{} {} {} {}", name, admin1, country, postcode);
  let label_tokens = keyword_set(&label);

  let overlap = query_tokens.iter().filter(|token| label_tokens.contains(*token)).count() as f64;
  let mut score = overlap * 1.35;

  if !name.is_empty() && (raw_lower == name || raw_lower.starts_with(&(name.clone() + ","))) {
    score += 3.2;
  }
  if !admin1.is_empty() && raw_lower.contains(&admin1) {
    score += 1.2;
  }
  if !postcode.is_empty() && raw_lower.contains(&postcode) {
    score += 4.0;
  }

  if feature_code.starts_with("PPL") || feature_code.starts_with("ADM") {
    score += 3.0;
  }
  if feature_code == "PPLA" || feature_code == "PPLA2" || feature_code == "PPLC" {
    score += 1.5;
  }

  if name.chars().any(|c| c.is_ascii_digit()) {
    score -= 3.0;
  }

  let population = result
    .get("population")
    .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|n| n as f64)))
    .unwrap_or(0.0);
  if population > 0.0 {
    score += (population.max(1.0).log10() - 3.0).clamp(0.0, 2.5);
  }

  score
}

async fn geocode_weather_location(client: &reqwest::Client, raw_location: &str) -> Result<WeatherGeoTarget, String> {
  let trimmed = raw_location.trim();
  if trimmed.is_empty() {
    return Err("Location is required".to_string());
  }

  let normalized_space = trimmed.replace(',', " ");
  let normalized_compact = normalized_space.split_whitespace().collect::<Vec<_>>().join(" ");
  let normalized_us = if normalized_compact.to_lowercase().contains(" usa") || normalized_compact.to_lowercase().contains(" united states") {
    normalized_compact.clone()
  } else {
    format!("{}, USA", normalized_compact)
  };

  let query_variants = vec![
    trimmed.to_string(),
    normalized_compact.clone(),
    normalized_us.clone(),
  ];

  for query in query_variants {
    let geo_resp = client
      .get("https://geocoding-api.open-meteo.com/v1/search")
      .query(&[("name", query.as_str()), ("count", "8"), ("language", "en"), ("format", "json")])
      .send()
      .await
      .map_err(|e| format!("Weather geocoding failed: {}", e))?;

    let geo_json: Value = geo_resp.json().await.map_err(|e| format!("Invalid geocoding response: {}", e))?;
    if let Some(result) = geo_json
      .get("results")
      .and_then(|v| v.as_array())
      .and_then(|arr| {
        arr.iter().max_by(|a, b| {
          score_geocode_result(a, trimmed)
            .partial_cmp(&score_geocode_result(b, trimmed))
            .unwrap_or(std::cmp::Ordering::Equal)
        })
      })
    {
      let lat = result.get("latitude").and_then(|v| v.as_f64()).ok_or_else(|| "Missing latitude".to_string())?;
      let lon = result.get("longitude").and_then(|v| v.as_f64()).ok_or_else(|| "Missing longitude".to_string())?;
      let city = result.get("name").and_then(|v| v.as_str()).unwrap_or(trimmed);
      let admin1 = result.get("admin1").and_then(|v| v.as_str()).unwrap_or("");
      let country = result.get("country").and_then(|v| v.as_str()).unwrap_or("");
      return Ok(WeatherGeoTarget {
        lat,
        lon,
        city: city.to_string(),
        region: admin1.to_string(),
        country: country.to_string(),
      });
    }
  }

  let nominatim_resp = client
    .get("https://nominatim.openstreetmap.org/search")
    .header("User-Agent", "iris-app-weather/1.0")
    .query(&[("q", normalized_us.as_str()), ("format", "jsonv2"), ("limit", "1"), ("addressdetails", "1")])
    .send()
    .await
    .map_err(|e| format!("Fallback geocoding failed: {}", e))?;

  let nominatim_json: Value = nominatim_resp.json().await.map_err(|e| format!("Invalid fallback geocoding response: {}", e))?;
  let fallback = nominatim_json
    .as_array()
    .and_then(|arr| arr.first())
    .ok_or_else(|| format!("No weather location match found for '{}'", trimmed))?;

  let lat = fallback
    .get("lat")
    .and_then(|v| v.as_str())
    .and_then(|s| s.parse::<f64>().ok())
    .ok_or_else(|| "Missing fallback latitude".to_string())?;
  let lon = fallback
    .get("lon")
    .and_then(|v| v.as_str())
    .and_then(|s| s.parse::<f64>().ok())
    .ok_or_else(|| "Missing fallback longitude".to_string())?;

  let addr = fallback.get("address").cloned().unwrap_or(Value::Null);
  let city = addr
    .get("city")
    .or_else(|| addr.get("town"))
    .or_else(|| addr.get("village"))
    .or_else(|| addr.get("municipality"))
    .and_then(|v| v.as_str())
    .unwrap_or(trimmed)
    .to_string();
  let region = addr
    .get("state")
    .or_else(|| addr.get("region"))
    .or_else(|| addr.get("county"))
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_string();
  let country = addr
    .get("country")
    .and_then(|v| v.as_str())
    .unwrap_or("")
    .to_string();

  Ok(WeatherGeoTarget {
    lat,
    lon,
    city,
    region,
    country,
  })
}

#[tauri::command]
pub async fn weather_lookup(location: String, day_offset: Option<u8>) -> Result<WeatherLookupResponse, String> {
  let q = location.trim();
  if q.is_empty() {
    return Err("Location is required".to_string());
  }

  let client = reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(3))
    .timeout(Duration::from_secs(8))
    .build()
    .map_err(|e| e.to_string())?;

  let geo = geocode_weather_location(&client, q).await?;

  let forecast_resp = client
    .get("https://api.open-meteo.com/v1/forecast")
    .query(&[
      ("latitude", geo.lat.to_string()),
      ("longitude", geo.lon.to_string()),
      ("daily", "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max".to_string()),
      ("timezone", "auto".to_string()),
      ("forecast_days", "4".to_string()),
      ("temperature_unit", "fahrenheit".to_string()),
    ])
    .send()
    .await
    .map_err(|e| format!("Weather forecast failed: {}", e))?;

  let forecast_json: Value = forecast_resp.json().await.map_err(|e| format!("Invalid forecast response: {}", e))?;
  let daily = forecast_json.get("daily").ok_or_else(|| "Forecast data missing".to_string())?;
  let index = usize::from(day_offset.unwrap_or(0).min(3));

  let times = daily.get("time").and_then(|v| v.as_array()).ok_or_else(|| "Forecast days missing".to_string())?;
  let maxes = daily.get("temperature_2m_max").and_then(|v| v.as_array()).ok_or_else(|| "Max temperatures missing".to_string())?;
  let mins = daily.get("temperature_2m_min").and_then(|v| v.as_array()).ok_or_else(|| "Min temperatures missing".to_string())?;
  let precip = daily.get("precipitation_probability_max").and_then(|v| v.as_array()).ok_or_else(|| "Precipitation data missing".to_string())?;
  let codes = daily.get("weather_code").and_then(|v| v.as_array()).ok_or_else(|| "Weather codes missing".to_string())?;

  if index >= times.len() || index >= maxes.len() || index >= mins.len() || index >= precip.len() || index >= codes.len() {
    return Err("Requested forecast day is unavailable".to_string());
  }

  let date = times[index].as_str().unwrap_or("");
  let max_temp = maxes[index].as_f64().unwrap_or(0.0);
  let min_temp = mins[index].as_f64().unwrap_or(0.0);
  let precip_prob = precip[index].as_i64().unwrap_or(0);
  let code = codes[index].as_i64().unwrap_or(-1);
  let day_label = if index == 0 { "today" } else if index == 1 { "tomorrow" } else { date };
  let sanitized = SanitizedWeatherPayload {
    city: geo.city.clone(),
    region: geo.region.clone(),
    temperature_f: max_temp,
    weather_conditions: weather_code_label(code).to_string(),
  };
  let location_label = [geo.city.as_str(), geo.region.as_str(), geo.country.as_str()]
    .into_iter()
    .filter(|s| !s.trim().is_empty())
    .collect::<Vec<_>>()
    .join(", ");
  let summary = format!(
    "{} with a high around {:.0} F, a low around {:.0} F, and about a {}% chance of precipitation.",
    sanitized.weather_conditions,
    max_temp,
    min_temp,
    precip_prob
  );

  Ok(WeatherLookupResponse {
    location: location_label,
    city: sanitized.city,
    region: sanitized.region,
    temperature_f: sanitized.temperature_f,
    weather_conditions: sanitized.weather_conditions,
    day_label: day_label.to_string(),
    summary,
    source_url: "https://open-meteo.com/".to_string(),
  })
}

#[tauri::command]
pub async fn uv_lookup(location: String) -> Result<UvLookupResponse, String> {
  let q = location.trim();
  if q.is_empty() {
    return Err("Location is required".to_string());
  }

  let client = reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(3))
    .timeout(Duration::from_secs(8))
    .build()
    .map_err(|e| e.to_string())?;

  let geo = geocode_weather_location(&client, q).await?;
  let location_label = [geo.city.as_str(), geo.region.as_str(), geo.country.as_str()]
    .into_iter()
    .filter(|s| !s.trim().is_empty())
    .collect::<Vec<_>>()
    .join(", ");

  let resp = client
    .get("https://api.open-meteo.com/v1/forecast")
    .query(&[
      ("latitude", geo.lat.to_string()),
      ("longitude", geo.lon.to_string()),
      ("current", "uv_index".to_string()),
      ("hourly", "uv_index".to_string()),
      ("timezone", "auto".to_string()),
      ("forecast_days", "1".to_string()),
    ])
    .send()
    .await
    .map_err(|e| format!("UV lookup failed: {}", e))?;

  let json: Value = resp.json().await.map_err(|e| format!("Invalid UV response: {}", e))?;
  let current = json.get("current");
  let current_uv = current.and_then(|v| v.get("uv_index")).and_then(|v| v.as_f64());
  let current_time = current.and_then(|v| v.get("time")).and_then(|v| v.as_str()).unwrap_or("");

  let (uv_index, observed_at) = if let Some(value) = current_uv {
    (value, current_time.to_string())
  } else {
    let hourly = json.get("hourly").ok_or_else(|| "UV data missing".to_string())?;
    let times = hourly.get("time").and_then(|v| v.as_array()).ok_or_else(|| "Hourly UV times missing".to_string())?;
    let values = hourly.get("uv_index").and_then(|v| v.as_array()).ok_or_else(|| "Hourly UV index missing".to_string())?;
    let idx = values.iter().position(|v| v.is_number()).ok_or_else(|| "No hourly UV values available".to_string())?;
    let value = values[idx].as_f64().ok_or_else(|| "Invalid hourly UV value".to_string())?;
    let observed = times.get(idx).and_then(|v| v.as_str()).unwrap_or("").to_string();
    (value, observed)
  };

  Ok(UvLookupResponse {
    location: location_label,
    uv_index,
    category: uv_index_category(uv_index).to_string(),
    observed_at,
    source_url: "https://open-meteo.com/".to_string(),
  })
}

fn keyword_set(s: &str) -> HashSet<String> {
  s.to_lowercase()
    .split(|c: char| !c.is_ascii_alphanumeric())
    .filter(|t| t.len() >= 3)
    .map(|t| t.to_string())
    .collect()
}

fn score_network_hit(hit: &NetworkHit, context: &HashSet<String>, query_lower: &str) -> f32 {
  let mut score = 0.0f32;
  let corpus = format!("{} {} {}", hit.title, hit.snippet, hit.url).to_lowercase();
  let terms = keyword_set(&corpus);

  let overlap = context.iter().filter(|t| terms.contains(*t)).count() as f32;
  score += overlap * 2.5;

  if corpus.contains(query_lower) {
    score += 5.0;
  }
  if hit.url.contains("wikipedia.org") || hit.url.contains("github.com") || hit.url.contains("docs") {
    score += 1.5;
  }
  score
}

fn add_related_topics_hits(value: &Value, out: &mut Vec<NetworkHit>) {
  match value {
    Value::Array(arr) => {
      for item in arr {
        add_related_topics_hits(item, out);
      }
    }
    Value::Object(map) => {
      if let (Some(text), Some(url)) = (
        map.get("Text").and_then(|v| v.as_str()),
        map.get("FirstURL").and_then(|v| v.as_str()),
      ) {
        let title = text.split(" - ").next().unwrap_or(text).trim().to_string();
        out.push(NetworkHit {
          title,
          url: url.to_string(),
          snippet: text.trim().to_string(),
          score: 0.0,
        });
      }
      if let Some(topics) = map.get("Topics") {
        add_related_topics_hits(topics, out);
      }
    }
    _ => {}
  }
}

fn is_news_query(query: &str) -> bool {
  let q = query.to_lowercase();
  q.contains("news") || q.contains("headline") || q.contains("current events") || q.contains("breaking")
}

fn parse_duckduckgo_lite_hits(html: &str, max_results: usize) -> Vec<NetworkHit> {
  let mut out = Vec::new();
  let doc = Html::parse_document(html);
  let selector = match Selector::parse("a") {
    Ok(s) => s,
    Err(_) => return out,
  };

  for anchor in doc.select(&selector) {
    if out.len() >= max_results {
      break;
    }
    let text = anchor.text().collect::<Vec<_>>().join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
    let href = anchor.value().attr("href").unwrap_or("").trim().to_string();
    if text.is_empty() || href.is_empty() {
      continue;
    }
    if !href.starts_with("http") {
      continue;
    }
    if href.contains("duckduckgo.com") {
      continue;
    }
    out.push(NetworkHit {
      title: text.clone(),
      url: href,
      snippet: text,
      score: 0.0,
    });
  }

  out
}

fn parse_news_rss_hits(xml: &str, max_results: usize) -> Vec<NetworkHit> {
  let mut out = Vec::new();
  let item_re = match Regex::new(r"(?is)<item>(.*?)</item>") {
    Ok(r) => r,
    Err(_) => return out,
  };
  let title_re = match Regex::new(r"(?is)<title>(.*?)</title>") {
    Ok(r) => r,
    Err(_) => return out,
  };
  let link_re = match Regex::new(r"(?is)<link>(.*?)</link>") {
    Ok(r) => r,
    Err(_) => return out,
  };
  let desc_re = match Regex::new(r"(?is)<description>(.*?)</description>") {
    Ok(r) => r,
    Err(_) => return out,
  };

  for cap in item_re.captures_iter(xml) {
    if out.len() >= max_results {
      break;
    }
    let block = cap.get(1).map(|m| m.as_str()).unwrap_or("");
    let title = title_re
      .captures(block)
      .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
      .unwrap_or_default();
    let link = link_re
      .captures(block)
      .and_then(|c| c.get(1).map(|m| m.as_str().trim().to_string()))
      .unwrap_or_default();
    let desc = desc_re
      .captures(block)
      .and_then(|c| c.get(1).map(|m| m.as_str().replace("<![CDATA[", "").replace("]]>", "")))
      .unwrap_or_default();

    if title.is_empty() || link.is_empty() {
      continue;
    }
    out.push(NetworkHit {
      title: title.clone(),
      url: link,
      snippet: title,
      score: if desc.is_empty() { 0.0 } else { 0.25 },
    });
  }

  out
}

#[tauri::command]
pub async fn network_search(query: String, project_context: Option<String>) -> Result<NetworkSearchResponse, String> {
  let q = query.trim();
  if q.is_empty() {
    return Ok(NetworkSearchResponse::default());
  }

  let gate = evaluate_background_queue_gate();
  if gate.delay_ms > 0 {
    tokio::time::sleep(Duration::from_millis(gate.delay_ms)).await;
  }

  let client = reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(3))
    .timeout(Duration::from_secs(if gate.mode == BackgroundQueueMode::Normal { 8 } else { 6 }))
    .build()
    .map_err(|e| e.to_string())?;

  let resp = client
    .get("https://api.duckduckgo.com/")
    .header("User-Agent", "iris-app-network-search/1.0")
    .query(&[("q", q), ("format", "json"), ("no_html", "1"), ("skip_disambig", "1")])
    .send()
    .await
    .map_err(|e| format!("Network search failed: {}", e))?;

  let json: Value = resp.json().await.map_err(|e| format!("Invalid network response: {}", e))?;

  let mut hits: Vec<NetworkHit> = Vec::new();

  let heading = json.get("Heading").and_then(|v| v.as_str()).unwrap_or("").trim();
  let abstract_text = json.get("AbstractText").and_then(|v| v.as_str()).unwrap_or("").trim();
  let abstract_url = json.get("AbstractURL").and_then(|v| v.as_str()).unwrap_or("").trim();
  if !heading.is_empty() || !abstract_text.is_empty() {
    hits.push(NetworkHit {
      title: if !heading.is_empty() { heading.to_string() } else { "Result".to_string() },
      url: abstract_url.to_string(),
      snippet: abstract_text.to_string(),
      score: 0.0,
    });
  }

  if let Some(related) = json.get("RelatedTopics") {
    add_related_topics_hits(related, &mut hits);
  }

  if hits.is_empty() {
    let lite_resp = client
      .get("https://lite.duckduckgo.com/lite/")
      .header("User-Agent", "iris-app-network-search/1.0")
      .query(&[("q", q)])
      .send()
      .await;
    if let Ok(resp) = lite_resp {
      if let Ok(body) = resp.text().await {
        hits.extend(parse_duckduckgo_lite_hits(&body, 3));
      }
    }
  }

  if hits.is_empty() && is_news_query(q) {
    let rss_resp = client
      .get("https://news.google.com/rss/search")
      .header("User-Agent", "iris-app-network-search/1.0")
      .query(&[("q", q), ("hl", "en-US"), ("gl", "US"), ("ceid", "US:en")])
      .send()
      .await;
    if let Ok(resp) = rss_resp {
      if let Ok(body) = resp.text().await {
        hits.extend(parse_news_rss_hits(&body, 3));
      }
    }
  }

  // Deduplicate by URL+title to keep output compact.
  let mut seen = HashSet::new();
  hits.retain(|h| {
    let key = format!("{}|{}", h.url.to_lowercase(), h.title.to_lowercase());
    if seen.contains(&key) {
      false
    } else {
      seen.insert(key);
      true
    }
  });

  let relevance_context = keyword_set(&format!("{} {}", q, project_context.unwrap_or_default()));
  let query_lower = q.to_lowercase();
  for h in &mut hits {
    h.score = score_network_hit(h, &relevance_context, &query_lower);
  }
  hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
  hits.truncate(gate.network_hit_cap);

  let summary = if hits.is_empty() {
    "No reliable network snippets were returned for this query.".to_string()
  } else {
    hits.iter()
      .take(4)
      .enumerate()
      .map(|(i, h)| format!("{}. {}{}", i + 1, h.snippet, if h.url.is_empty() { "".to_string() } else { format!(" ({})", h.url) }))
      .collect::<Vec<_>>()
      .join("\n")
  };

  Ok(NetworkSearchResponse {
    query: q.to_string(),
    summary,
    hits,
  })
}

fn compile_context_from_mem(mem: &TabMemory, token_budget: usize) -> CompiledContext {
  let budget = token_budget;
  let mut arts = mem.artifacts.clone();
  arts.sort_by_key(|a| a.ts);
  let recent_artifacts: Vec<Artifact> = arts.into_iter().rev().take(2).collect();

  let reserve = 300usize;
  let mut transcript = String::new();
  let mut used = 0usize;
  for m in mem.messages.iter().rev() {
    let line = format!("{}: {}\n", if m.role == "user" { "User" } else { "Iris" }, m.text);
    let est = line.len() / 4;
    if used + est + reserve > budget { break; }
    used += est;
    transcript.push_str(&line);
  }
  let recent_transcript = transcript.lines().rev().collect::<Vec<_>>().join("\n");

  CompiledContext {
    micro_summary: mem.micro_summary.clone(),
    dialogue_bullets: mem.dialogue_bullets.clone(),
    recent_transcript,
    recent_artifacts,
  }
}

fn load_tab(app: &tauri::AppHandle, tab_id: u32) -> Result<TabMemory, String> {
  // Prefer reading the Snapshot format; fall back handled by iris_read_snapshot_file
  let p = tab_file(app, tab_id)?;
  if !p.exists() {
    return Ok(TabMemory { tab_id, ..Default::default() });
  }

  match iris_read_snapshot_file(&p) {
    Ok(snap) => {
      // Convert Snapshot â†’ TabMemory for callers that expect TabMemory
      let mut mem = TabMemory {
        tab_id,
        title: snap.title,
        messages: snap.messages,
        artifacts: snap.artifacts,
        micro_summary: snap.micro_summary,
        dialogue_bullets: snap.dialogue_bullets,
        summary: snap.summary,
        is_closed: false,
        last_updated: snap.last_updated.unwrap_or_else(now_ts),
      };
      // Ensure timestamps are present
      let now = now_ts();
      for m in &mut mem.messages { if m.time == 0 { m.time = now; } }
      Ok(mem)
    }
    Err(_) => {
      // As a last resort, try to parse as legacy TabMemory
      let s = fs::read_to_string(p).map_err(|e| e.to_string())?;
      serde_json::from_str(&s).map_err(|e| e.to_string())
    }
  }
}

fn save_tab(app: &tauri::AppHandle, mem: &TabMemory) -> Result<(), String> {
    let p = tab_file(app, mem.tab_id)?;
    println!("Saving tab memory to: {:?}", p); // <-- Add this line
    let s = serde_json::to_string_pretty(mem).map_err(|e| e.to_string())?;
    atomic_write_json(&p, &s)
}

#[tauri::command]
pub fn show_devtools(app: tauri::AppHandle) -> Result<(), String> {
  // Devtools are not enabled in this build; keep the command to satisfy frontend calls.
  if app.get_webview_window("main").is_some() {
    Err("Devtools are disabled in this build".into())
  } else {
    Err("no 'main' webview window found".into())
  }
}


// ========== commands ==========

#[tauri::command]
pub fn update_tab_memory(app: tauri::AppHandle, args: UpdateTabMemoryArgs) -> Result<(), String> {
    let mut mem = load_tab(&app, args.tab_id)?;
    mem.summary = args.summary;
    mem.micro_summary = args.micro_summary;
    mem.dialogue_bullets = args.dialogue_bullets;
    let ts = now_ts();
    mem.messages.push(ChatMessage { role: "llm".into(), text: args.new_message, time: ts });
    let mut arts = args.artifacts;
    for a in arts.iter_mut() { a.ts = ts; }
    mem.artifacts.extend(arts);
    mem.last_updated = ts;
    save_tab(&app, &mem)
}

#[tauri::command]
pub fn create_tab_memory(app: tauri::AppHandle, tab_id: u32) -> Result<(), String> {
  let mem = TabMemory { tab_id, is_closed: false, last_updated: now_ts(), ..Default::default() };
  save_tab(&app, &mem)
}

#[tauri::command]
pub fn get_compiled_context(
  app: tauri::AppHandle,
  tab_id: u32,
  token_budget: usize,
) -> Result<CompiledContext, String> {
  let mem = load_tab(&app, tab_id)?;
  let compiled = compile_context_from_mem(&mem, token_budget);
  println!("[DEBUG get_compiled_context] tab_id={}, messages count={}, recent_transcript:\n{}\n", tab_id, mem.messages.len(), compiled.recent_transcript);
  Ok(compiled)
}

#[tauri::command]
pub fn interpret_turn_v2(app: tauri::AppHandle, args: InterpretTurnArgs) -> Result<InterpretPlanV2, String> {
  let flags = read_setup_flags(&app);
  if !flags.interpret_v2_enabled {
    return Err("interpret_v2 is disabled by setup flags".to_string());
  }

  let mem = load_tab(&app, args.tab_id)?;
  let compiled = compile_context_from_mem(&mem, args.token_budget.unwrap_or(1200));
  let lanes = build_memory_lanes(&mem, &compiled);
  let scores = score_intents(&args.user_text, &mem, &lanes);
  let (primary_intent, secondary_intent) = top_two_intents(&scores);
  let lane_weights = compute_lane_weights(&primary_intent);
  let pressure = pressure_score(&mem, &lanes);

  let deterministic_recall = try_resolve_recall_reply(&args.user_text, &mem.messages, &compiled.recent_transcript);
  let deterministic_math = try_resolve_math_reply(&args.user_text, &compiled.recent_transcript);
  let suggested_godot_version = detect_godot_version_hint(&args.user_text, &compiled.recent_transcript);
  let (deterministic_reply, resolver_used) = if let Some(r) = deterministic_recall {
    (r, "recall".to_string())
  } else if let Some(m) = deterministic_math {
    (m, "math".to_string())
  } else {
    (String::new(), "none".to_string())
  };

  let has_active_artifact = !mem.artifacts.is_empty();
  let use_coder_force = args.use_coder.unwrap_or(false);
  let coder_enabled = args.coder_enabled.unwrap_or(true);
  let vision_enabled = args.vision_enabled.unwrap_or(true);
  let summarizer_enabled = args.summarizer_enabled.unwrap_or(true);
  let custom_enabled_models = args.custom_enabled_models.clone().unwrap_or_default();
  let should_use_coder = coder_enabled && (use_coder_force
    || is_coder_intent_text(&args.user_text)
    || (has_active_artifact && is_edit_followup_text(&args.user_text)));
  let model = if should_use_coder { "iris-coder:latest".to_string() } else { "iris-organizer:latest".to_string() };
  let mut strategy = resolve_primary_strategy(&primary_intent, &deterministic_reply);

  let lower_text = args.user_text.to_lowercase();
  let needs_vision = vision_enabled && contains_any(&lower_text, &[
    "image", "screenshot", "ui", "vision", "photo", "picture", "diagram", "look at this"
  ]);

  // Planner route always starts with organizer; middle stages are capability-driven.
  let mut routed_models: Vec<String> = vec!["iris-organizer".to_string()];
  if should_use_coder {
    routed_models.push("iris-coder".to_string());
  }
  if needs_vision {
    routed_models.push("iris-vision".to_string());
  }
  for custom in custom_enabled_models {
    let trimmed = custom.trim();
    if trimmed.is_empty() { continue; }
    if !routed_models.iter().any(|m| m == trimmed) {
      routed_models.push(trimmed.to_string());
    }
  }
  if summarizer_enabled {
    routed_models.push("iris-summarizer".to_string());
  }
  let route_summary = format!("Planner route: {}", routed_models.join(" -> "));
  let status_hint = if should_use_coder {
    "coding".to_string()
  } else if needs_vision {
    "vision".to_string()
  } else {
    "thinking".to_string()
  };

  let last_number_hint = extract_last_assistant_number(&compiled.recent_transcript)
    .map(|n| format!("Resolved reference hint: most recent numeric value in conversation memory is {}.", format_num(n)))
    .unwrap_or_else(String::new);

  let has_project_context = has_concrete_project_context(&mem, &lanes);
  let bridge_note = if should_emit_bridge(&primary_intent, pressure, &mem) && has_project_context {
    "Playful bridge-back: Keep the tone warm, then nudge the user toward the active project goals in one concise sentence. Use only project details present in memory lanes/artifacts; do not infer missing specifics.".to_string()
  } else if should_emit_bridge(&primary_intent, pressure, &mem) {
    "Playful bridge-back: Keep the tone warm and add at most one gentle, non-assumptive question about what they want to build next. Do not name features, files, or implementation details unless they already exist in memory.".to_string()
  } else {
    String::new()
  };

  let followup_hint = if contains_any(&args.user_text.to_lowercase(), &["tell me more", "more details", "go deeper", "elaborate"]) {
    "Follow-up grounding: unless user names a new topic, expand the immediately previous assistant answer rather than jumping to an older topic.".to_string()
  } else {
    String::new()
  };

  let alcohol_age_hint = if contains_any(&args.user_text.to_lowercase(), &["alcohol", "drinking", "drink"]) && contains_any(&args.user_text.to_lowercase(), &["age", "adult", "21", "that age"]) {
    "If user asks what changes at a specific age concerning alcohol, answer the main legal/regulatory point first, then add brief regional caveat if needed.".to_string()
  } else {
    String::new()
  };

  let persona = load_persona_prompt();
  let assistant_name = args.assistant_name.as_deref()
    .map(|v| v.trim())
    .filter(|v| !v.is_empty())
    .unwrap_or(flags.assistant_name.as_str());
  let model_profile = args.model_profile.as_deref()
    .map(|v| v.trim())
    .filter(|v| !v.is_empty())
    .unwrap_or(flags.model_profile.as_str());
  let model_profile_lc = model_profile.to_ascii_lowercase();
  let constrained_profile = model_profile_lc == "low" || model_profile_lc == "minimal";
  let network_enabled = args.network_enabled.unwrap_or(flags.network_enabled);
  let mut repos_enabled = args.repos_enabled.unwrap_or(flags.repos_enabled);
  let mut mcp_enabled = args.mcp_enabled.unwrap_or(flags.mcp_enabled);
  if constrained_profile {
    repos_enabled = false;
    mcp_enabled = false;
    strategy = "latency_guard".to_string();
  }
  let desktop_tools_enabled = args.desktop_tools_enabled.unwrap_or(flags.desktop_tools_enabled);
  let selected_project_name = args.selected_project_name.as_deref()
    .map(str::trim)
    .filter(|v| !v.is_empty())
    .unwrap_or("(none)");
  let project_context = args.project_context.as_deref().unwrap_or("").trim();
  let project_dataweb = args.project_dataweb.as_deref().unwrap_or("").trim();
  let universal_dataweb = args.universal_dataweb.as_deref().unwrap_or("").trim();
  let allow_long_term_dataweb = explicitly_requests_long_term_memory(&args.user_text)
    || transcript_insufficient_for_long_term(&compiled.recent_transcript, &args.user_text);
  let system_state_block = format!(
    "System state:\n- Assistant name: {}\n- Model profile: {}\n- Network: {}\n- Repos context: {}\n- MCP context: {}\n- Desktop tools: {}\n- Selected project: {}",
    assistant_name,
    model_profile,
    if network_enabled { "ON" } else { "OFF" },
    if repos_enabled { "ON" } else { "OFF" },
    if mcp_enabled { "ON" } else { "OFF" },
    if desktop_tools_enabled { "ON" } else { "OFF" },
    selected_project_name,
  );
  let mut injected_context_parts: Vec<String> = Vec::new();
  if !project_context.is_empty() {
    injected_context_parts.push(format!("Project context:\n{}", project_context));
  }
  if allow_long_term_dataweb && !project_dataweb.is_empty() {
    injected_context_parts.push(format!("Project dataweb memory:\n{}", project_dataweb));
  }
  if allow_long_term_dataweb && !universal_dataweb.is_empty() {
    injected_context_parts.push(format!("Universal dataweb memory:\n{}", universal_dataweb));
  }
  let injected_context_block = if injected_context_parts.is_empty() {
    "Injected external context:\n(none)".to_string()
  } else {
    format!("Injected external context:\n{}", injected_context_parts.join("\n\n"))
  };
  let lane_block = format!(
    "Memory lanes (soft-weighted):\n- Project ({:.2}): {}\n- Coding ({:.2}): {}\n- Recall ({:.2}): {}",
    lane_weights.project,
    lanes.project,
    lane_weights.coding,
    lanes.coding,
    lane_weights.recall,
    lanes.recall
  );

  // --- routine intent detection ---
  let is_observe_routine = vision_enabled && contains_any(&lower_text, &[
    "look at", "screenshot", "what's happening", "what is happening", "what's going on",
    "what is going on", "what's wrong with", "what is wrong with", "investigate",
    "check my godot", "analyze my", "look at my", "what do you see", "scan my screen",
    "capture screen", "take a screenshot", "look at the screen", "see my screen",
    "look at what", "see what", "what can you see", "show me what"
  ]);
  let is_action_routine = is_observe_routine && contains_any(&lower_text, &[
    "make a change", "fix this", "apply", "create in godot", "add to godot",
    "change in godot", "modify godot", "update godot", "write to", "implement in"
  ]);
  let routine_plan: Option<RoutinePlan> = if is_observe_routine {
    let ts = now_ts();
    let mut steps = vec![
      RoutineStep {
        id: format!("rs_{}_1", ts),
        step_type: "screenshot".to_string(),
        label: "Capture screen".to_string(),
        params: Default::default(),
      },
      RoutineStep {
        id: format!("rs_{}_2", ts),
        step_type: "vision".to_string(),
        label: "Analyze screenshot with Vision".to_string(),
        params: Default::default(),
      },
    ];
    if is_action_routine {
      steps.push(RoutineStep {
        id: format!("rs_{}_3", ts),
        step_type: "coder".to_string(),
        label: "Generate code change".to_string(),
        params: Default::default(),
      });
    }
    steps.push(RoutineStep {
      id: format!("rs_{}_4", ts),
      step_type: "llm_reply".to_string(),
      label: "Compile and respond".to_string(),
      params: Default::default(),
    });
    Some(RoutinePlan {
      id: format!("routine_{}", ts),
      goal: args.user_text.clone(),
      steps,
      is_long_running: is_action_routine,
    })
  } else if contains_any(&lower_text, &[
    "keep going", "repeat", "loop", "continuously", "until done", "until complete",
    "iterate", "step by step", "go through each", "process all", "one by one",
    "do each", "fix all", "go through all", "complete all",
  ]) {
    // User wants a multi-step iterative routine defined by the LLM.
    // Generate a plan_routine + iterate + verify + complete cycle.
    let ts = now_ts();
    let steps = vec![
      RoutineStep {
        id: format!("rs_{}_1", ts),
        step_type: "plan".to_string(),
        label: "Plan steps".to_string(),
        params: Default::default(),
      },
      RoutineStep {
        id: format!("rs_{}_2", ts),
        step_type: "iterate".to_string(),
        label: "Execute steps".to_string(),
        params: Default::default(),
      },
      RoutineStep {
        id: format!("rs_{}_3", ts),
        step_type: "verify".to_string(),
        label: "Self-check results".to_string(),
        params: Default::default(),
      },
      RoutineStep {
        id: format!("rs_{}_4", ts),
        step_type: "llm_reply".to_string(),
        label: "Summarize outcome".to_string(),
        params: Default::default(),
      },
    ];
    Some(RoutinePlan {
      id: format!("routine_{}", ts),
      goal: args.user_text.clone(),
      steps,
      is_long_running: true,
    })
  } else {
    None
  };

  let prompt = if should_use_coder {
    let active_art = mem.artifacts.iter().max_by_key(|a| a.ts)
      .map(|a| format!("```{}\n{}\n```", a.lang, a.content))
      .unwrap_or_else(|| "(none)".to_string());
    let godot_hint = match suggested_godot_version.as_str() {
      "godot4" => "Godot target: prefer Godot 4 APIs and syntax (Node3D/CharacterBody, await, @export/@onready).",
      "godot3" => "Godot target: prefer Godot 3 APIs and syntax (Spatial/KinematicBody, yield(), export var/onready var).",
      _ => "Godot target: unspecified. If API version matters, ask one concise clarifying question before producing version-specific code.",
    };
    format!(
      "{}\n\nIdentity:\nYou are {}.\n\n{}\n\nConversation memory:\n{}\n\n{}\n\n{}\n\nActive artifact:\n{}\n\nUser request:\n{}\n\n{}\n{}\nExecution contract:\n- Decode user intent(s) first.\n- Build an internal step checklist before writing code.\n- Execute steps in a deliberate order; do not rush through all context at once.\n- If a step fails or evidence conflicts, backtrack and adjust plan.\n- Prefer grounded edits tied to available memory/context.\nRules:\n- Return a single fenced code block.\n- If editing prior code, modify the active artifact unless user asks for a rewrite.",
      persona,
      assistant_name,
      system_state_block,
      if compiled.recent_transcript.is_empty() { "(empty)" } else { &compiled.recent_transcript },
      lane_block,
      injected_context_block,
      active_art,
      args.user_text,
      godot_hint,
      bridge_note
    )
  } else {
    let gk_hint = if primary_intent == "general_knowledge" {
      "General knowledge fallback: answer normally from world knowledge when conversation memory does not contain the topic."
    } else { "" };
    let math_realism_hint = if primary_intent == "math_units" {
      "Math/units realism: separate energy (Wh/kWh) from power (W/kW), compute with realistic assumptions, and keep the explanation concise."
    } else { "" };
    let clarify_hint = if primary_intent == "clarification_repair" {
      "The user signaled confusion or correction. Re-interpret from scratch and ask one concise clarifying question if still ambiguous."
    } else { "" };
    format!(
      "{}\n\nIdentity:\nYou are {}.\n\n{}\n\nConversation memory:\n{}\n\n{}\n\n{}\n\n{}\n\nUser request:\n{}\n\n{}{}\n{}\n{}\n{}\n{}\n{}\nExecution contract:\n- Decode user intent(s) first (there may be multiple).\n- Build an internal task checklist and process it step-by-step.\n- Prioritize the most relevant evidence and avoid scanning context recklessly.\n- If a path looks wrong, backtrack and try a better sequence.\n- Perform a brief self-check before finalizing.\nOutput style:\n- Natural, direct prose by default.\n- Do not output scaffolding labels like TODO, PLAN, User request, Assumptions, or Sanity check unless explicitly requested.\n- Never invent project facts, files, features, previous decisions, or completed work. If unknown, say you do not have that project detail yet and ask one short clarifying question.\n- Keep concise unless user asks for depth.",
      persona,
      assistant_name,
      system_state_block,
      if compiled.recent_transcript.is_empty() { "(empty)" } else { &compiled.recent_transcript },
      lane_block,
      last_number_hint,
      injected_context_block,
      args.user_text,
      {
        let note = args.organizer_dispatch_note.as_deref().unwrap_or("").trim();
        if note.is_empty() { String::new() } else { format!("\n\nModel dispatch instructions:\n{}\n", note) }
      },
      gk_hint,
      math_realism_hint,
      clarify_hint,
      followup_hint,
      alcohol_age_hint,
      bridge_note
    )
  };

  Ok(InterpretPlanV2 {
    primary_intent,
    secondary_intent,
    strategy,
    should_use_coder,
    model,
    pressure_score: pressure,
    lane_weights,
    intent_scores: scores,
    memory_lanes: lanes,
    compiled_context: compiled,
    prompt,
    deterministic_reply,
    resolver_used,
    bridge_note,
    suggested_godot_version,
    routed_models,
    route_summary,
    status_hint,
    routine_plan,
  })
}

#[tauri::command]
pub fn close_tab_and_snapshot(app: tauri::AppHandle, tab_id: u32, mut snapshot: Snapshot) -> Result<(), String> {
  let now = now_ts();
  if snapshot.last_updated.unwrap_or(0) == 0 {
    snapshot.last_updated = Some(now);
  }
  if snapshot.tab_id.is_none() {
    snapshot.tab_id = Some(tab_id);
  }
  for m in &mut snapshot.messages {
    if m.time == 0 {
      m.time = now;
    }
  }

  let mut stack = read_last_closed_stack(&app)?;
  stack.push_front(snapshot);
  while stack.len() > 3 {
    stack.pop_back();
  }
  write_last_closed_stack(&app, &stack)?;

  if let Ok(dir) = iris_open_tabs_dir(&app) {
    let p = dir.join(format!("tab_{}.json", tab_id));
    let _ = remove_file(&p);
  }

  Ok(())
}

#[tauri::command]
// AFTER (normalize timestamps and return Snapshot without tab_id)
pub fn restore_last_closed_tab(app: tauri::AppHandle) -> Result<Snapshot, String> {
  let mut stack = read_last_closed_stack(&app)?;
  let mut snap = stack.pop_front().ok_or_else(|| "no recently closed tab snapshot".to_string())?;

  let now = now_ts();
  if snap.last_updated.unwrap_or(0) == 0 {
    snap.last_updated = Some(now);
  }
  for m in &mut snap.messages {
    if m.time == 0 {
      m.time = now;
    }
  }

  write_last_closed_stack(&app, &stack)?;
  Ok(snap)
}

#[tauri::command]
pub fn clear_last_closed_tab(app: tauri::AppHandle) -> Result<(), String> {
  let stack_file = iris_last_closed_stack_file(&app)?;
  if stack_file.exists() {
    remove_file(&stack_file).map_err(|e| e.to_string())?;
  }
  let lf = iris_last_closed_file(&app)?;
  if lf.exists() {
    remove_file(&lf).map_err(|e| e.to_string())?;
  }
  Ok(())
}

#[tauri::command]
pub async fn ensure_coder_lite_model() -> Result<String, String> {
    ensure_ollama_running_once();

    let client: Client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    // Wait briefly for the server to come up
    let tags_url = format!("{}/api/tags", OLLAMA_BASE);
    for _ in 0..6 {
        if client.get(&tags_url).send().is_ok() { break; }
        std::thread::sleep(Duration::from_millis(500));
    }

    // Check installed models
    let resp = client.get(&tags_url).send().map_err(|e| e.to_string())?;
    let tags: Value = resp.json().map_err(|e| e.to_string())?;

    let mut found = false;
    if let Some(arr) = tags["models"].as_array() {
        for m in arr {
            if m["name"] == MODEL_TAG { found = true; break; }
        }
    }

    // Custom tags can't be pulled via /api/pull; they must exist locally.
    if !found {
        return Err(format!(
            "Model '{}' not found. Build it first:\n  ollama create {} -f .\\model_files\\modelfile_organizer.txt",
            MODEL_TAG, MODEL_TAG
        ));
    }

    // Warm up (load weights into cache)
    let gen_url = format!("{}/api/generate", OLLAMA_BASE);
    let warm = client
        .post(&gen_url)
        .json(&serde_json::json!({
            "model": MODEL_TAG,
            "prompt": " ",
            "stream": false,
            "options": { "num_predict": 1 },
            "keep_alive": "90s"   // â† unload after 90s of inactivity
        }))
        .send();

    match warm {
        Ok(_) => Ok("Model ready (warmed)".to_string()),
        Err(e) => Ok(format!("Model ready (warmup skipped: {})", e)),
    }
}

pub fn ensure_ollama_running_once() {
    if OLLAMA_START_ATTEMPTED.swap(true, Ordering::SeqCst) { return; }
    thread::spawn(|| {
        #[cfg(target_os = "windows")]
        let check = Command::new("tasklist")
            .arg("/FI").arg("IMAGENAME eq ollama.exe")
            .output();
        #[cfg(not(target_os = "windows"))]
        let check = Command::new("pgrep").arg("ollama").output();

        if let Ok(output) = check {
            let already = if cfg!(target_os = "windows") {
                String::from_utf8_lossy(&output.stdout).contains("ollama.exe")
            } else {
                !output.stdout.is_empty()
            };
            if !already {
              let ollama_bin = resolve_ollama_executable();
              let _ = Command::new(ollama_bin)
                    .arg("serve")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            }
        }
    });
}

#[tauri::command]
pub async fn get_universal_prompts() -> Result<Vec<String>, String> {
    Ok(vec![
        "Summarize this file".to_string(),
        "Write a test for this function".to_string(),
        "Explain this code".to_string(),
    ])
}

#[tauri::command]
pub async fn test_model(model: String, prompt: String) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .post(&format!("{}/api/generate", OLLAMA_BASE))
        .json(&serde_json::json!({
            "model": model,
            "prompt": prompt,
            "stream": false,
            "keep_alive": "30s"
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "ok": true,
        "error": null,
        "response": json.get("response").cloned()
    }))
}

#[tauri::command]
pub fn get_tab_context(
    app: tauri::AppHandle,
    tab_id: u32,
) -> Result<(String, String, Vec<String>), String> {
    let mem = load_tab(&app, tab_id)?;
    let summary = mem.summary.clone();
    let micro = mem.micro_summary.clone();
    let transcript: Vec<String> = mem.messages.iter().map(|m| {
        format!("{}: {}", if m.role == "user" { "User" } else { "Iris" }, m.text)
    }).collect();
    Ok((summary, micro, transcript))
}

#[tauri::command]
pub fn close_window(window: tauri::Window) {
    let _ = window.close();
}

#[tauri::command]
#[allow(dead_code)]
pub fn migrate_open_tabs_to_snapshot_format(app: tauri::AppHandle) -> Result<usize, String> {
  let dir = iris_open_tabs_dir(&app)?;
  let mut count = 0;

  if let Ok(rd) = read_dir(&dir) {
    for e in rd.flatten() {
      let path = e.path();
      if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
      if path.to_string_lossy().contains(".tmp") { continue; }

      if migrate_legacy_tabmemory_to_snapshot(&path).is_ok() {
        count += 1;
      }
    }
  }

  eprintln!("[migrate_open_tabs_to_snapshot_format] Migrated {} files", count);
  Ok(count)
}

#[tauri::command]
pub async fn list_open_tabs(app: tauri::AppHandle) -> Result<Vec<Snapshot>, String> {
    let dir = iris_open_tabs_dir(&app)?;
  eprintln!("[list_open_tabs] Reading from directory: {}", dir.display());
    let mut out: Vec<Snapshot> = Vec::new();

    if let Ok(rd) = read_dir(&dir) {
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
            // skip tmp-like files
            if path.to_string_lossy().contains(".tmp") { continue; }

            match iris_read_snapshot_file(&path) {
                Ok(mut snap) => {
                    // normalize messages if needed
                    let (norm, changed) = normalize_messages(&snap.messages);
                    if changed {
                        snap.messages = norm.clone();
                        let _ = iris_write_snapshot_file(&path, &snap);
                    }
                    out.push(snap);
                }
                Err(err) => {
                    eprintln!("[list_open_tabs] skipping corrupt {}: {}", path.display(), err);
                    continue;
                }
            }
        }
    }

      eprintln!("[list_open_tabs] Found {} snapshots", out.len());
    // If empty during dev, seed from ./iris_memory (one-shot)
    if out.is_empty() && cfg!(debug_assertions) {
        let copied = seed_open_tabs_from_dev_dir(app.clone());
        if copied > 0 {
            if let Ok(rd) = read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("json") {
                        if let Ok(snap) = iris_read_snapshot_file(&p) {
                            out.push(snap);
                        }
                    }
                }
            }
        }
    }

    out.sort_by(|a, b| b.last_updated.unwrap_or(0).cmp(&a.last_updated.unwrap_or(0)));
    Ok(out)
}

#[tauri::command]
pub async fn ensure_model(name: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
    ensure_ollama_running_once();

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;

    // Wait briefly for Ollama to become reachable.
    for _ in 0..12 {
      if client.get("http://127.0.0.1:11434/api/tags").send().is_ok() {
        break;
      }
      std::thread::sleep(Duration::from_millis(500));
    }

        // check if model exists
        let show = client
            .post("http://127.0.0.1:11434/api/show")
            .json(&serde_json::json!({ "name": name }))
            .send();

        let need_pull = match show {
            Ok(resp) => resp.status() == reqwest::StatusCode::NOT_FOUND,
            Err(_) => {
                // if Ollama is down, surface that
                return Err("Cannot reach Ollama at 127.0.0.1:11434".into());
            }
        };

        if need_pull {
          // Custom Iris tags must be created from modelfiles, not pulled directly.
          let definitions = model_definitions(None);
          if let Some((custom_name, base_model, modelfile_file)) =
            definitions.iter().find(|(tag, _, _)| *tag == name)
          {
            let mut pulled = false;
            let mut pulled_base = base_model.clone();
            let mut pull_errors: Vec<String> = Vec::new();
            for candidate in pull_candidates(base_model) {
              let pull = client
                .post("http://127.0.0.1:11434/api/pull")
                .json(&serde_json::json!({ "name": candidate, "stream": false }))
                .send()
                .map_err(|e| e.to_string())?;
              if pull.status().is_success() {
                pulled = true;
                pulled_base = candidate;
                break;
              }
              let status = pull.status();
              let body = pull.text().unwrap_or_else(|_| "<no body>".to_string());
              pull_errors.push(format!("{} -> {} {}", base_model, status, body));
            }
            if !pulled {
              return Err(format!("Base model pull failed: {}", pull_errors.join(" | ")));
            }

            let mf_path = modelfile_path(None, modelfile_file)?;
            let tmp_mf = make_temp_modelfile_with_from(&mf_path, &pulled_base)?;

            let out = Command::new(resolve_ollama_executable())
              .arg("create")
              .arg(custom_name)
              .arg("-f")
              .arg(&tmp_mf)
              .output()
              .map_err(|e| format!("Failed to run ollama create: {}", e))?;
            let _ = fs::remove_file(&tmp_mf);
            if !out.status.success() {
              let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
              let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
              return Err(format!(
                "Model create failed ({}): exit {} | stdout: {} | stderr: {}",
                custom_name,
                out.status,
                stdout,
                stderr
              ));
            }
          } else {
            // Non-custom models can be pulled directly.
            let pull = client
              .post("http://127.0.0.1:11434/api/pull")
              .json(&serde_json::json!({ "name": name, "stream": false }))
              .send()
              .map_err(|e| e.to_string())?;

            if !pull.status().is_success() {
              return Err(format!("Model pull failed: {}", pull.status()));
            }
            }
        }
        Ok(())
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn restore_full_tab_memory(app: tauri::AppHandle, args: RestoreFullTabMemoryArgs) -> Result<(), String> {
  // load/normalize/save as you already do, but only via args.*
  let mem = TabMemory {
    tab_id: args.tab_id,
    title: args.title,
    messages: args.messages,
    artifacts: args.artifacts,
    micro_summary: args.micro_summary,
    dialogue_bullets: args.dialogue_bullets,
    summary: args.summary,
    is_closed: false,
    last_updated: args.last_updated.unwrap_or_else(now_ts),
  };
  save_tab(&app, &mem)
}

fn is_standard_role(s: &str) -> bool {
  matches!(s, "user" | "llm")
}

fn split_legacy_block(txt: &str) -> Option<(String, String)> {
  let lower = txt.to_lowercase();
  if let Some(i) = lower.find("user:") {
    if let Some(j) = lower[i..].find("\niris:") {
      let u = txt[i + 5 .. i + j].trim().to_string();
      let a = txt[i + j + 7 ..].trim().to_string();
      return Some((u, a));
    }
  }
  None
}

fn normalize_messages(msgs: &[ChatMessage]) -> (Vec<ChatMessage>, bool) {
  let mut changed = false;
  let mut out: Vec<ChatMessage> = Vec::new();
  for m in msgs.iter() {
    let role_l = m.role.to_lowercase();
    if is_standard_role(&role_l) {
      out.push(ChatMessage { role: role_l, text: m.text.clone(), time: now_ts() });
    } else if let Some((u, a)) = split_legacy_block(&m.text) {
      if !u.is_empty() { out.push(ChatMessage { role: "user".into(), text: u, time: now_ts() }); }
      if !a.is_empty() { out.push(ChatMessage { role: "llm".into(), text: a, time: now_ts() }); }
      changed = true;
    } else {
      out.push(ChatMessage { role: "llm".into(), text: m.text.clone(), time: now_ts() });
      if role_l != "llm" { changed = true; }
    }
  }
  (out, changed)
}

// new code starts here
#[tauri::command]
#[allow(dead_code)]
pub fn save_snapshot(path: &str, snap: &Snapshot) -> Result<(), String> {
  let s = serde_json::to_string_pretty(snap).map_err(|e| e.to_string())?;
  atomic_write_json(&PathBuf::from(path), &s)
}

#[tauri::command]
#[allow(dead_code)]
pub fn load_snapshot(path: &str) -> Result<Snapshot, String> {
  let s = fs::read_to_string(path).map_err(|e| e.to_string())?;
  let mut snap: Snapshot = serde_json::from_str(&s).map_err(|e| e.to_string())?;
  snap.messages.retain(|m| m.role != "system");
  Ok(snap)
}

#[tauri::command]
pub async fn update_snapshot_memory(
  app: tauri::AppHandle,
  args: UpdateSnapshotMemoryArgs,
) -> Result<(), String> {
  tauri::async_runtime::spawn_blocking(move || {
    let mut snapshot = args.snapshot;
    snapshot.tab_id = Some(args.tab_id);
    bound_snapshot_payload(&mut snapshot);
    let now = now_ts();
    if snapshot.last_updated.unwrap_or(0) == 0 {
      snapshot.last_updated = Some(now);
    }
    for m in &mut snapshot.messages {
      if m.time == 0 {
        m.time = now;
      }
    }

    let dir = iris_open_tabs_dir(&app)?;
    let path = dir.join(format!("tab_{}.json", args.tab_id));
    iris_write_snapshot_file(&path, &snapshot)
  })
  .await
  .map_err(|e| format!("snapshot task join error: {}", e))?
}

#[tauri::command]
pub fn read_tab_snapshot(app: tauri::AppHandle, args: ReadTabSnapshotArgs) -> Result<Snapshot, String> {
  let path = iris_normalize_key_to_path(&app, &args.key)?;
  if !path.exists() {
    return Err(format!("snapshot not found: {}", path.display()));
  }

  match iris_read_snapshot_file(&path) {
    Ok(snap) => Ok(snap),
    Err(e) => Err(e),
  }
}
// new code ends here

#[tauri::command]
pub fn debug_memory_dir(app: tauri::AppHandle) -> String {
  match memory_dir(&app) {
    Ok(p) => p.display().to_string(),
    Err(e) => e,
  }
}

// APPEND: new iris_* helpers, debug_list_open_tab_files, and seed_open_tabs_from_dev_dir
use std::fs::{read, write, read_dir, create_dir_all, remove_file, copy};
use serde_json::{from_slice, to_vec_pretty};

fn iris_open_tabs_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  let mut p = memory_dir(app)?;
  p.push("open_tabs");
  create_dir_all(&p).map_err(|e| e.to_string())?;
  Ok(p)
}

fn iris_last_closed_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  let mut p = memory_dir(app)?;
  p.push("last_closed.json");
  Ok(p)
}

fn iris_last_closed_stack_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  let mut p = memory_dir(app)?;
  p.push("last_closed_stack.json");
  Ok(p)
}

fn tab_memory_to_snapshot(mem: TabMemory) -> Snapshot {
  Snapshot {
    tab_id: Some(mem.tab_id),
    title: mem.title,
    messages: mem.messages,
    associated_project_id: None,
    micro_summary: mem.micro_summary,
    dialogue_bullets: mem.dialogue_bullets,
    summary: mem.summary,
    artifacts: mem.artifacts,
    prompt_history: Vec::new(),
    last_updated: Some(mem.last_updated),
  }
}

fn read_last_closed_stack(app: &tauri::AppHandle) -> Result<VecDeque<Snapshot>, String> {
  let stack_file = iris_last_closed_stack_file(app)?;
  if stack_file.exists() {
    let raw = fs::read_to_string(&stack_file).map_err(|e| e.to_string())?;
    if let Ok(v) = serde_json::from_str::<Vec<Snapshot>>(&raw) {
      return Ok(v.into_iter().collect());
    }
    if let Ok(vd) = serde_json::from_str::<VecDeque<Snapshot>>(&raw) {
      return Ok(vd);
    }
  }

  // Legacy fallback: migrate from single last_closed.json if present.
  let legacy_file = iris_last_closed_file(app)?;
  if legacy_file.exists() {
    let raw = fs::read_to_string(&legacy_file).map_err(|e| e.to_string())?;
    if let Ok(snap) = serde_json::from_str::<Snapshot>(&raw) {
      return Ok(VecDeque::from(vec![snap]));
    }
    if let Ok(mem) = serde_json::from_str::<TabMemory>(&raw) {
      return Ok(VecDeque::from(vec![tab_memory_to_snapshot(mem)]));
    }
  }

  Ok(VecDeque::new())
}

fn write_last_closed_stack(app: &tauri::AppHandle, stack: &VecDeque<Snapshot>) -> Result<(), String> {
  let stack_file = iris_last_closed_stack_file(app)?;
  let as_vec: Vec<Snapshot> = stack.iter().cloned().collect();
  let json = serde_json::to_string_pretty(&as_vec).map_err(|e| e.to_string())?;
  atomic_write_json(&stack_file, &json)
}

fn iris_normalize_key_to_path(app: &tauri::AppHandle, key: &str) -> Result<PathBuf, String> {
  let dir = iris_open_tabs_dir(app)?;
  let k = key.trim();
  let fname = if k.ends_with(".json") {
    k.to_string()
  } else if k.starts_with("tab_") {
    if k.ends_with(".json") { k.to_string() } else { format!("{k}.json") }
  } else {
    format!("tab_{k}.json")
  };
  Ok(dir.join(fname))
}

/// Migration helper: converts legacy TabMemory to Snapshot format and rewrites the file.
#[allow(dead_code)]
fn migrate_legacy_tabmemory_to_snapshot(path: &std::path::Path) -> Result<Snapshot, String> {
  let bytes = read(path).map_err(|e| e.to_string())?;

  // First, try parsing as Snapshot (already-migrated)
  if let Ok(snap) = from_slice::<Snapshot>(&bytes) {
    return Ok(snap);
  }

  // Fallback: parse as legacy TabMemory
  let mem: TabMemory = from_slice(&bytes).map_err(|e| e.to_string())?;
  let now = now_ts();
  let mut snap = Snapshot {
    tab_id: Some(mem.tab_id),
    title: mem.title.clone(),
    messages: mem.messages.clone(),
    associated_project_id: None,
    micro_summary: mem.micro_summary.clone(),
    dialogue_bullets: mem.dialogue_bullets.clone(),
    summary: mem.summary.clone(),
    artifacts: mem.artifacts.clone(),
    prompt_history: Vec::new(),
    last_updated: Some(if mem.last_updated > 0 { mem.last_updated } else { now }),
  };
  for m in &mut snap.messages { if m.time == 0 { m.time = now; } }
  snap.messages.retain(|m| m.role != "system");

  // Rewrite the file in the new format
  let _ = iris_write_snapshot_file(path, &snap);
  eprintln!("[migrate_legacy_tabmemory_to_snapshot] Migrated {} to Snapshot format", path.display());

  Ok(snap)
}

fn iris_read_snapshot_file(path: &std::path::Path) -> Result<Snapshot, String> {
  let bytes = read(path).map_err(|e| e.to_string())?;

  // First, try parsing as the newer Snapshot format
  let sres: Result<Snapshot, serde_json::Error> = from_slice(&bytes);
  if let Ok(mut snap) = sres {
    snap.messages.retain(|m| m.role != "system");
    return Ok(snap);
  }

  // Fallback: older TabMemory format â€” convert into Snapshot (and rewrite file)
  let mres: Result<TabMemory, serde_json::Error> = from_slice(&bytes);
  if let Ok(mem) = mres {
    let now = now_ts();
    let mut snap = Snapshot {
      tab_id: Some(mem.tab_id),
      title: mem.title.clone(),
      messages: mem.messages.clone(),
      associated_project_id: None,
      micro_summary: mem.micro_summary.clone(),
      dialogue_bullets: mem.dialogue_bullets.clone(),
      summary: mem.summary.clone(),
      artifacts: mem.artifacts.clone(),
      prompt_history: Vec::new(),
      last_updated: Some(mem.last_updated),
    };
    for m in &mut snap.messages { if m.time == 0 { m.time = now; } }
    snap.messages.retain(|m| m.role != "system");
    // Rewrite in new format for next time
    let _ = iris_write_snapshot_file(path, &snap);
    return Ok(snap);
  }

  // Neither format parsed â€” return combined error messages for debugging
  let s_err = sres.err().map(|e| e.to_string()).unwrap_or_else(|| "no snapshot error".into());
  let m_err = mres.err().map(|e| e.to_string()).unwrap_or_else(|| "no tabmem error".into());
  Err(format!("failed to parse snapshot: snapshot_err: {}; tabmem_err: {}", s_err, m_err))
}

fn iris_write_snapshot_file(path: &std::path::Path, snap: &Snapshot) -> Result<(), String> {
  let bytes = to_vec_pretty(snap).map_err(|e| e.to_string())?;
  let result = write(path, bytes).map_err(|e| e.to_string());
  if result.is_ok() {
    eprintln!("[iris_write_snapshot_file] Successfully wrote {} bytes to: {}", snap.messages.len(), path.display());
  }
  result
}

#[tauri::command]
pub fn debug_list_open_tab_files(app: tauri::AppHandle) -> Vec<String> {
  let mut names = Vec::new();
  if let Ok(dir) = iris_open_tabs_dir(&app) {
    if let Ok(rd) = read_dir(&dir) {
      for e in rd.flatten() {
        if let Some(n) = e.file_name().to_str() {
          names.push(n.to_string());
        }
      }
    }
  }
  names
}

/// Development helper: copy ./iris_memory/tab_*.json into open_tabs if present (one-shot)
#[tauri::command]
pub fn seed_open_tabs_from_dev_dir(app: tauri::AppHandle) -> usize {
  if !cfg!(debug_assertions) { return 0; }
//   let base = match memory_dir(&app) { Ok(p) => p, Err(_) => return 0 };
  let dev = PathBuf::from("./iris_memory");
  if !dev.exists() { return 0; }
  let open = match iris_open_tabs_dir(&app) { Ok(p) => p, Err(_) => return 0 };
  let mut copied = 0usize;
  if let Ok(rd) = read_dir(&dev) {
    for e in rd.flatten() {
      let p = e.path();
      let name_ok = p.file_name().and_then(|s| s.to_str()).map(|n| n.starts_with("tab_") && n.ends_with(".json")).unwrap_or(false);
      if name_ok {
        if let Some(name) = p.file_name() {
          let mut dst = open.clone();
          dst.push(name);
          if copy(&p, &dst).is_ok() { copied += 1; }
        }
      }
    }
  }
  copied
}

// ========== Ollama setup commands ==========

const MODEL_FILES: &[(&str, &str)] = &[
    ("iris-organizer:latest", "modelfile_organizer.txt"),
    ("iris-coder:latest", "modelfile_coder.txt"),
    ("iris-summarizer:latest", "modelfile_summarizer.txt"),
    ("iris-vision:latest", "modelfile_vision.txt"),
];

#[derive(Clone, Copy)]
enum ModelProfile {
  Ultra,
  High,
  MediumHigh,
  Medium,
  Low,
  Minimal,
}

impl ModelProfile {
  fn as_str(&self) -> &'static str {
    match self {
      Self::Ultra => "Ultra",
      Self::High => "High",
      Self::MediumHigh => "MediumHigh",
      Self::Medium => "Medium",
      Self::Low => "Low",
      Self::Minimal => "Minimal",
    }
  }

  fn parse(s: &str) -> Self {
    match s.trim().to_ascii_lowercase().as_str() {
      "ultra" => Self::Ultra,
      "high" => Self::High,
      "mediumhigh" | "medium-high" | "medium_high" => Self::MediumHigh,
      "low" => Self::Low,
      "minimal" => Self::Minimal,
      _ => Self::Medium,
    }
  }
}

fn default_base_model_for(custom_name: &str, profile: ModelProfile) -> &'static str {
  match (custom_name, profile) {
    ("iris-organizer:latest", ModelProfile::Ultra) => "qwen2.5:14b-instruct",
    ("iris-organizer:latest", ModelProfile::High) => "qwen2.5:7b-instruct",
    ("iris-organizer:latest", ModelProfile::MediumHigh) => "qwen2.5:7b-instruct",
    ("iris-organizer:latest", ModelProfile::Medium) => "llama3.2:3b",
    ("iris-organizer:latest", ModelProfile::Low) => "llama3.2:3b",
    ("iris-organizer:latest", ModelProfile::Minimal) => "qwen2.5:1.5b-instruct",

    ("iris-coder:latest", ModelProfile::Ultra) => "qwen2.5-coder:14b-instruct",
    ("iris-coder:latest", ModelProfile::High) => "qwen2.5-coder:7b-instruct",
    ("iris-coder:latest", ModelProfile::MediumHigh) => "qwen2.5-coder:7b-instruct",
    ("iris-coder:latest", ModelProfile::Medium) => "qwen2.5-coder:7b-instruct",
    ("iris-coder:latest", ModelProfile::Low) => "qwen2.5-coder:3b-instruct",
    ("iris-coder:latest", ModelProfile::Minimal) => "qwen2.5-coder:1.5b-instruct",

    ("iris-summarizer:latest", ModelProfile::Ultra) => "gemma3:4b",
    ("iris-summarizer:latest", ModelProfile::High) => "gemma3:4b",
    ("iris-summarizer:latest", ModelProfile::MediumHigh) => "gemma3:4b",
    ("iris-summarizer:latest", ModelProfile::Medium) => "gemma3:1b",
    ("iris-summarizer:latest", ModelProfile::Low) => "gemma3:1b",
    ("iris-summarizer:latest", ModelProfile::Minimal) => "gemma3:1b",

    ("iris-vision:latest", ModelProfile::Ultra) => "qwen2-vl:7b-instruct",
    ("iris-vision:latest", ModelProfile::High) => "qwen2-vl:2b-instruct",
    ("iris-vision:latest", ModelProfile::MediumHigh) => "qwen2-vl:2b-instruct",
    ("iris-vision:latest", ModelProfile::Medium) => "qwen2-vl:2b-instruct",
    ("iris-vision:latest", ModelProfile::Low) => "qwen2-vl:2b-instruct",
    ("iris-vision:latest", ModelProfile::Minimal) => "qwen2-vl:2b-instruct",

    _ => "llama3.2:3b",
  }
}

fn model_definitions(app: Option<&tauri::AppHandle>) -> Vec<(String, String, String)> {
  let profile = if let Some(a) = app {
    ModelProfile::parse(&read_setup_flags(a).model_profile)
  } else {
    ModelProfile::Medium
  };

  let mut out: Vec<(String, String, String)> = Vec::new();
  for (custom_name, file_name) in MODEL_FILES {
    let mut base = default_base_model_for(custom_name, profile).to_string();

    if let Ok(path) = modelfile_path(app, file_name) {
      if let Ok(raw) = fs::read_to_string(&path) {
        for line in raw.lines() {
          let t = line.trim();
          if t.starts_with("FROM ") {
            let parsed = t.trim_start_matches("FROM ").trim();
            if !parsed.is_empty() {
              base = parsed.to_string();
            }
            break;
          }
        }
      }
    }

    out.push((custom_name.to_string(), base, file_name.to_string()));
  }
  out
}

fn pull_candidates(base: &str) -> Vec<String> {
  match base {
    "llama3.2:3b" => vec!["llama3.2:3b".to_string(), "llama3.2:3b-instruct".to_string()],
    "qwen2.5-coder:7b-instruct" => vec!["qwen2.5-coder:7b-instruct".to_string(), "qwen2.5-coder:7b".to_string()],
    "qwen2.5-coder:14b-instruct" => vec!["qwen2.5-coder:14b-instruct".to_string(), "qwen2.5-coder:14b".to_string(), "qwen2.5-coder:7b-instruct".to_string()],
    "qwen2.5-coder:3b-instruct" => vec!["qwen2.5-coder:3b-instruct".to_string(), "qwen2.5-coder:3b".to_string(), "qwen2.5-coder:1.5b-instruct".to_string()],
    "qwen2.5-coder:1.5b-instruct" => vec!["qwen2.5-coder:1.5b-instruct".to_string(), "qwen2.5-coder:3b-instruct".to_string()],
    "qwen2.5:14b-instruct" => vec!["qwen2.5:14b-instruct".to_string(), "qwen2.5:14b".to_string(), "qwen2.5:7b-instruct".to_string()],
    "qwen2.5:7b-instruct" => vec!["qwen2.5:7b-instruct".to_string(), "qwen2.5:7b".to_string(), "llama3.2:3b".to_string()],
    "qwen2.5:1.5b-instruct" => vec!["qwen2.5:1.5b-instruct".to_string(), "qwen2.5:3b-instruct".to_string(), "llama3.2:3b".to_string()],
    "gemma3:4b" => vec!["gemma3:4b".to_string(), "gemma3:1b".to_string()],
    "gemma3:1b" => vec!["gemma3:1b".to_string(), "gemma3:4b".to_string()],
    "qwen2-vl:2b-instruct" => vec![
      "qwen2-vl:2b-instruct".to_string(),
      "qwen2-vl:2b-instruct-q4_K_M".to_string(),
      "qwen2-vl:7b-instruct".to_string(),
      "llava:7b".to_string(),
      "llava:13b".to_string(),
    ],
    "qwen2-vl:7b-instruct" => vec![
      "qwen2-vl:7b-instruct".to_string(),
      "qwen2-vl:2b-instruct".to_string(),
      "llava:7b".to_string(),
      "llava:13b".to_string(),
    ],
    _ => vec![base.to_string()],
  }
}

fn locate_modelfiles_dir(app: Option<&tauri::AppHandle>) -> Result<PathBuf, String> {
  let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
  let mut candidates: Vec<PathBuf> = vec![
    cwd.join("model_files"),
    cwd.join("..\\model_files"),
    cwd.join("..\\..\\model_files"),
  ];

  if let Ok(exe) = std::env::current_exe() {
    if let Some(exe_dir) = exe.parent() {
      candidates.push(exe_dir.join("model_files"));
      candidates.push(exe_dir.join("..\\..\\..\\model_files"));
    }
  }

  if let Some(a) = app {
    if let Ok(resource_dir) = a.path().resource_dir() {
      candidates.push(resource_dir.join("model_files"));
    }
  }

  candidates
    .into_iter()
    .find(|p| p.exists() && p.is_dir())
    .ok_or_else(|| "Could not locate model_files directory".to_string())
}

fn modelfile_path(app: Option<&tauri::AppHandle>, filename: &str) -> Result<PathBuf, String> {
  let dir = locate_modelfiles_dir(app)?;
  let path = dir.join(filename);
  if path.exists() && path.is_file() {
    Ok(path)
  } else {
    Err(format!("Modelfile not found: {}", path.display()))
  }
}

fn make_temp_modelfile_with_from(src: &std::path::Path, from_model: &str) -> Result<PathBuf, String> {
  let raw = fs::read_to_string(src).map_err(|e| format!("Failed to read {}: {}", src.display(), e))?;
  let mut replaced = false;
  let mut out_lines: Vec<String> = Vec::new();

  for line in raw.lines() {
    if !replaced && line.trim_start().starts_with("FROM ") {
      out_lines.push(format!("FROM {}", from_model));
      replaced = true;
    } else {
      out_lines.push(line.to_string());
    }
  }

  if !replaced {
    out_lines.insert(0, format!("FROM {}", from_model));
  }

  let safe = from_model.replace(':', "_").replace('/', "_");
  let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
  let tmp = std::env::temp_dir().join(format!("iris_modelfile_{}_{}.txt", safe, nanos));
  fs::write(&tmp, out_lines.join("\n")).map_err(|e| format!("Failed to write {}: {}", tmp.display(), e))?;
  Ok(tmp)
}

#[tauri::command]
pub fn check_ollama_installed() -> bool {
  let via_path = Command::new(resolve_ollama_executable())
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
  via_path
}

#[tauri::command]
pub async fn download_and_install_ollama(app: tauri::AppHandle) -> Result<(), String> {
  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(600))
    .build()
    .map_err(|e| e.to_string())?;

  let resp = client
    .get("https://ollama.com/download/OllamaSetup.exe")
    .send()
    .await
    .map_err(|e| format!("Download failed: {}", e))?;

  if !resp.status().is_success() {
    return Err(format!("Download returned status {}", resp.status()));
  }

  let total = resp.content_length().unwrap_or(0);
  let mut downloaded: u64 = 0;
  let mut bytes: Vec<u8> = if total > 0 { Vec::with_capacity(total as usize) } else { Vec::new() };

  let mut resp = resp;
  loop {
    match resp.chunk().await.map_err(|e| e.to_string())? {
      Some(chunk) => {
        downloaded += chunk.len() as u64;
        bytes.extend_from_slice(&chunk);
        let _ = app.emit("ollama-download-progress", serde_json::json!({
          "downloaded": downloaded,
          "total": total,
        }));
      }
      None => break,
    }
  }

  let installer = std::env::temp_dir().join("OllamaSetup.exe");
  fs::write(&installer, &bytes).map_err(|e| e.to_string())?;

  // Launch with UAC elevation via PowerShell so App Control policy allows it
  let path_str = installer.to_string_lossy().to_string();
  Command::new("powershell")
    .args([
      "-NoProfile",
      "-WindowStyle", "Hidden",
      "-Command",
      &format!("Start-Process -FilePath '{}' -Verb RunAs", path_str),
    ])
    .spawn()
    .map_err(|e| format!("Failed to launch installer: {}", e))?;

  Ok(())
}

#[tauri::command]
pub fn open_modelfiles_folder(app: tauri::AppHandle) -> Result<(), String> {
  let folder = locate_modelfiles_dir(Some(&app))?;

  let cmd = if cfg!(target_os = "windows") { "explorer" }
        else if cfg!(target_os = "macos") { "open" }
        else { "xdg-open" };

  Command::new(cmd)
    .arg(&folder)
    .spawn()
    .map_err(|e| e.to_string())?;

  Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyModelProfileResult {
  pub profile: String,
  pub files_written: usize,
}

fn profile_params(profile: ModelProfile, role: &str) -> (usize, usize, usize, f32, f32, usize, f32, usize) {
  match (profile, role) {
    (ModelProfile::Ultra, "organizer") => (8192, 64, 1536, 0.22, 0.92, 48, 1.06, 192),
    (ModelProfile::Ultra, "coder") => (6144, 64, 1200, 0.18, 0.90, 48, 1.06, 192),
    (ModelProfile::Ultra, "summarizer") => (2048, 32, 240, 0.08, 0.60, 24, 1.10, 128),
    (ModelProfile::Ultra, "vision") => (4096, 64, 1400, 0.30, 0.90, 40, 1.07, 192),

    (ModelProfile::High, "organizer") => (4096, 48, 1200, 0.24, 0.90, 40, 1.06, 160),
    (ModelProfile::High, "coder") => (3072, 48, 900, 0.20, 0.90, 40, 1.06, 160),
    (ModelProfile::High, "summarizer") => (1536, 24, 220, 0.09, 0.60, 24, 1.10, 128),
    (ModelProfile::High, "vision") => (2048, 48, 1100, 0.32, 0.90, 40, 1.07, 192),

    // MediumHigh: 8 GB VRAM + 32 GB RAM â€” Developer Baseline (7B models, 3-4K context)
    (ModelProfile::MediumHigh, "organizer") => (4096, 48, 1100, 0.24, 0.90, 40, 1.06, 160),
    (ModelProfile::MediumHigh, "coder") => (3072, 48, 896, 0.20, 0.90, 40, 1.06, 160),
    (ModelProfile::MediumHigh, "summarizer") => (1536, 24, 200, 0.09, 0.60, 24, 1.10, 128),
    (ModelProfile::MediumHigh, "vision") => (2048, 48, 1000, 0.32, 0.90, 40, 1.07, 192),

    (ModelProfile::Medium, "organizer") => (2048, 32, 896, 0.25, 0.90, 40, 1.06, 160),
    (ModelProfile::Medium, "coder") => (1536, 32, 700, 0.20, 0.90, 40, 1.06, 128),
    (ModelProfile::Medium, "summarizer") => (1024, 20, 160, 0.10, 0.60, 30, 1.10, 160),
    (ModelProfile::Medium, "vision") => (1536, 48, 1000, 0.35, 0.90, 40, 1.07, 192),

    (ModelProfile::Low, "organizer") => (1280, 24, 640, 0.24, 0.88, 32, 1.06, 128),
    (ModelProfile::Low, "coder") => (1024, 24, 520, 0.18, 0.88, 32, 1.06, 96),
    (ModelProfile::Low, "summarizer") => (768, 16, 128, 0.08, 0.58, 24, 1.10, 96),
    (ModelProfile::Low, "vision") => (1024, 32, 800, 0.32, 0.88, 32, 1.07, 128),

    (ModelProfile::Minimal, "organizer") => (896, 16, 480, 0.22, 0.85, 24, 1.05, 96),
    (ModelProfile::Minimal, "coder") => (768, 16, 420, 0.16, 0.85, 24, 1.05, 80),
    (ModelProfile::Minimal, "summarizer") => (512, 12, 96, 0.07, 0.55, 20, 1.10, 64),
    (ModelProfile::Minimal, "vision") => (768, 24, 640, 0.30, 0.85, 24, 1.06, 96),
    _ => (1024, 24, 512, 0.20, 0.90, 40, 1.06, 128),
  }
}

fn build_modelfile(custom_name: &str, from_model: &str, profile: ModelProfile) -> String {
  let role = if custom_name.contains("coder") {
    "coder"
  } else if custom_name.contains("summarizer") {
    "summarizer"
  } else if custom_name.contains("vision") {
    "vision"
  } else {
    "organizer"
  };

  let (num_ctx, num_keep, num_predict, temperature, top_p, top_k, repeat_penalty, repeat_last_n) = profile_params(profile, role);

  let system = match role {
    "coder" => "You are Iris-Coder. Produce correct, minimal, runnable code.\n- Default to the shown language/framework.\n- When the user asks for code, output ONLY one fenced code block with the correct language tag. No plans or extra text.\n- For edits, return the full revised file inside the single code fence.\n- Validation first: imports, paths, interfaces.\n- For Godot scene drafting, generate clean .tscn structures that can scale from simple primitives to multi-node compositions (buildings, cloud clusters, environment blocks).\n- Prefer reusable node hierarchies and clear naming so future procedural expansion is possible.\n- No chain-of-thought.",
    "summarizer" => "You are Iris-Summarizer. Output <=6 tight bullets or <=90 words.\nFacts only; preserve names, numbers, file/function ids, decisions.\nFor code/logs: extract key errors, likely causes, next actions.\nNo advice unless asked. No fluff. No chain-of-thought.",
    "vision" => "You are Iris-Vision. Analyze images and UI captures with clear, grounded observations.\nCall out uncertainty when image evidence is weak.\nPrefer concise, actionable findings over speculation.",
    _ => "You are Iris, a helpful assistant that uses provided context.\nIf context is missing, state uncertainty briefly and ask one concise clarifying question.\nNever invent project facts, files, features, or previous decisions.",
  };

  format!(
    "FROM {}\n\nSYSTEM \"\"\"\n{}\n\"\"\"\n\nPARAMETER num_ctx {}\nPARAMETER num_keep {}\nPARAMETER num_predict {}\nPARAMETER temperature {:.2}\nPARAMETER top_p {:.2}\nPARAMETER top_k {}\nPARAMETER repeat_penalty {:.2}\nPARAMETER repeat_last_n {}\n",
    from_model,
    system,
    num_ctx,
    num_keep,
    num_predict,
    temperature,
    top_p,
    top_k,
    repeat_penalty,
    repeat_last_n
  )
}

#[tauri::command]
pub fn apply_model_profile(app: tauri::AppHandle, profile: String) -> Result<ApplyModelProfileResult, String> {
  let parsed = ModelProfile::parse(&profile);
  let mut files_written = 0usize;

  for (custom_name, file_name) in MODEL_FILES {
    let base = default_base_model_for(custom_name, parsed);
    let body = build_modelfile(custom_name, base, parsed);
    let p = modelfile_path(Some(&app), file_name)?;
    fs::write(&p, body).map_err(|e| format!("Failed writing {}: {}", p.display(), e))?;
    files_written += 1;
  }

  let mut flags = read_setup_flags(&app);
  flags.model_profile = parsed.as_str().to_string();
  write_setup_flags(&app, &flags)?;

  Ok(ApplyModelProfileResult {
    profile: parsed.as_str().to_string(),
    files_written,
  })
}

// ---------------------------------------------------------------------------
// Hardware detection
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareProfile {
  pub vram_gb: f64,
  pub ram_gb: f64,
  pub cpu_cores: usize,
  pub gpu_name: String,
  pub detected_profile: String,
  pub detection_note: String,
}

/// Try to get VRAM GB from nvidia-smi, falling back to WMIC on Windows.
/// Returns (gb, gpu_name, note).
fn probe_vram() -> (f64, String, String) {
  // Attempt 1: nvidia-smi
  if let Ok(out) = Command::new("nvidia-smi")
    .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
    .output()
  {
    if out.status.success() {
      let text = String::from_utf8_lossy(&out.stdout);
      let line = text.lines().next().unwrap_or("").trim().to_string();
      if !line.is_empty() {
        let parts: Vec<&str> = line.splitn(2, ',').collect();
        let gpu_name = parts.first().map(|s| s.trim().to_string()).unwrap_or_default();
        let vram_mib_str = parts.get(1).map(|s| s.trim()).unwrap_or("0");
        if let Ok(mib) = vram_mib_str.parse::<f64>() {
          let gb = mib / 1024.0;
          return (gb, gpu_name, "nvidia-smi".to_string());
        }
      }
    }
  }

  // Attempt 2: PowerShell Win32_VideoController (Windows only)
  #[cfg(target_os = "windows")]
  {
    if let Ok(out) = Command::new("powershell")
      .args(["-NoProfile", "-Command",
        "Get-CimInstance Win32_VideoController | Select-Object -First 1 Name,AdapterRAM | ConvertTo-Json -Compress"])
      .output()
    {
      if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) {
          let name = v.get("Name").and_then(|n| n.as_str()).unwrap_or("").to_string();
          // AdapterRAM is in bytes
          let bytes = v.get("AdapterRAM").and_then(|b| b.as_f64()).unwrap_or(0.0);
          let gb = bytes / (1024.0 * 1024.0 * 1024.0);
          if gb > 0.5 {
            return (gb, name, "Win32_VideoController".to_string());
          }
        }
      }
    }
  }

  (0.0, "None detected".to_string(), "probe-failed".to_string())
}

#[tauri::command]
pub fn detect_hardware_profile() -> Result<HardwareProfile, String> {
  let mut sys = System::new_all();
  sys.refresh_all();

  let ram_bytes = sys.total_memory(); // bytes
  let ram_gb = ram_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
  let cpu_cores = sys.cpus().len();

  let (vram_gb, gpu_name, probe_source) = probe_vram();
  let (detected_profile, detection_note) = if vram_gb == 0.0 {
    (
      "Minimal",
      format!("No dedicated GPU detected via {}. CPU-only inference.", probe_source),
    )
  } else if vram_gb >= 16.0 {
    (
      "Ultra",
      format!("{:.1} GB VRAM (via {}) >= 16 GB - Ultra tier.", vram_gb, probe_source),
    )
  } else if vram_gb >= 12.0 {
    (
      "High",
      format!("{:.1} GB VRAM (via {}) >= 12 GB - High tier.", vram_gb, probe_source),
    )
  } else if vram_gb >= 7.5 && ram_gb >= 31.0 {
    (
      "MediumHigh",
      format!(
        "{:.1} GB VRAM + {:.1} GB RAM (via {}) - Developer Baseline (MediumHigh).",
        vram_gb, ram_gb, probe_source
      ),
    )
  } else if vram_gb >= 6.0 && vram_gb <= 8.99 && ram_gb >= 16.0 {
    (
      "Medium",
      format!(
        "{:.1} GB VRAM + {:.1} GB RAM (via {}) - Medium tier.",
        vram_gb, ram_gb, probe_source
      ),
    )
  } else if vram_gb <= 4.0 || ram_gb < 16.0 {
    (
      "Low",
      format!(
        "{:.1} GB VRAM, {:.1} GB RAM (via {}) - Low tier.",
        vram_gb, ram_gb, probe_source
      ),
    )
  } else {
    (
      "Low",
      format!(
        "{:.1} GB VRAM, {:.1} GB RAM (via {}) - defaulting to Low for stability.",
        vram_gb, ram_gb, probe_source
      ),
    )
  };

  Ok(HardwareProfile {
    vram_gb,
    ram_gb,
    cpu_cores,
    gpu_name,
    detected_profile: detected_profile.to_string(),
    detection_note,
  })
}


// ---------------------------------------------------------------------------
// Modelfile editor commands
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModelfileParam {
  pub key: String,
  pub value: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModelfileData {
  pub filename: String,
  pub display_name: String,
  pub nickname: String,
  pub from_model: String,
  pub system_prompt: String,
  pub params: Vec<ModelfileParam>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModelfileParamsData {
  pub filename: String,
  pub from_model: String,
  pub params: Vec<ModelfileParam>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SaveModelfileArgs {
  pub filename: String,
  pub from_model: String,
  pub params: Vec<ModelfileParam>,
  #[serde(default)]
  pub system_prompt: Option<String>,
  #[serde(default)]
  pub nickname: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SaveModelfileParamsArgs {
  pub filename: String,
  #[serde(default)]
  pub from_model: Option<String>,
  pub params: Vec<ModelfileParam>,
}

fn modelfile_display_name(filename: &str) -> &'static str {
  if filename.contains("organizer") { "Organizer" }
  else if filename.contains("coder") { "Coder" }
  else if filename.contains("summarizer") { "Summarizer" }
  else if filename.contains("vision") { "Vision" }
  else { "" }
}

fn parse_modelfile_from_model(raw: &str) -> String {
  let from_re = match Regex::new(r"(?im)^\s*FROM\s+(.+?)\s*$") {
    Ok(r) => r,
    Err(_) => return String::new(),
  };
  from_re
    .captures(raw)
    .and_then(|caps| caps.get(1).map(|m| m.as_str().trim().to_string()))
    .unwrap_or_default()
}

fn parse_modelfile_params(raw: &str) -> Vec<ModelfileParam> {
  let param_re = match Regex::new(r"(?im)^\s*PARAMETER\s+([A-Za-z0-9_.-]+)\s+(.+?)\s*$") {
    Ok(r) => r,
    Err(_) => return Vec::new(),
  };

  param_re
    .captures_iter(raw)
    .filter_map(|caps| {
      let key = caps.get(1)?.as_str().trim();
      let value = caps.get(2)?.as_str().trim();
      if key.is_empty() || value.is_empty() {
        None
      } else {
        Some(ModelfileParam {
          key: key.to_string(),
          value: value.to_string(),
        })
      }
    })
    .collect()
}

// ---------------------------------------------------------------------------
// ModelConfig â€” per-model enable flags, custom models, and organizer notes
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CustomModelDef {
  pub id: String,
  pub filename: String,
  pub nickname: String,
  pub enabled: bool,
  pub note: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
  #[serde(default = "default_true")]
  pub coder_enabled: bool,
  #[serde(default = "default_true")]
  pub vision_enabled: bool,
  #[serde(default = "default_true")]
  pub summarizer_enabled: bool,
  #[serde(default)]
  pub custom_models: Vec<CustomModelDef>,
  #[serde(default)]
  pub model_notes: std::collections::HashMap<String, String>,
  #[serde(default)]
  pub status_verbs: std::collections::HashMap<String, String>,
}

fn default_true() -> bool { true }

impl Default for ModelConfig {
  fn default() -> Self {
    Self {
      coder_enabled: true,
      vision_enabled: true,
      summarizer_enabled: true,
      custom_models: Vec::new(),
      model_notes: std::collections::HashMap::new(),
      status_verbs: std::collections::HashMap::new(),
    }
  }
}

fn model_config_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
  let dir = memory_dir(app)?;
  Ok(dir.join("model_config.json"))
}

fn read_model_config(app: &tauri::AppHandle) -> ModelConfig {
  let path = match model_config_path(app) { Ok(p) => p, Err(_) => return ModelConfig::default() };
  if !path.exists() { return ModelConfig::default(); }
  let raw = match fs::read_to_string(&path) { Ok(s) => s, Err(_) => return ModelConfig::default() };
  serde_json::from_str::<ModelConfig>(&raw).unwrap_or_default()
}

#[tauri::command]
pub fn get_model_config(app: tauri::AppHandle) -> ModelConfig {
  read_model_config(&app)
}

#[tauri::command]
pub fn save_model_config(app: tauri::AppHandle, config: ModelConfig) -> Result<(), String> {
  let path = model_config_path(&app)?;
  let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
  atomic_write_json(&path, &json)
}

#[tauri::command]
pub fn create_custom_modelfile(app: tauri::AppHandle, filename: String, nickname: String) -> Result<(), String> {
  let fname = filename.trim();
  if fname.contains("..") || fname.contains('/') || fname.contains('\\') {
    return Err("Invalid filename".to_string());
  }
  if !fname.ends_with(".txt") && !fname.ends_with(".modelfile") {
    return Err("Filename must end with .txt or .modelfile".to_string());
  }
  let dir = locate_modelfiles_dir(Some(&app))?;
  let path = dir.join(fname);
  if path.exists() {
    return Err(format!("File {} already exists", fname));
  }
  let safe_nick = nickname.trim();
  let content = format!(
    "# NICKNAME: {}\nFROM llama3\n\nSYSTEM \"\"\"\nYou are a helpful assistant.\n\"\"\"\n\nPARAMETER num_ctx 4096\nPARAMETER temperature 0.7\n",
    safe_nick
  );
  atomic_write_text(&path, &content).map_err(|e| format!("Failed to create {}: {}", fname, e))
}

#[tauri::command]
pub fn delete_custom_modelfile(app: tauri::AppHandle, filename: String) -> Result<(), String> {
  let fname = filename.trim();
  if fname.contains("..") || fname.contains('/') || fname.contains('\\') {
    return Err("Invalid filename".to_string());
  }
  let defaults = ["modelfile_organizer.txt", "modelfile_coder.txt", "modelfile_summarizer.txt", "modelfile_vision.txt"];
  if defaults.contains(&fname) {
    return Err("Cannot delete a default modelfile".to_string());
  }
  let dir = locate_modelfiles_dir(Some(&app))?;
  let path = dir.join(fname);
  if path.exists() {
    fs::remove_file(&path).map_err(|e| format!("Failed to delete {}: {}", fname, e))?;
  }
  Ok(())
}

#[tauri::command]
pub fn list_modelfiles(app: tauri::AppHandle) -> Result<Vec<String>, String> {
  let dir = locate_modelfiles_dir(Some(&app))?;
  let mut names = Vec::new();
  let entries = fs::read_dir(&dir).map_err(|e| format!("Cannot read model_files dir: {}", e))?;
  for entry in entries.flatten() {
    let name = entry.file_name().to_string_lossy().to_string();
    if name.ends_with(".txt") {
      names.push(name);
    }
  }
  names.sort();
  Ok(names)
}

#[tauri::command]
pub fn read_modelfile_params(app: tauri::AppHandle, filename: String) -> Result<ModelfileParamsData, String> {
  let path = modelfile_path(Some(&app), &filename)?;
  let raw = fs::read_to_string(&path)
    .map_err(|e| format!("Cannot read {}: {}", filename, e))?;

  Ok(ModelfileParamsData {
    filename,
    from_model: parse_modelfile_from_model(&raw),
    params: parse_modelfile_params(&raw),
  })
}

#[tauri::command]
pub fn read_modelfile_data(app: tauri::AppHandle, filename: String) -> Result<ModelfileData, String> {
  let path = modelfile_path(Some(&app), &filename)?;
  let raw = fs::read_to_string(&path)
    .map_err(|e| format!("Cannot read {}: {}", filename, e))?;

  let from_model = parse_modelfile_from_model(&raw);
  let params = parse_modelfile_params(&raw);
  let mut system_lines: Vec<String> = Vec::new();
  let mut in_system = false;
  let mut nickname = String::new();

  for line in raw.lines() {
    let trimmed = line.trim();
    if trimmed.starts_with("# NICKNAME:") && nickname.is_empty() {
      nickname = trimmed.trim_start_matches("# NICKNAME:").trim().to_string();
      continue;
    }
    if trimmed.starts_with("SYSTEM \"\"\"") {
      in_system = true;
      let after_tag = trimmed.trim_start_matches("SYSTEM").trim().trim_start_matches("\"\"\"");
      let closes_inline = after_tag.ends_with("\"\"\"") && after_tag.len() > 3;
      let content = if closes_inline { after_tag.trim_end_matches("\"\"\"").trim_end() } else { after_tag };
      if !content.is_empty() { system_lines.push(content.to_string()); }
      if closes_inline { in_system = false; }
    } else if in_system {
      if trimmed == "\"\"\"" {
        in_system = false;
      } else if trimmed.ends_with("\"\"\"") {
        system_lines.push(trimmed.trim_end_matches("\"\"\"").trim_end().to_string());
        in_system = false;
      } else {
        system_lines.push(line.to_string());
      }
    }
  }

  let display = modelfile_display_name(&filename);
  if nickname.is_empty() {
    nickname = display.to_string();
  }
  Ok(ModelfileData {
    filename,
    display_name: if nickname.is_empty() { display.to_string() } else { nickname.clone() },
    nickname,
    from_model,
    system_prompt: system_lines.join("\n"),
    params,
  })
}

#[tauri::command]
pub fn save_modelfile_data(app: tauri::AppHandle, args: SaveModelfileArgs) -> Result<(), String> {
  let fname = args.filename.trim();
  if fname.contains("..") || fname.contains("/") || fname.contains("\\") {
    return Err("Invalid filename: must not contain path separators or '..'".to_string());
  }
  if !fname.ends_with(".txt") && !fname.ends_with(".modelfile") {
    return Err("Invalid filename: must end with .txt or .modelfile".to_string());
  }

  let path = modelfile_path(Some(&app), fname)?;
  let raw = fs::read_to_string(&path)
    .map_err(|e| format!("Cannot read {}: {}", fname, e))?;

  let mut out_lines: Vec<String> = Vec::new();
  let mut in_system = false;
  let mut from_replaced = false;
  let mut params_written = false;
  let mut in_params_section = false;
  let mut nickname_written = false;
  let mut system_replaced = false;
  let system_override = args.system_prompt.as_deref().map(|s| s.trim_end_matches('\r')).map(|s| s.to_string());
  let nickname_override = args.nickname.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| s.to_string());

  for line in raw.lines() {
    let trimmed = line.trim();
    if trimmed.starts_with("# NICKNAME:") {
      if let Some(ref nick) = nickname_override {
        out_lines.push(format!("# NICKNAME: {}", nick));
      } else {
        out_lines.push(line.to_string());
      }
      nickname_written = true;
      continue;
    }
    if trimmed.starts_with("FROM ") && !from_replaced {
      out_lines.push(format!("FROM {}", args.from_model.trim()));
      from_replaced = true;
      continue;
    }
    if trimmed.starts_with("SYSTEM \"\"\"") {
      if let Some(ref prompt) = system_override {
        let after_tag = trimmed.trim_start_matches("SYSTEM").trim().trim_start_matches("\"\"\"");
        let closes_inline = after_tag.ends_with("\"\"\"") && after_tag.len() > 3;
        out_lines.push("SYSTEM \"\"\"".to_string());
        for p in prompt.lines() {
          out_lines.push(p.to_string());
        }
        out_lines.push("\"\"\"".to_string());
        in_system = !closes_inline;
        system_replaced = true;
        continue;
      }
      in_system = true;
      let after_tag = trimmed.trim_start_matches("SYSTEM").trim().trim_start_matches("\"\"\"");
      if after_tag.ends_with("\"\"\"") && after_tag.len() > 3 { in_system = false; }
      out_lines.push(line.to_string());
      continue;
    }
    if in_system {
      if trimmed == "\"\"\"" || trimmed.ends_with("\"\"\"") { in_system = false; }
      if system_replaced {
        continue;
      }
      out_lines.push(line.to_string());
      continue;
    }
    if trimmed.starts_with("PARAMETER ") {
      if !in_params_section { in_params_section = true; }
      continue;
    }
    if in_params_section && !trimmed.starts_with("PARAMETER ") && !params_written {
      for p in &args.params {
        out_lines.push(format!("PARAMETER {} {}", p.key, p.value));
      }
      params_written = true;
      in_params_section = false;
    }
    out_lines.push(line.to_string());
  }

  if let Some(nick) = nickname_override {
    if !nickname_written {
      out_lines.insert(0, format!("# NICKNAME: {}", nick));
    }
  }

  if let Some(prompt) = system_override {
    if !system_replaced {
      if out_lines.last().map(|l| !l.is_empty()).unwrap_or(false) {
        out_lines.push(String::new());
      }
      out_lines.push("SYSTEM \"\"\"".to_string());
      for p in prompt.lines() {
        out_lines.push(p.to_string());
      }
      out_lines.push("\"\"\"".to_string());
    }
  }

  if !params_written {
    if out_lines.last().map(|l| !l.is_empty()).unwrap_or(false) {
      out_lines.push(String::new());
    }
    for p in &args.params {
      out_lines.push(format!("PARAMETER {} {}", p.key, p.value));
    }
  }

  atomic_write_text(&path, &out_lines.join("\n"))?;

  Ok(())
}

#[tauri::command]
pub fn save_modelfile_params(app: tauri::AppHandle, args: SaveModelfileParamsArgs) -> Result<(), String> {
  let fname = args.filename.trim();
  if fname.contains("..") || fname.contains("/") || fname.contains("\\") {
    return Err("Invalid filename: must not contain path separators or '..'".to_string());
  }
  if !fname.ends_with(".txt") && !fname.ends_with(".modelfile") {
    return Err("Invalid filename: must end with .txt or .modelfile".to_string());
  }

  let path = modelfile_path(Some(&app), fname)?;
  let raw = fs::read_to_string(&path)
    .map_err(|e| format!("Cannot read {}: {}", fname, e))?;
  let fallback_from = parse_modelfile_from_model(&raw);
  let from_model = args
    .from_model
    .as_deref()
    .map(str::trim)
    .filter(|v| !v.is_empty())
    .unwrap_or(fallback_from.as_str())
    .to_string();

  let mut out_lines: Vec<String> = Vec::new();
  let mut from_replaced = false;
  let mut params_written = false;
  let mut in_params_section = false;

  for line in raw.lines() {
    let trimmed = line.trim();
    if trimmed.starts_with("FROM ") && !from_replaced {
      out_lines.push(format!("FROM {}", from_model));
      from_replaced = true;
      continue;
    }
    if trimmed.starts_with("PARAMETER ") {
      in_params_section = true;
      continue;
    }
    if in_params_section && !trimmed.starts_with("PARAMETER ") && !params_written {
      for p in &args.params {
        out_lines.push(format!("PARAMETER {} {}", p.key, p.value));
      }
      params_written = true;
      in_params_section = false;
    }
    out_lines.push(line.to_string());
  }

  if !from_replaced {
    out_lines.insert(0, format!("FROM {}", from_model));
  }

  if !params_written {
    if out_lines.last().map(|l| !l.is_empty()).unwrap_or(false) {
      out_lines.push(String::new());
    }
    for p in &args.params {
      out_lines.push(format!("PARAMETER {} {}", p.key, p.value));
    }
  }

  atomic_write_text(&path, &out_lines.join("\n"))?;

  Ok(())
}

#[tauri::command]
pub fn check_models_ready() -> Vec<String> {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return MODEL_FILES
                .iter()
              .map(|&(name, _)| name.to_string())
                .collect()
        }
    };

    let mut missing = Vec::new();
        for &(custom_name, _) in MODEL_FILES {
        let show = client
            .post("http://127.0.0.1:11434/api/show")
            .json(&serde_json::json!({ "name": custom_name }))
            .send();
        match show {
            Ok(resp) if resp.status().is_success() => {}
            _ => missing.push(custom_name.to_string()),
        }
    }
    missing
}

#[tauri::command]
pub async fn pull_and_create_models(app: tauri::AppHandle) -> Result<(), String> {
    ensure_ollama_running_once();

    // Wait up to 6 s for the Ollama server to be reachable
    tauri::async_runtime::spawn_blocking(|| {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        for _ in 0..12 {
            if client.get("http://127.0.0.1:11434/api/tags").send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    })
    .await
    .ok();

    let definitions = model_definitions(Some(&app));
    let total_steps = definitions.len() * 2;
    let mut step = 0usize;

    for (custom_name, base_model, modelfile_file) in definitions {
        // --- Pull base model ---
        step += 1;
        let _ = app.emit("ollama-setup-progress", serde_json::json!({
            "phase": "pulling",
            "model": base_model,
            "step": step,
            "total": total_steps,
        }));

        let base = base_model.to_string();
        let app_for_pull = app.clone();
        let step_now = step;
        let total_now = total_steps;
        let resolved_base = tauri::async_runtime::spawn_blocking(move || {
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(7200)) // 2 h for large models
                .build()
                .map_err(|e| e.to_string())?;
          let mut errors: Vec<String> = Vec::new();
          for candidate in pull_candidates(&base) {
            let resp = client
              .post("http://127.0.0.1:11434/api/pull")
              .json(&serde_json::json!({ "name": candidate, "stream": true }))
              .send()
              .map_err(|e| format!("Pull failed: {}", e))?;

            if !resp.status().is_success() {
              let status = resp.status();
              let body = resp.text().unwrap_or_else(|_| "<no body>".to_string());
              errors.push(format!("{} -> {} {}", candidate, status, body));
              continue;
            }

            let mut succeeded = false;
            let reader = BufReader::new(resp);
            for line in reader.lines() {
              let line = match line {
                Ok(l) => l,
                Err(_) => continue,
              };
              if line.trim().is_empty() {
                continue;
              }
              if let Ok(v) = serde_json::from_str::<Value>(&line) {
                let st = v.get("status").and_then(|x| x.as_str()).unwrap_or("");
                let completed = v.get("completed").and_then(|x| x.as_u64()).unwrap_or(0);
                let bytes_total = v.get("total").and_then(|x| x.as_u64()).unwrap_or(0);
                let _ = app_for_pull.emit("ollama-setup-progress", serde_json::json!({
                  "phase": "pulling",
                  "model": candidate,
                  "step": step_now,
                  "total": total_now,
                  "status": st,
                  "completed": completed,
                  "bytesTotal": bytes_total,
                }));
                if st.eq_ignore_ascii_case("success") {
                  succeeded = true;
                }
              }
            }

            if succeeded {
              return Ok::<String, String>(candidate);
            }
          }

          Err(format!("Pull {} failed: {}", base, errors.join(" | ")))
        })
        .await
        .map_err(|e| e.to_string())??;

        // --- Create custom model from modelfile ---
        step += 1;
        let _ = app.emit("ollama-setup-progress", serde_json::json!({
            "phase": "creating",
            "model": custom_name,
            "step": step,
            "total": total_steps,
        }));

        let name = custom_name.to_string();
        let mf_path = modelfile_path(Some(&app), &modelfile_file)?;
        let tmp_mf = make_temp_modelfile_with_from(&mf_path, &resolved_base)?;
        tauri::async_runtime::spawn_blocking(move || {
            let out = Command::new(resolve_ollama_executable())
              .arg("create")
              .arg(&name)
              .arg("-f")
              .arg(&tmp_mf)
              .output()
              .map_err(|e| format!("Failed to run ollama create: {}", e))?;
            let _ = fs::remove_file(&tmp_mf);

            if !out.status.success() {
              let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
              let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
              return Err(format!(
                "Create {} failed: exit {} | stdout: {} | stderr: {}",
                name,
                out.status,
                stdout,
                stderr
              ));
            }
            Ok::<(), String>(())
        })
        .await
        .map_err(|e| e.to_string())??;
    }

    let _ = app.emit("ollama-setup-progress", serde_json::json!({
        "phase": "done",
        "step": total_steps,
        "total": total_steps,
    }));

    Ok(())
}

#[tauri::command]
pub fn restart_app(app: tauri::AppHandle) {
    app.restart();
}

#[tauri::command]
pub async fn sanitize_tab_titles(app: tauri::AppHandle) -> Result<u32, String> {
  let dir = iris_open_tabs_dir(&app)?;
  let mut updated = 0u32;

  if let Ok(rd) = read_dir(&dir) {
    for e in rd.flatten() {
      let path = e.path();
      if path.extension().and_then(|s| s.to_str()) != Some("json") {
        continue;
      }
      if path.to_string_lossy().contains(".tmp") {
        continue;
      }

      // Extract the tab ID from the filename (e.g., "tab_5.json" -> 5)
      if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
        if file_name.starts_with("tab_") && file_name.ends_with(".json") {
          let id_str = &file_name[4..file_name.len() - 5]; // "tab_5.json" -> "5"
          if let Ok(tab_id) = id_str.parse::<u32>() {
            // Load the snapshot
            match iris_read_snapshot_file(&path) {
              Ok(mut snap) => {
                let expected_title = format!("Tab {}", tab_id);
                // Only update if the title doesn't match the expected format
                if snap.title != expected_title {
                  snap.title = expected_title;
                  // Write the corrected snapshot back
                  if iris_write_snapshot_file(&path, &snap).is_ok() {
                    updated += 1;
                    eprintln!("[sanitize_tab_titles] Updated tab_{}.json title", tab_id);
                  }
                }
              }
              Err(err) => {
                eprintln!("[sanitize_tab_titles] Failed to read {}: {}", path.display(), err);
              }
            }
          }
        }
      }
    }
  }

  eprintln!("[sanitize_tab_titles] Updated {} tab titles", updated);
  Ok(updated)
}

#[tauri::command]
pub fn get_setup_flags(app: tauri::AppHandle) -> SetupFlags {
  read_setup_flags(&app)
}

#[tauri::command]
pub fn resolve_github_release_asset(
  repo: String,
  prefer_msi: Option<bool>,
  include_prerelease: Option<bool>,
) -> Result<serde_json::Value, String> {
  let raw_repo = repo.trim();
  let repo_slug = if raw_repo.starts_with("http://") || raw_repo.starts_with("https://") {
    let normalized = raw_repo.replace("github.com/", "github.com/");
    if let Some(idx) = normalized.find("github.com/") {
      let tail = &normalized[idx + "github.com/".len()..];
      let parts: Vec<&str> = tail.split('/').filter(|p| !p.is_empty()).collect();
      if parts.len() >= 2 {
        format!("{}/{}", parts[0], parts[1])
      } else {
        return Err("GitHub URL must include owner/repo".to_string());
      }
    } else {
      return Err("Invalid GitHub URL format".to_string());
    }
  } else {
    raw_repo.to_string()
  };

  let parts: Vec<&str> = repo_slug.split('/').collect();
  if parts.len() != 2 || parts[0].trim().is_empty() || parts[1].trim().is_empty() {
    return Err("Repo must be in owner/repo format".to_string());
  }

  let prefer_msi = prefer_msi.unwrap_or(false);
  let include_prerelease = include_prerelease.unwrap_or(true);
  let api_url = format!("https://api.github.com/repos/{}/releases?per_page=20", repo_slug);
  let client = Client::builder()
    .timeout(Duration::from_secs(12))
    .build()
    .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
  let res = client
    .get(&api_url)
    .header("User-Agent", "iris-app-updater/1.0")
    .header("Accept", "application/vnd.github+json")
    .send()
    .map_err(|e| format!("GitHub request failed: {}", e))?;
  if !res.status().is_success() {
    return Err(format!("GitHub API returned status {}", res.status()));
  }
  let releases: Value = res
    .json()
    .map_err(|e| format!("Failed to parse GitHub response: {}", e))?;
  let arr = releases
    .as_array()
    .ok_or_else(|| "GitHub response was not a release list".to_string())?;

  for release in arr {
    let is_draft = release.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);
    let is_prerelease = release.get("prerelease").and_then(|v| v.as_bool()).unwrap_or(false);
    if is_draft {
      continue;
    }
    if !include_prerelease && is_prerelease {
      continue;
    }

    let tag = release.get("tag_name").and_then(|v| v.as_str()).unwrap_or("(unknown)").to_string();
    let release_url = release.get("html_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let assets = release.get("assets").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let pick_asset = |is_msi: bool| -> Option<(String, String)> {
      let mut best_setup: Option<(String, String)> = None;
      let mut fallback: Option<(String, String)> = None;
      for a in &assets {
        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let dl = a
          .get("browser_download_url")
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string();
        if name.is_empty() || dl.is_empty() {
          continue;
        }
        let lower = name.to_lowercase();
        if is_msi {
          if lower.ends_with(".msi") {
            return Some((name, dl));
          }
        } else if lower.ends_with("setup.exe") {
          best_setup = Some((name, dl));
        } else if lower.ends_with(".exe") {
          fallback = Some((name, dl));
        }
      }
      best_setup.or(fallback)
    };

    let picked = if prefer_msi {
      pick_asset(true).or_else(|| pick_asset(false))
    } else {
      pick_asset(false).or_else(|| pick_asset(true))
    };

    if let Some((asset_name, download_url)) = picked {
      return Ok(serde_json::json!({
        "repo": repo_slug,
        "tag": tag,
        "prerelease": is_prerelease,
        "releaseUrl": release_url,
        "assetName": asset_name,
        "downloadUrl": download_url,
      }));
    }
  }

  Err("No matching .exe/.msi asset found in recent releases".to_string())
}

// ========== routine plan types ==========

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct RoutineStep {
  pub id: String,
  pub step_type: String,
  pub label: String,
  pub params: std::collections::HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RoutinePlan {
  pub id: String,
  pub goal: String,
  pub steps: Vec<RoutineStep>,
  pub is_long_running: bool,
}

// ========== window / screenshot commands ==========

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
  pub id: u32,
  pub title: String,
  pub process_name: String,
}

#[tauri::command]
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
  let script = "Get-Process | Where-Object { $_.MainWindowTitle -ne '' } | Select-Object Id,ProcessName,MainWindowTitle | ConvertTo-Json -Compress";
  let output = Command::new("powershell")
    .args(["-NoProfile", "-NonInteractive", "-Command", script])
    .output()
    .map_err(|e| format!("PowerShell failed: {}", e))?;
  let stdout = String::from_utf8_lossy(&output.stdout);
  let text = stdout.trim();
  if text.is_empty() {
    return Ok(Vec::new());
  }
  let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Array(vec![]));
  let arr = if parsed.is_array() {
    parsed.as_array().cloned().unwrap_or_default()
  } else {
    vec![parsed]
  };
  let windows = arr.iter().filter_map(|v| {
    let id = v["Id"].as_u64().unwrap_or(0) as u32;
    let title = v["MainWindowTitle"].as_str().unwrap_or("").to_string();
    let pname = v["ProcessName"].as_str().unwrap_or("").to_string();
    if title.is_empty() { None } else { Some(WindowInfo { id, title, process_name: pname }) }
  }).collect();
  Ok(windows)
}

#[tauri::command]
pub fn get_system_stats() -> Result<serde_json::Value, String> {
  let mut sys = System::new();
  sys.refresh_cpu_usage();
  // A second refresh after a brief sleep gives a more accurate delta-based CPU %
  std::thread::sleep(std::time::Duration::from_millis(150));
  sys.refresh_cpu_usage();
  sys.refresh_memory();
  let cpus = sys.cpus();
  let cpu = if cpus.is_empty() {
    0.0_f32
  } else {
    let total: f32 = cpus.iter().map(|c| c.cpu_usage()).sum();
    ((total / cpus.len() as f32) * 10.0).round() / 10.0
  };
  let mem_used_mb = sys.used_memory() / 1_048_576;
  let mem_total_mb = sys.total_memory() / 1_048_576;
  Ok(serde_json::json!({
    "cpuPercent": cpu,
    "memUsedMb": mem_used_mb,
    "memTotalMb": mem_total_mb,
  }))
}

#[tauri::command]
pub fn take_screenshot() -> Result<String, String> {
  let script = r#"
Add-Type -AssemblyName System.Windows.Forms,System.Drawing
try {
  $bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
  $bmp = [System.Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
  $ms = [System.IO.MemoryStream]::new()
  $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  [Convert]::ToBase64String($ms.ToArray())
} catch {
  Write-Error $_.Exception.Message
  exit 1
}"#;
  let output = Command::new("powershell")
    .args(["-NoProfile", "-NonInteractive", "-Command", script])
    .output()
    .map_err(|e| format!("Screenshot failed: {}", e))?;
  if !output.status.success() {
    let err = String::from_utf8_lossy(&output.stderr);
    return Err(format!("Screenshot error: {}", err.trim()));
  }
  let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
  if raw.is_empty() {
    return Err("Screenshot returned empty data".to_string());
  }
  Ok(raw)
}

#[tauri::command]
pub fn take_window_screenshot(window_id: u32) -> Result<String, String> {
  let script = format!(r#"
Add-Type -AssemblyName System.Windows.Forms,System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32 {{
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT {{
    public int Left;
    public int Top;
    public int Right;
    public int Bottom;
  }}
}}
"@
try {{
  $p = Get-Process -Id {window_id} -ErrorAction Stop
  if (-not $p -or $p.MainWindowHandle -eq 0) {{
    throw "Target process has no visible main window"
  }}
  $handle = $p.MainWindowHandle
  [Win32]::ShowWindow($handle, 9) | Out-Null
  [Win32]::SetForegroundWindow($handle) | Out-Null
  Start-Sleep -Milliseconds 220

  $rect = New-Object Win32+RECT
  $ok = [Win32]::GetWindowRect($handle, [ref]$rect)
  if (-not $ok) {{
    throw "Unable to read target window bounds"
  }}
  $width = [Math]::Max(1, $rect.Right - $rect.Left)
  $height = [Math]::Max(1, $rect.Bottom - $rect.Top)

  $bmp = [System.Drawing.Bitmap]::new($width, $height)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, [System.Drawing.Size]::new($width, $height))
  $ms = [System.IO.MemoryStream]::new()
  $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  [Convert]::ToBase64String($ms.ToArray())
}} catch {{
  Write-Error $_.Exception.Message
  exit 1
}}"#);
  let output = Command::new("powershell")
    .args(["-NoProfile", "-NonInteractive", "-Command", &script])
    .output()
    .map_err(|e| format!("Window screenshot failed: {}", e))?;
  if !output.status.success() {
    let err = String::from_utf8_lossy(&output.stderr);
    return Err(format!("Window screenshot error: {}", err.trim()));
  }
  let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
  if raw.is_empty() {
    return Err("Window screenshot returned empty data".to_string());
  }
  Ok(raw)
}

fn resolve_desktop_launch_target(target: &str) -> String {
  let normalized = target.trim().to_lowercase();
  match normalized.as_str() {
    "ms project" | "microsoft project" | "project professional" | "project standard" => "WINPROJ.EXE".to_string(),
    "visual studio code" | "vs code" | "vscode" => "code".to_string(),
    "file explorer" | "windows explorer" | "explorer" => "explorer.exe".to_string(),
    "calculator" => "calc.exe".to_string(),
    "notepad" => "notepad.exe".to_string(),
    "paint" | "mspaint" => "mspaint.exe".to_string(),
    "command prompt" | "cmd" => "cmd.exe".to_string(),
    "powershell" => "powershell.exe".to_string(),
    _ => target.trim().to_string(),
  }
}

#[tauri::command]
pub fn launch_desktop_item(target: String, args: Option<Vec<String>>) -> Result<String, String> {
  let trimmed = target.trim();
  if trimmed.is_empty() {
    return Err("Desktop launch target is required".to_string());
  }

  let resolved = resolve_desktop_launch_target(trimmed);
  let arg_items = args
    .unwrap_or_default()
    .into_iter()
    .map(|arg| ps_single_quote(&arg))
    .collect::<Vec<_>>();
  let script = if arg_items.is_empty() {
    format!(
      "$ErrorActionPreference='Stop'; Start-Process -FilePath {} | Out-Null; Write-Output {}",
      ps_single_quote(&resolved),
      ps_single_quote(&format!("Opened {}", trimmed))
    )
  } else {
    format!(
      "$ErrorActionPreference='Stop'; $irisArgs = @({}); Start-Process -FilePath {} -ArgumentList $irisArgs | Out-Null; Write-Output {}",
      arg_items.join(", "),
      ps_single_quote(&resolved),
      ps_single_quote(&format!("Opened {}", trimmed))
    )
  };

  let output = Command::new("powershell")
    .args(["-NoProfile", "-NonInteractive", "-Command", &script])
    .output()
    .map_err(|e| format!("Failed to launch desktop item: {}", e))?;
  if !output.status.success() {
    let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
    return Err(if err.is_empty() {
      format!("Failed to launch {}", trimmed)
    } else {
      format!("Failed to launch {}: {}", trimmed, err)
    });
  }

  let message = String::from_utf8_lossy(&output.stdout).trim().to_string();
  Ok(if message.is_empty() {
    format!("Opened {}", trimmed)
  } else {
    message
  })
}

// GPU detection struct
#[derive(serde::Serialize)]
pub struct GpuInfo {
  pub available: bool,
  pub name: Option<String>,
}

#[tauri::command]
pub fn check_gpu_available() -> Result<GpuInfo, String> {
  // Try nvidia-smi first
  let nvidia = std::process::Command::new("nvidia-smi")
    .args(["--query-gpu=name", "--format=csv,noheader"])
    .output();
  if let Ok(out) = nvidia {
    if out.status.success() {
      let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
      if !name.is_empty() {
        return Ok(GpuInfo { available: true, name: Some(name) });
      }
    }
  }
  // Fallback: PowerShell WMI check for non-virtual GPU
  let ps = std::process::Command::new("powershell")
    .args(["-Command", "Get-WmiObject Win32_VideoController | Where-Object { $_.Name -notmatch 'Basic|Virtual|Remote|Microsoft' } | Select-Object -First 1 -ExpandProperty Name"])
    .output();
  if let Ok(out) = ps {
    if out.status.success() {
      let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
      if !name.is_empty() {
        return Ok(GpuInfo { available: true, name: Some(name) });
      }
    }
  }
  Ok(GpuInfo { available: false, name: None })
}

#[tauri::command]
pub fn focus_window_by_title(query: String) -> Result<String, String> {
  let trimmed = query.trim();
  if trimmed.is_empty() {
    return Err("Window title or process name is required".to_string());
  }

  let script = format!(r#"
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win32Focus {{
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
}}
"@
$ErrorActionPreference = 'Stop'
$q = {query}
$windows = Get-Process | Where-Object {{ $_.MainWindowTitle -ne '' }} | Where-Object {{ $_.MainWindowTitle -like ('*' + $q + '*') -or $_.ProcessName -like ('*' + $q + '*') }}
$picked = $windows |
  Sort-Object `
    @{{ Expression = {{ if ($_.MainWindowTitle -ieq $q) {{ 0 }} elseif ($_.ProcessName -ieq $q) {{ 1 }} else {{ 2 }} }} }},
    @{{ Expression = {{ $_.MainWindowTitle.Length }} }} |
  Select-Object -First 1
if (-not $picked -or $picked.MainWindowHandle -eq 0) {{
  throw "No visible window matched '$q'"
}}
[Win32Focus]::ShowWindow($picked.MainWindowHandle, 9) | Out-Null
if (-not [Win32Focus]::SetForegroundWindow($picked.MainWindowHandle)) {{
  throw "Failed to focus matched window"
}}
Write-Output ($picked.ProcessName + ' :: ' + $picked.MainWindowTitle)
"#, query = ps_single_quote(trimmed));

  let output = Command::new("powershell")
    .args(["-NoProfile", "-NonInteractive", "-Command", &script])
    .output()
    .map_err(|e| format!("Failed to focus window: {}", e))?;
  if !output.status.success() {
    let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
    return Err(if err.is_empty() {
      format!("Failed to focus a window matching {}", trimmed)
    } else {
      format!("Failed to focus {}: {}", trimmed, err)
    });
  }

  let matched = String::from_utf8_lossy(&output.stdout).trim().to_string();
  Ok(if matched.is_empty() {
    format!("Focused {}", trimmed)
  } else {
    format!("Focused {}", matched)
  })
}

#[tauri::command]
pub fn launch_mcp_server(mcp_id: Option<String>, command: String, args: Vec<String>) -> Result<u32, String> {
  // Reject shell metacharacters to prevent injection.
  let dangerous: &[char] = &[';', '&', '|', '`', '$', '>', '<', '\n', '\r'];
  if command.chars().any(|c| dangerous.contains(&c)) {
    return Err("Unsafe characters in command".to_string());
  }
  for arg in &args {
    if arg.chars().any(|c| dangerous.contains(&c)) {
      return Err("Unsafe characters in args".to_string());
    }
  }
  let resolved = resolve_executable(&command).ok_or_else(|| "Missing command".to_string())?;
  let child = Command::new(&resolved)
    .args(&args)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|e| {
      if command.eq_ignore_ascii_case("uv") || command.eq_ignore_ascii_case("uv.exe") {
        format!("Failed to launch '{}': {}. Install uv (https://docs.astral.sh/uv/) or use the full path to uv.exe.", command, e)
      } else {
        format!("Failed to launch '{}': {}", command, e)
      }
    })?;
  let pid = child.id();
  if let Some(id) = mcp_id {
    let mut procs = mcp_launched_procs().lock().map_err(|_| "MCP process lock poisoned".to_string())?;
    procs.insert(id, child);
  }
  Ok(pid)
}

#[tauri::command]
pub fn stop_mcp_server(mcp_id: String) -> Result<bool, String> {
  // Stop stdio-connected session first, if present.
  if let Ok(mut sessions) = mcp_sessions().lock() {
    if let Some(mut sess) = sessions.remove(&mcp_id) {
      let _ = sess.child.kill();
      let _ = sess.child.wait();
      return Ok(true);
    }
  }

  // Stop launched detached process, if present.
  let mut procs = mcp_launched_procs().lock().map_err(|_| "MCP process lock poisoned".to_string())?;
  if let Some(mut child) = procs.remove(&mcp_id) {
    let _ = child.kill();
    let _ = child.wait();
    return Ok(true);
  }
  Ok(false)
}

fn connect_mcp_server_inner(
  mcp_id: String,
  target: String,
  connection_type: Option<String>,
  command: Option<String>,
  args: Option<Vec<String>>,
) -> Result<McpConnectResult, String> {
  let (parsed_type, parsed_cmd, parsed_args) = parse_mcp_target(&target);
  let mut ctype = connection_type.unwrap_or(parsed_type.clone());
  let hinted = command.clone().or(parsed_cmd.clone()).unwrap_or_else(|| target.trim().to_string());
  if ctype == "url" && !looks_like_http_url(&hinted) {
    ctype = "stdio".to_string();
  }

  if ctype == "url" {
    let target_url = command.or(parsed_cmd).unwrap_or_else(|| target.trim().to_string());
    if target_url.is_empty() {
      return Err("MCP URL target is empty".to_string());
    }
    let _ = mcp_http_request(
      &target_url,
      "initialize",
      serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": { "name": "Iris", "version": "0.1.0" }
      }),
    )?;
    return Ok(McpConnectResult { connected: true, pid: None, protocol: Some("http".to_string()) });
  }

  let cmd = command.or(parsed_cmd).ok_or_else(|| "MCP command is required for stdio connections".to_string())?;
  let argv = args.unwrap_or(parsed_args);
  let pid = ensure_stdio_initialized(&mcp_id, &cmd, &argv)?;
  // Retrieve the detected protocol so we can report it to the frontend.
  let protocol_str = mcp_sessions().lock().ok()
    .and_then(|s| s.get(&mcp_id).map(|sess| match sess.protocol { McpProtocol::Ndjson => "ndjson".to_string(), McpProtocol::Lsp => "lsp".to_string() }));
  Ok(McpConnectResult { connected: true, pid: Some(pid), protocol: protocol_str })
}

#[tauri::command]
pub async fn connect_mcp_server(
  mcp_id: String,
  target: String,
  connection_type: Option<String>,
  command: Option<String>,
  args: Option<Vec<String>>,
) -> Result<McpConnectResult, String> {
  let mcp_id_for_kill = mcp_id.clone();
  // Kill-timer: after 60 s kill the process by PID (no sessions lock needed).
  // This makes the blocked read_line return EOF, unblocking the spawn_blocking thread.
  let kill_handle = tokio::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    kill_mcp_by_pid_if_known(&mcp_id_for_kill);
    // Give the blocking thread 1 s to see EOF and exit, then clean up the dead entry.
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    kill_mcp_session_process(&mcp_id_for_kill);
  });
  let task = tokio::task::spawn_blocking(move || {
    connect_mcp_server_inner(mcp_id, target, connection_type, command, args)
  });
  let result = task.await.unwrap_or_else(|e| Err(format!("MCP connect task panicked: {}", e)));
  kill_handle.abort();
  result
}

fn mcp_list_tools_inner(
  mcp_id: String,
  target: String,
  connection_type: Option<String>,
  command: Option<String>,
  args: Option<Vec<String>>,
) -> Result<Vec<McpToolInfo>, String> {
  let (parsed_type, parsed_cmd, parsed_args) = parse_mcp_target(&target);
  let mut ctype = connection_type.unwrap_or(parsed_type.clone());
  let hinted = command.clone().or(parsed_cmd.clone()).unwrap_or_else(|| target.trim().to_string());
  if ctype == "url" && !looks_like_http_url(&hinted) {
    ctype = "stdio".to_string();
  }

  let raw_tools = if ctype == "url" {
    let target_url = command.or(parsed_cmd).unwrap_or_else(|| target.trim().to_string());
    mcp_http_request(&target_url, "tools/list", serde_json::json!({}))?
  } else {
    let cmd = command.or(parsed_cmd).ok_or_else(|| "MCP command is required for stdio connections".to_string())?;
    let argv = args.unwrap_or(parsed_args);
    let _ = ensure_stdio_initialized(&mcp_id, &cmd, &argv)?;
    let mut sessions = mcp_sessions().lock().map_err(|_| "MCP session lock poisoned".to_string())?;
    let session = sessions.get_mut(&mcp_id).ok_or_else(|| "MCP session missing".to_string())?;
    mcp_stdio_request(session, "tools/list", serde_json::json!({}))?
  };

  let tools_arr = raw_tools
    .get("tools")
    .and_then(|v| v.as_array())
    .cloned()
    .unwrap_or_default();

  let tools = tools_arr
    .into_iter()
    .map(|t| McpToolInfo {
      name: t.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
      description: t
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string(),
      input_schema: t
        .get("inputSchema")
        .cloned()
        .or_else(|| t.get("input_schema").cloned())
        .unwrap_or(Value::Null),
    })
    .filter(|t| !t.name.is_empty())
    .collect();

  Ok(tools)
}

#[tauri::command]
pub async fn mcp_list_tools(
  mcp_id: String,
  target: String,
  connection_type: Option<String>,
  command: Option<String>,
  args: Option<Vec<String>>,
) -> Result<Vec<McpToolInfo>, String> {
  let mcp_id_for_kill = mcp_id.clone();
  // Kill-timer: after 30 s, kill by PID (no sessions lock) to unblock the blocked read.
  let kill_handle = tokio::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    kill_mcp_by_pid_if_known(&mcp_id_for_kill);
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    kill_mcp_session_process(&mcp_id_for_kill);
  });
  let task = tokio::task::spawn_blocking(move || {
    mcp_list_tools_inner(mcp_id, target, connection_type, command, args)
  });
  let result = task.await.unwrap_or_else(|e| Err(format!("MCP list_tools task panicked: {}", e)));
  kill_handle.abort();
  result
}

#[tauri::command]
pub fn mcp_call_tool(
  mcp_id: String,
  target: String,
  connection_type: Option<String>,
  command: Option<String>,
  args: Option<Vec<String>>,
  tool_name: String,
  arguments: Value,
) -> Result<Value, String> {
  let (parsed_type, parsed_cmd, parsed_args) = parse_mcp_target(&target);
  let mut ctype = connection_type.unwrap_or(parsed_type.clone());
  let hinted = command.clone().or(parsed_cmd.clone()).unwrap_or_else(|| target.trim().to_string());
  if ctype == "url" && !looks_like_http_url(&hinted) {
    ctype = "stdio".to_string();
  }

  if ctype == "url" {
    let target_url = command.or(parsed_cmd).unwrap_or_else(|| target.trim().to_string());
    return mcp_http_request(
      &target_url,
      "tools/call",
      serde_json::json!({ "name": tool_name, "arguments": arguments }),
    );
  }

  let cmd = command.or(parsed_cmd).ok_or_else(|| "MCP command is required for stdio connections".to_string())?;
  let argv = args.unwrap_or(parsed_args);
  let _ = ensure_stdio_initialized(&mcp_id, &cmd, &argv)?;
  let mut sessions = mcp_sessions().lock().map_err(|_| "MCP session lock poisoned".to_string())?;
  let session = sessions.get_mut(&mcp_id).ok_or_else(|| "MCP session missing".to_string())?;
  mcp_stdio_request(
    session,
    "tools/call",
    serde_json::json!({ "name": tool_name, "arguments": arguments }),
  )
}

#[tauri::command]
pub fn set_setup_flags(app: tauri::AppHandle, args: SetupFlagsArgs) -> Result<SetupFlags, String> {
  let mut flags = read_setup_flags(&app);
  if let Some(v) = args.ollama_verified {
    flags.ollama_verified = v;
  }
  if let Some(v) = args.models_verified {
    flags.models_verified = v;
  }
  if let Some(v) = args.interpret_v2_enabled {
    flags.interpret_v2_enabled = v;
  }
  if let Some(v) = args.model_profile {
    let trimmed = v.trim();
    if !trimmed.is_empty() {
      flags.model_profile = trimmed.to_string();
    }
  }
  if let Some(v) = args.assistant_name {
    let trimmed = v.trim();
    if !trimmed.is_empty() {
      flags.assistant_name = trimmed.to_string();
    }
  }
  if let Some(v) = args.theme_color {
    let trimmed = v.trim();
    if !trimmed.is_empty() {
      flags.theme_color = trimmed.to_string();
    }
  }
  if let Some(v) = args.theme_preset {
    let trimmed = v.trim();
    if !trimmed.is_empty() {
      flags.theme_preset = trimmed.to_string();
    }
  }
  if let Some(v) = args.network_enabled {
    flags.network_enabled = v;
  }
  if let Some(v) = args.manual_download_url {
    flags.manual_download_url = v.trim().to_string();
  }
  if let Some(v) = args.release_notes_url {
    flags.release_notes_url = v.trim().to_string();
  }
  if let Some(v) = args.update_feed_url {
    flags.update_feed_url = v.trim().to_string();
  }
  if let Some(v) = args.auto_updates_enabled {
    flags.auto_updates_enabled = v;
  }
  if let Some(v) = args.repos_enabled {
    flags.repos_enabled = v;
  }
  if let Some(v) = args.mcp_enabled {
    flags.mcp_enabled = v;
  }
  if let Some(v) = args.desktop_tools_enabled {
    flags.desktop_tools_enabled = v;
  }
  if let Some(v) = args.universal_dataweb_enabled {
    flags.universal_dataweb_enabled = v;
  }
  if let Some(v) = args.color_mode {
    let trimmed = v.trim();
    if trimmed == "light" || trimmed == "dark" {
      flags.color_mode = trimmed.to_string();
    }
  }
  write_setup_flags(&app, &flags)?;
  Ok(flags)
}
