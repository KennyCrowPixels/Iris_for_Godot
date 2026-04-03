use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use sysinfo::System;


use reqwest;
use serde_json;
use reqwest::blocking::Client;
use serde_json::Value;
use std::time::Duration;
use std::collections::{HashMap, HashSet, VecDeque};
use serde::{Deserialize, Serialize};
use tauri::{Manager, Emitter};

// Extended ChatMessage with a unix timestamp (defaults to 0 when missing on disk)
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub time: i64, // unix seconds
}



use std::{fs, path::PathBuf};
use std::io::{Write, Read, BufRead, BufReader};
use std::time::{SystemTime, UNIX_EPOCH};

static OLLAMA_START_ATTEMPTED: AtomicBool = AtomicBool::new(false);

const OLLAMA_BASE: &str = "http://127.0.0.1:11434";
// <-- your custom tag created via `ollama create iris-organizer -f ...`
const MODEL_TAG: &str = "iris-organizer:latest";

struct McpStdioSession {
  child: std::process::Child,
  stdin: std::process::ChildStdin,
  stdout: BufReader<std::process::ChildStdout>,
  next_id: u64,
  initialized: bool,
}

static MCP_STDIO_SESSIONS: OnceLock<Mutex<HashMap<String, McpStdioSession>>> = OnceLock::new();
static MCP_LAUNCHED_PROCS: OnceLock<Mutex<HashMap<String, std::process::Child>>> = OnceLock::new();

