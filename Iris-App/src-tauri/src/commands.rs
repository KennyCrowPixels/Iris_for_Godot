use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;


use reqwest;
use serde_json;
use reqwest::blocking::Client;
use serde_json::Value;
use std::time::Duration;
use serde::{Deserialize, Serialize};
use tauri::Manager;

// Extended ChatMessage with a unix timestamp (defaults to 0 when missing on disk)
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ChatMessage {
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub time: i64, // unix seconds
}



use std::{fs, path::PathBuf};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

static OLLAMA_START_ATTEMPTED: AtomicBool = AtomicBool::new(false);

const OLLAMA_BASE: &str = "http://127.0.0.1:11434";
// <-- your custom tag created via `ollama create iris-organizer -f ...`
const MODEL_TAG: &str = "iris-organizer:latest";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Artifact { pub lang: String, pub filename: Option<String>, pub content: String, pub ts: i64 }

#[derive(Serialize, Deserialize, Clone)]
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
    pub title: String,
    pub messages: Vec<ChatMessage>,
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


fn atomic_write_json(path: &PathBuf, json: &str) -> Result<(), String> {
    let mut tmp = path.clone();
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    tmp.set_extension(format!("json.tmp.{nanos}"));

    {
        let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(json.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn now_ts() -> i64 {
  SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

fn load_tab(app: &tauri::AppHandle, tab_id: u32) -> Result<TabMemory, String> {
  let p = tab_file(app, tab_id)?;
  if !p.exists() {
    return Ok(TabMemory { tab_id, ..Default::default() });
  }
  let s = fs::read_to_string(p).map_err(|e| e.to_string())?;
  serde_json::from_str(&s).map_err(|e| e.to_string())
}

fn save_tab(app: &tauri::AppHandle, mem: &TabMemory) -> Result<(), String> {
    let p = tab_file(app, mem.tab_id)?;
    println!("Saving tab memory to: {:?}", p); // <-- Add this line
    let s = serde_json::to_string_pretty(mem).map_err(|e| e.to_string())?;
    atomic_write_json(&p, &s)
}

#[tauri::command]
pub fn show_devtools(app: tauri::AppHandle) -> Result<(), String> {
    // for Tauri 2, we can get the main window and open devtools on its webview
    if let Some(window) = app.get_webview_window("main") {
        window.open_devtools();
        Ok(())
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
  // naive token estimate
  let budget = token_budget;

  // pick last up-to-2 artifacts
  let mut arts = mem.artifacts.clone();
  arts.sort_by_key(|a| a.ts);
  let recent_artifacts: Vec<Artifact> = arts.into_iter().rev().take(2).collect();

  // reserve ~300 tokens for micro + artifacts
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

  Ok(CompiledContext {
    micro_summary: mem.micro_summary.clone(),
    dialogue_bullets: mem.dialogue_bullets.clone(),
    recent_transcript,
    recent_artifacts,
  })
}

#[tauri::command]
pub fn close_tab_and_snapshot(app: tauri::AppHandle, tab_id: u32, mut snapshot: Snapshot) -> Result<(), String> {
  let now = now_ts();
  if snapshot.last_updated.unwrap_or(0) == 0 {
    snapshot.last_updated = Some(now);
  }
  for m in &mut snapshot.messages {
    if m.time == 0 {
      m.time = now;
    }
  }

  let lf = iris_last_closed_file(&app)?;
  iris_write_snapshot_file(&lf, &snapshot)?;

  if let Ok(dir) = iris_open_tabs_dir(&app) {
    let p = dir.join(format!("tab_{}.json", tab_id));
    let _ = remove_file(&p);
  }

  Ok(())
}

#[tauri::command]
// AFTER (normalize timestamps and return Snapshot without tab_id)
pub fn restore_last_closed_tab(app: tauri::AppHandle) -> Result<Snapshot, String> {
  let lf = iris_last_closed_file(&app)?;
  let s = fs::read_to_string(&lf).map_err(|e| e.to_string())?;

  // Try parse as Snapshot first, else fall back to TabMemory mapping
  if let Ok(mut snap) = serde_json::from_str::<Snapshot>(&s) {
    let now = now_ts();
    if snap.last_updated.unwrap_or(0) == 0 { snap.last_updated = Some(now); }
    for m in &mut snap.messages { if m.time == 0 { m.time = now; } }
    return Ok(snap);
  }

  let mem: TabMemory = serde_json::from_str(&s).map_err(|e| e.to_string())?;
  let now = now_ts();
  let mut snap = Snapshot {
    title: mem.title.clone(),
    messages: mem.messages.clone(),
    micro_summary: mem.micro_summary.clone(),
    dialogue_bullets: mem.dialogue_bullets.clone(),
    summary: mem.summary.clone(),
    artifacts: mem.artifacts.clone(),
    last_updated: Some(mem.last_updated),
  };
  for m in &mut snap.messages { if m.time == 0 { m.time = now; } }
  Ok(snap)
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
                let _ = Command::new("ollama")
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
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;

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
            // pull without streaming to avoid long status spam
            let pull = client
                .post("http://127.0.0.1:11434/api/pull")
                .json(&serde_json::json!({ "name": name, "stream": false }))
                .send()
                .map_err(|e| e.to_string())?;

            if !pull.status().is_success() {
                return Err(format!("Model pull failed: {}", pull.status()));
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
pub fn update_snapshot_memory(
    app: tauri::AppHandle,
    args: UpdateSnapshotMemoryArgs,
) -> Result<(), String> {
    let mut snapshot = args.snapshot;
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
    title: mem.title.clone(),
    messages: mem.messages.clone(),
    micro_summary: mem.micro_summary.clone(),
    dialogue_bullets: mem.dialogue_bullets.clone(),
    summary: mem.summary.clone(),
    artifacts: mem.artifacts.clone(),
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
      title: mem.title.clone(),
      messages: mem.messages.clone(),
      micro_summary: mem.micro_summary.clone(),
      dialogue_bullets: mem.dialogue_bullets.clone(),
      summary: mem.summary.clone(),
      artifacts: mem.artifacts.clone(),
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