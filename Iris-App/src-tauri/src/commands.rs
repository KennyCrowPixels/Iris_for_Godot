use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;


use reqwest;
use serde_json;
use reqwest::blocking::Client;
use serde_json::Value;
use std::time::Duration;
use serde::{Deserialize, Serialize};

// Minimal fallback for ChatMessage if not imported from elsewhere
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct ChatMessage {
    role: String,
    text: String,
}



use std::{fs, path::PathBuf};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

static OLLAMA_START_ATTEMPTED: AtomicBool = AtomicBool::new(false);

const OLLAMA_BASE: &str = "http://127.0.0.1:11434";
// <-- your custom tag created via `ollama create iris-organizer -f ...`
const MODEL_TAG: &str = "iris-organizer:latest";

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Artifact { pub lang: String, pub filename: Option<String>, pub content: String, pub ts: i64 }

#[derive(Serialize, Deserialize, Clone)]
pub struct Msg { pub role: String, pub text: String, pub ts: i64 }

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct TabMemory {
    pub tab_id: u32,
    pub title: String,
    pub messages: Vec<Msg>,
    pub artifacts: Vec<Artifact>,
    pub micro_summary: String,
    pub dialogue_bullets: String,
    pub summary: String,
    #[serde(default)]
    pub is_closed: bool,
    #[serde(default)]
    pub last_updated: i64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CompiledContext {
    pub micro_summary: String,
    pub dialogue_bullets: String,
    pub recent_transcript: String,
    pub recent_artifacts: Vec<Artifact>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Snapshot {
    pub tab_id: u32,
    pub title: String,
    pub messages: Vec<Msg>,
    pub micro_summary: String,
    pub dialogue_bullets: String,
    pub summary: String,
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub last_updated: i64,
}

// ========== helpers ==========

fn dev_memory_dir() -> Result<PathBuf, String> {
    let mut dir = std::env::current_dir().map_err(|e| e.to_string())?;
    if dir.ends_with("src-tauri") { dir.pop(); }
    let dir = dir.join("iris_memory");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn prod_memory_dir(_app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let exe_dir = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or("could not get exe dir")?;
    let dir = exe_dir.join("iris_memory");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn memory_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if cfg!(debug_assertions) { dev_memory_dir() } else { prod_memory_dir(app) }
}

fn tab_file(app: &tauri::AppHandle, tab_id: u32) -> Result<PathBuf, String> {
    Ok(memory_dir(app)?.join(format!("tab_{}.json", tab_id)))
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

// ========== commands ==========

#[tauri::command]
#[allow(non_snake_case)]
pub fn update_tab_memory(
    app: tauri::AppHandle,
    tabId: u32,
    summary: String,
    micro_summary: Option<String>,
    microSummary: Option<String>,
    dialogue_bullets: Option<String>,
    dialogueBullets: Option<String>,
    new_message: Option<String>,
    newMessage: Option<String>,
    artifacts: Vec<Artifact>,
) -> Result<(), String> {
    let micro_summary = microSummary.or(micro_summary)
        .ok_or("missing micro_summary/microSummary")?;
    let dialogue_bullets = dialogueBullets.or(dialogue_bullets)
        .ok_or("missing dialogue_bullets/dialogueBullets")?;
    let new_message = newMessage.or(new_message)
        .ok_or("missing new_message/newMessage")?;

    // Existing implementation, using coalesced locals:
    let mut mem = load_tab(&app, tabId)?;
    mem.tab_id = tabId;
    mem.summary = summary;
    mem.micro_summary = micro_summary;
    mem.dialogue_bullets = dialogue_bullets;
    mem.messages.push(Msg { role: "assistant".into(), text: new_message, ts: now_ts() });
    let ts = now_ts();
    let mut arts = artifacts;
    for a in arts.iter_mut() { a.ts = ts; }
    mem.artifacts.extend(arts);
    mem.last_updated = now_ts();
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
pub fn close_tab_and_snapshot(app: tauri::AppHandle, tab_id: u32) -> Result<(), String> {
  let mut mem = load_tab(&app, tab_id)?;
  mem.is_closed = true;
  mem.last_updated = now_ts();
  let dir = memory_dir(&app)?;
  let path1 = { let mut p = dir.clone(); p.push("last_closed_tab_1.json"); p };
  let path2 = { let mut p = dir.clone(); p.push("last_closed_tab_2.json"); p };
  let path3 = { let mut p = dir.clone(); p.push("last_closed_tab_3.json"); p };
  // rotate: _2 -> _3, _1 -> _2
  if path3.exists() { let _ = fs::remove_file(&path3); }
  if path2.exists() { fs::rename(&path2, &path3).map_err(|e| e.to_string())?; }
  if path1.exists() { fs::rename(&path1, &path2).map_err(|e| e.to_string())?; }
  // write new _1
  let s = serde_json::to_string_pretty(&mem).map_err(|e| e.to_string())?;
  atomic_write_json(&path1, &s)?;
  // delete tab file
  let tf = tab_file(&app, tab_id)?;
  if tf.exists() { let _ = fs::remove_file(tf); }
  Ok(())
}

#[tauri::command]
pub fn restore_last_closed_tab(app: tauri::AppHandle) -> Result<TabMemory, String> {
  let dir = memory_dir(&app)?;
  let path1 = { let mut p = dir.clone(); p.push("last_closed_tab_1.json"); p };
  let path2 = { let mut p = dir.clone(); p.push("last_closed_tab_2.json"); p };
  let path3 = { let mut p = dir.clone(); p.push("last_closed_tab_3.json"); p };

  if !path1.exists() { return Err("no recent closed tabs".into()); }

  let s = fs::read_to_string(&path1).map_err(|e| e.to_string())?;
  let mem: TabMemory = serde_json::from_str(&s).map_err(|e| e.to_string())?;

  // rotate down
  let _ = fs::remove_file(&path1);
  if path2.exists() { fs::rename(&path2, &path1).map_err(|e| e.to_string())?; }
  if path3.exists() { fs::rename(&path3, &path2).map_err(|e| e.to_string())?; }

  Ok(mem)
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
pub async fn list_open_tabs(app: tauri::AppHandle) -> Result<Vec<Snapshot>, String> {
    let dir = memory_dir(&app)?;
    let mut out = Vec::new();
    if !dir.exists() { return Ok(out); }

    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = match entry { Ok(e) => e, Err(_) => continue };
        let path = entry.path();

        let is_final = path.extension().and_then(|s| s.to_str()) == Some("json");
        let looks_tmp = path.extension().and_then(|s| s.to_str()).map(|x| x.contains(".tmp")).unwrap_or(false);
        if !is_final || looks_tmp { continue; }

        let name_ok = path.file_name().and_then(|s| s.to_str()).map_or(false, |n| n.starts_with("tab_"));
        if !name_ok { continue; }

        let s = match fs::read_to_string(&path) { Ok(s) => s, Err(_) => continue };
        let mem: Result<TabMemory, _> = serde_json::from_str(&s);
        let mem = match mem { Ok(m) => m, Err(_) => continue };

        if mem.is_closed { continue; }

        out.push(Snapshot {
            tab_id: mem.tab_id,
            title: mem.title.clone(),
            messages: mem.messages.clone(),
            micro_summary: mem.micro_summary.clone(),
            dialogue_bullets: mem.dialogue_bullets.clone(),
            summary: mem.summary.clone(),
            artifacts: mem.artifacts.clone(),
            last_updated: mem.last_updated,
        });
    }

    out.sort_by_key(|s| s.last_updated);
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
#[allow(non_snake_case)]
pub fn restore_full_tab_memory(
    app: tauri::AppHandle,
    tabId: u32,
    title: String,
    messages: Vec<ChatMessage>,
    artifacts: Vec<Artifact>,
    micro_summary: Option<String>,
    microSummary: Option<String>,
    dialogue_bullets: Option<String>,
    dialogueBullets: Option<String>,
    summary: String,
    last_updated: Option<i64>,
    lastUpdated: Option<i64>,
) -> Result<(), String> {
    let micro_summary = microSummary.or(micro_summary)
        .ok_or("missing micro_summary/microSummary")?;
    let dialogue_bullets = dialogueBullets.or(dialogue_bullets)
        .ok_or("missing dialogue_bullets/dialogueBullets")?;
    let last_updated = lastUpdated.or(last_updated).unwrap_or_else(now_ts);

    // Existing implementation, using coalesced locals:
    let mem = TabMemory {
        tab_id: tabId,
        title,
        messages: messages.into_iter().map(|m| Msg { role: m.role, text: m.text, ts: now_ts() }).collect(),
        artifacts,
        micro_summary,
        dialogue_bullets,
        summary,
        is_closed: false,
        last_updated,
    };
    save_tab(&app, &mem)
}