fn mcp_sessions() -> &'static Mutex<HashMap<String, McpStdioSession>> {
  MCP_STDIO_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mcp_launched_procs() -> &'static Mutex<HashMap<String, std::process::Child>> {
  MCP_LAUNCHED_PROCS.get_or_init(|| Mutex::new(HashMap::new()))
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
    ("stdio".to_string(), Some(parts[0].clone()), parts[1..].to_vec())
  }
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
    if c.eq_ignore_ascii_case("uv") {
      let mut candidates: Vec<std::path::PathBuf> = vec![];
      if let Ok(local) = std::env::var("LOCALAPPDATA") {
        candidates.push(std::path::Path::new(&local).join("Programs").join("uv").join("uv.exe"));
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

fn write_mcp_framed_json(stdin: &mut std::process::ChildStdin, value: &Value) -> Result<(), String> {
  let payload = serde_json::to_vec(value).map_err(|e| format!("encode failed: {}", e))?;
  let header = format!("Content-Length: {}\r\n\r\n", payload.len());
  stdin
    .write_all(header.as_bytes())
    .and_then(|_| stdin.write_all(&payload))
    .and_then(|_| stdin.flush())
    .map_err(|e| format!("write failed: {}", e))
}

fn read_mcp_framed_json(stdout: &mut BufReader<std::process::ChildStdout>) -> Result<Value, String> {
  let mut content_length: Option<usize> = None;
  loop {
    let mut line = String::new();
    let n = stdout.read_line(&mut line).map_err(|e| format!("read header failed: {}", e))?;
    if n == 0 {
      return Err("MCP stream ended while reading headers".to_string());
    }
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
      break;
    }
    if let Some((name, val)) = trimmed.split_once(':') {
      if name.trim().eq_ignore_ascii_case("Content-Length") {
        content_length = val.trim().parse::<usize>().ok();
      }
    }
  }
  let len = content_length.ok_or_else(|| "Missing Content-Length in MCP frame".to_string())?;
  let mut body = vec![0u8; len];
  stdout
    .read_exact(&mut body)
    .map_err(|e| format!("read body failed: {}", e))?;
  serde_json::from_slice::<Value>(&body).map_err(|e| format!("decode body failed: {}", e))
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
  write_mcp_framed_json(&mut session.stdin, &req)?;
  loop {
    let msg = read_mcp_framed_json(&mut session.stdout)?;
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
  write_mcp_framed_json(&mut session.stdin, &req)
}

fn ensure_stdio_session(mcp_id: &str, command: &str, args: &[String]) -> Result<u32, String> {
  let mut sessions = mcp_sessions().lock().map_err(|_| "MCP session lock poisoned".to_string())?;
  if let Some(existing) = sessions.get_mut(mcp_id) {
    if let Ok(None) = existing.child.try_wait() {
      return Ok(existing.child.id());
    }
  }

  let dangerous: &[char] = &[';', '&', '|', '`', '$', '>', '<', '\n', '\r'];
  if command.chars().any(|c| dangerous.contains(&c)) {
    return Err("Unsafe characters in command".to_string());
  }
  for arg in args {
    if arg.chars().any(|c| dangerous.contains(&c)) {
      return Err("Unsafe characters in args".to_string());
    }
  }

  let resolved = resolve_executable(command).ok_or_else(|| "Missing command".to_string())?;
  let mut child = Command::new(&resolved)
    .args(args)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|e| {
      if command.eq_ignore_ascii_case("uv") {
        format!("Failed to launch '{}': {}. Install uv or set MCP target command to the full uv.exe path.", command, e)
      } else {
        format!("Failed to launch '{}': {}", command, e)
      }
    })?;

  let pid = child.id();
  let stdin = child.stdin.take().ok_or_else(|| "Failed to open MCP stdin".to_string())?;
  let stdout = child.stdout.take().ok_or_else(|| "Failed to open MCP stdout".to_string())?;

  sessions.insert(
    mcp_id.to_string(),
    McpStdioSession {
      child,
      stdin,
      stdout: BufReader::new(stdout),
      next_id: 1,
      initialized: false,
    },
  );
  Ok(pid)
}

fn ensure_stdio_initialized(mcp_id: &str, command: &str, args: &[String]) -> Result<u32, String> {
  let pid = ensure_stdio_session(mcp_id, command, args)?;
  let mut sessions = mcp_sessions().lock().map_err(|_| "MCP session lock poisoned".to_string())?;
  let session = sessions.get_mut(mcp_id).ok_or_else(|| "MCP session missing".to_string())?;
  if !session.initialized {
    let _ = mcp_stdio_request(
      session,
      "initialize",
      serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": { "name": "Iris", "version": "0.1.0" }
      }),
    )?;
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
  #[serde(default, alias = "custom_enabled_models", alias = "customEnabledModels")]
  pub custom_enabled_models: Option<Vec<String>>,
  #[serde(default, alias = "organizer_dispatch_note", alias = "organizerDispatchNote")]
  pub organizer_dispatch_note: Option<String>,
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


fn atomic_write_json(path: &PathBuf, json: &str) -> Result<(), String> {
    let mut tmp = path.clone();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    tmp.set_extension(format!("json.tmp.{nanos}"));

    {
        let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().ok();
    }

    match fs::rename(&tmp, path) {
      Ok(_) => {}
      Err(rename_err) => {
        // Windows can fail rename when destination is briefly locked.
        // Fallback to copy+remove to keep writes resilient.
        fs::copy(&tmp, path).map_err(|copy_err| {
          format!("rename failed: {}; copy fallback failed: {}", rename_err, copy_err)
        })?;
        let _ = fs::remove_file(&tmp);
      }
    }
    Ok(())
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
  format!("{}…", &collapsed[..max.saturating_sub(1)])
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
  contains_any(&t, &["what was", "what is", "what joke", "first joke", "last joke", "just told", "earlier", "previous", "remember", "topic", "joke did you tell"])
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

fn should_emit_bridge(primary_intent: &str, pressure: f32, mem: &TabMemory) -> bool {
  if primary_intent != "banter_roleplay" || pressure <= 0.58 {
    return false;
  }
  let streak = banter_streak(mem);
  if streak < 3 {
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
    "You are Iris, a disciplined but personable local development assistant. Interpret first, answer clearly, and preserve project continuity while staying natural.".to_string()
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
}

fn default_true_flag() -> bool { true }

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RepoProjectStore {
  pub repos: Vec<RepoFolder>,
  #[serde(default)]
  pub mcps: Vec<McpConnection>,
  pub projects: Vec<ProjectDef>,
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
  Ok(scan_repo_entries_internal(&root, 2000))
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
  pub day_label: String,
  pub summary: String,
  pub source_url: String,
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

async fn geocode_weather_location(client: &reqwest::Client, raw_location: &str) -> Result<(f64, f64, String), String> {
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
      .query(&[("name", query.as_str()), ("count", "1"), ("language", "en"), ("format", "json")])
      .send()
      .await
      .map_err(|e| format!("Weather geocoding failed: {}", e))?;

    let geo_json: Value = geo_resp.json().await.map_err(|e| format!("Invalid geocoding response: {}", e))?;
    if let Some(result) = geo_json
      .get("results")
      .and_then(|v| v.as_array())
      .and_then(|arr| arr.first())
    {
      let lat = result.get("latitude").and_then(|v| v.as_f64()).ok_or_else(|| "Missing latitude".to_string())?;
      let lon = result.get("longitude").and_then(|v| v.as_f64()).ok_or_else(|| "Missing longitude".to_string())?;
      let city = result.get("name").and_then(|v| v.as_str()).unwrap_or(trimmed);
      let admin1 = result.get("admin1").and_then(|v| v.as_str()).unwrap_or("");
      let country = result.get("country").and_then(|v| v.as_str()).unwrap_or("");
      let location_label = [city, admin1, country]
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(", ");
      return Ok((lat, lon, location_label));
    }
  }

  let nominatim_resp = client
    .get("https://nominatim.openstreetmap.org/search")
    .header("User-Agent", "iris-app-weather/1.0")
    .query(&[("q", normalized_us.as_str()), ("format", "jsonv2"), ("limit", "1")])
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
  let label = fallback
    .get("display_name")
    .and_then(|v| v.as_str())
    .unwrap_or(trimmed)
    .to_string();
  Ok((lat, lon, label))
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

  let (lat, lon, location_label) = geocode_weather_location(&client, q).await?;

  let forecast_resp = client
    .get("https://api.open-meteo.com/v1/forecast")
    .query(&[
      ("latitude", lat.to_string()),
      ("longitude", lon.to_string()),
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
  let summary = format!(
    "{} with a high around {:.0} F, a low around {:.0} F, and about a {}% chance of precipitation.",
    weather_code_label(code),
    max_temp,
    min_temp,
    precip_prob
  );

  Ok(WeatherLookupResponse {
    location: location_label,
    day_label: day_label.to_string(),
    summary,
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

#[tauri::command]
pub async fn network_search(query: String, project_context: Option<String>) -> Result<NetworkSearchResponse, String> {
  let q = query.trim();
  if q.is_empty() {
    return Ok(NetworkSearchResponse::default());
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
  hits.truncate(8);

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
      // Convert Snapshot → TabMemory for callers that expect TabMemory
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
  let custom_enabled_models = args.custom_enabled_models.clone().unwrap_or_default();
  let should_use_coder = coder_enabled && (use_coder_force
    || is_coder_intent_text(&args.user_text)
    || (has_active_artifact && is_edit_followup_text(&args.user_text)));
  let model = if should_use_coder { "iris-coder:latest".to_string() } else { "iris-organizer:latest".to_string() };
  let strategy = resolve_primary_strategy(&primary_intent, &deterministic_reply);

  let lower_text = args.user_text.to_lowercase();
  let needs_vision = vision_enabled && contains_any(&lower_text, &[
    "image", "screenshot", "ui", "vision", "photo", "picture", "diagram", "look at this"
  ]);

  // Planner route always starts with organizer and ends with summarizer; middle stages are capability-driven.
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
  routed_models.push("iris-summarizer".to_string());
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
      "{}\n\nConversation memory:\n{}\n\n{}\n\nActive artifact:\n{}\n\nUser request:\n{}\n\n{}\nRules:\n- Return a single fenced code block.\n- If editing prior code, modify the active artifact unless user asks for a rewrite.",
      persona,
      if compiled.recent_transcript.is_empty() { "(empty)" } else { &compiled.recent_transcript },
      lane_block,
      active_art,
      args.user_text,
      godot_hint
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
      "{}\n\nConversation memory:\n{}\n\n{}\n\n{}\n\nUser request:\n{}\n\n{}{}\n{}\n{}\n{}\n{}\n{}\nOutput style:\n- Natural, direct prose by default.\n- Do not output scaffolding labels like TODO, PLAN, User request, Assumptions, or Sanity check unless explicitly requested.\n- Never invent project facts, files, features, previous decisions, or completed work. If unknown, say you do not have that project detail yet and ask one short clarifying question.\n- Keep concise unless user asks for depth.",
      persona,
      if compiled.recent_transcript.is_empty() { "(empty)" } else { &compiled.recent_transcript },
      lane_block,
      last_number_hint,
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
            "keep_alive": "90s"   // ← unload after 90s of inactivity
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

  // Fallback: older TabMemory format — convert into Snapshot (and rewrite file)
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

  // Neither format parsed — return combined error messages for debugging
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

    // MediumHigh: 8 GB VRAM + 32 GB RAM — Developer Baseline (7B models, 3-4K context)
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
    "coder" => "You are Iris-Coder. Produce correct, minimal, runnable code.\n- Default to the shown language/framework.\n- When the user asks for code, output ONLY one fenced code block with the correct language tag. No plans or extra text.\n- For edits, return the full revised file inside the single code fence.\n- Validation first: imports, paths, interfaces.\n- No chain-of-thought.",
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
  } else if vram_gb >= 15.5 {
    (
      "Ultra",
      format!("{:.1} GB VRAM (via {}) ≥ 16 GB — Ultra tier.", vram_gb, probe_source),
    )
  } else if vram_gb >= 11.5 {
    (
      "High",
      format!("{:.1} GB VRAM (via {}) ≥ 12 GB — High tier.", vram_gb, probe_source),
    )
  } else if vram_gb >= 7.5 && ram_gb >= 31.0 {
    (
      "MediumHigh",
      format!(
        "{:.1} GB VRAM + {:.1} GB RAM (via {}) — Developer Baseline (Medium-High).",
        vram_gb, ram_gb, probe_source
      ),
    )
  } else if vram_gb >= 5.5 && ram_gb >= 15.0 {
    (
      "Medium",
      format!(
        "{:.1} GB VRAM + {:.1} GB RAM (via {}) — Medium tier.",
        vram_gb, ram_gb, probe_source
      ),
    )
  } else if vram_gb <= 4.5 || ram_gb < 15.0 {
    (
      "Low",
      format!(
        "{:.1} GB VRAM, {:.1} GB RAM (via {}) — Low tier.",
        vram_gb, ram_gb, probe_source
      ),
    )
  } else {
    (
      "Medium",
      format!(
        "{:.1} GB VRAM, {:.1} GB RAM (via {}) — defaulting to Medium.",
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

fn modelfile_display_name(filename: &str) -> &'static str {
  if filename.contains("organizer") { "Organizer" }
  else if filename.contains("coder") { "Coder" }
  else if filename.contains("summarizer") { "Summarizer" }
  else if filename.contains("vision") { "Vision" }
  else { "" }
}

// ---------------------------------------------------------------------------
// ModelConfig — per-model enable flags, custom models, and organizer notes
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
  let path = modelfile_path(Some(&app), fname)?;
  if path.exists() {
    return Err(format!("File {} already exists", fname));
  }
  let safe_nick = nickname.trim();
  let content = format!(
    "# NICKNAME: {}\nFROM llama3.2:3b\n\nSYSTEM \"\"\"\nYou are a helpful assistant.\n\"\"\"\n\nPARAMETER num_ctx 2048\nPARAMETER num_keep 24\nPARAMETER num_predict 512\nPARAMETER temperature 0.3\n",
    safe_nick
  );
  fs::write(&path, content).map_err(|e| format!("Failed to create {}: {}", fname, e))
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
  let path = modelfile_path(Some(&app), fname)?;
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
    if name.ends_with(".txt") || name.ends_with(".modelfile") {
      names.push(name);
    }
  }
  names.sort();
  Ok(names)
}

#[tauri::command]
pub fn read_modelfile_data(app: tauri::AppHandle, filename: String) -> Result<ModelfileData, String> {
  let path = modelfile_path(Some(&app), &filename)?;
  let raw = fs::read_to_string(&path)
    .map_err(|e| format!("Cannot read {}: {}", filename, e))?;

  let mut from_model = String::new();
  let mut params: Vec<ModelfileParam> = Vec::new();
  let mut system_lines: Vec<String> = Vec::new();
  let mut in_system = false;
  let mut nickname = String::new();

  for line in raw.lines() {
    let trimmed = line.trim();
    if trimmed.starts_with("# NICKNAME:") && nickname.is_empty() {
      nickname = trimmed.trim_start_matches("# NICKNAME:").trim().to_string();
      continue;
    }
    if trimmed.starts_with("FROM ") && from_model.is_empty() {
      from_model = trimmed.trim_start_matches("FROM ").trim().to_string();
    } else if trimmed.starts_with("SYSTEM \"\"\"") {
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
    } else if trimmed.starts_with("PARAMETER ") {
      let rest = trimmed.trim_start_matches("PARAMETER ").trim();
      if let Some(space_pos) = rest.find(' ') {
        let key = rest[..space_pos].trim().to_string();
        let value = rest[space_pos..].trim().to_string();
        params.push(ModelfileParam { key, value });
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

  let dir = path.parent().ok_or_else(|| "No parent dir".to_string())?;
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_nanos();
  let tmp_path = dir.join(format!(".iris_tmp_{}.txt", nanos));
  fs::write(&tmp_path, out_lines.join("\n"))
    .map_err(|e| format!("Failed to write temp file: {}", e))?;
  fs::rename(&tmp_path, &path).map_err(|e| {
    let _ = fs::remove_file(&tmp_path);
    format!("Failed to rename to {}: {}", path.display(), e)
  })?;

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
pub fn take_screenshot() -> Result<String, String> {
  let script = r#"
Add-Type -AssemblyName System.Windows.Forms,System.Drawing
try {
  $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $bmp = [System.Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen([System.Drawing.Point]::Empty, [System.Drawing.Point]::Empty, $bounds.Size)
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
}}
"@
try {{
  $p = Get-Process -Id {window_id} -ErrorAction Stop
  if ($p -and $p.MainWindowHandle -ne 0) {{
    [Win32]::ShowWindow($p.MainWindowHandle, 9) | Out-Null
    [Win32]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    Start-Sleep -Milliseconds 220
  }}
  $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
  $bmp = [System.Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen([System.Drawing.Point]::Empty, [System.Drawing.Point]::Empty, $bounds.Size)
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
      if command.eq_ignore_ascii_case("uv") {
        format!("Failed to launch '{}': {}. Install uv or set MCP target to the full uv.exe path.", command, e)
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

#[tauri::command]
pub fn connect_mcp_server(
  mcp_id: String,
  target: String,
  connection_type: Option<String>,
  command: Option<String>,
  args: Option<Vec<String>>,
) -> Result<McpConnectResult, String> {
  let (parsed_type, parsed_cmd, parsed_args) = parse_mcp_target(&target);
  let ctype = connection_type.unwrap_or(parsed_type);

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
    return Ok(McpConnectResult { connected: true, pid: None });
  }

  let cmd = command.or(parsed_cmd).ok_or_else(|| "MCP command is required for stdio connections".to_string())?;
  let argv = args.unwrap_or(parsed_args);
  let pid = ensure_stdio_initialized(&mcp_id, &cmd, &argv)?;
  Ok(McpConnectResult { connected: true, pid: Some(pid) })
}

#[tauri::command]
pub fn mcp_list_tools(
  mcp_id: String,
  target: String,
  connection_type: Option<String>,
  command: Option<String>,
  args: Option<Vec<String>>,
) -> Result<Vec<McpToolInfo>, String> {
  let (parsed_type, parsed_cmd, parsed_args) = parse_mcp_target(&target);
  let ctype = connection_type.unwrap_or(parsed_type);

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
  let ctype = connection_type.unwrap_or(parsed_type);

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
