use super::*;
use crate::agent_runtime::excerpt;
use base64::Engine as _;
use sha2::{Digest, Sha256};

// Local speech-to-text with whisper.cpp. On first use Iris downloads the official prebuilt engine
// (Windows) and an English model, verifies both against pinned SHA-256 hashes, then transcribes
// recordings captured by the app entirely on this machine.

const MODEL_FILE: &str = "ggml-base.en-q5_1.bin";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en-q5_1.bin";
const MODEL_SHA256: &str = "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f";
#[cfg(target_os = "windows")]
const ENGINE_URL: &str = "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip";
#[cfg(target_os = "windows")]
const ENGINE_SHA256: &str = "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb";
const MAX_SECONDS: usize = 180;

static SETUP: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

fn speech_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|err| err.to_string())?.join("speech");
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir)
}

fn model_path(dir: &std::path::Path) -> PathBuf { dir.join(MODEL_FILE) }

/// The whisper.cpp command-line engine: the downloaded build on Windows, otherwise one installed
/// on the system (for example `brew install whisper-cpp` on macOS).
fn engine_path(dir: &std::path::Path) -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        let path = dir.join("engine").join("Release").join("whisper-cli.exe");
        return path.is_file().then_some(path);
    }
    ["/opt/homebrew/bin/whisper-cli", "/usr/local/bin/whisper-cli", "/usr/bin/whisper-cli"].iter()
        .map(PathBuf::from).find(|path| path.is_file())
        .or_else(|| std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|dir| dir.join("whisper-cli")).find(|path| path.is_file())))
}

fn missing_engine_hint() -> String {
    if cfg!(target_os = "macos") {
        "Voice input needs the whisper.cpp engine. Install it once with Homebrew: brew install whisper-cpp — then try the microphone again.".into()
    } else if cfg!(target_os = "windows") {
        "The speech engine is not installed yet.".into()
    } else {
        "Voice input needs whisper.cpp's whisper-cli on your PATH.".into()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechStatus { engine_ready: bool, model_ready: bool, can_download_engine: bool, hint: String }

#[tauri::command]
pub fn speech_status(app: tauri::AppHandle) -> Result<SpeechStatus, String> {
    let dir = speech_dir(&app)?;
    let engine_ready = engine_path(&dir).is_some();
    Ok(SpeechStatus {
        engine_ready, model_ready: model_path(&dir).is_file(), can_download_engine: cfg!(target_os = "windows"),
        hint: if engine_ready { String::new() } else { missing_engine_hint() },
    })
}

/// Streams a download to `target`, emitting progress, and installs it only if its SHA-256 matches.
async fn download_verified(window: &tauri::Window, url: &str, sha256: &str, target: &std::path::Path, label: &str) -> Result<(), String> {
    let client = reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(900))
        .build().map_err(|err| err.to_string())?;
    let mut response = client.get(url).send().await.map_err(|err| format!("{label} download failed: {err}"))?
        .error_for_status().map_err(|err| format!("{label} download failed: {err}"))?;
    let total = response.content_length().unwrap_or(0);
    let partial = target.with_extension("part");
    let mut file = fs::File::create(&partial).map_err(|err| err.to_string())?;
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut last_percent = u64::MAX;
    while let Some(chunk) = response.chunk().await.map_err(|err| format!("{label} download interrupted: {err}"))? {
        std::io::Write::write_all(&mut file, &chunk).map_err(|err| err.to_string())?;
        hasher.update(&chunk);
        received += chunk.len() as u64;
        let percent = if total > 0 { received * 100 / total } else { 0 };
        if percent != last_percent {
            last_percent = percent;
            let _ = window.emit("speech_setup_progress", serde_json::json!({"stage":label,"received":received,"total":total,"percent":percent}));
        }
    }
    drop(file);
    let digest = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    if digest != sha256 {
        let _ = fs::remove_file(&partial);
        return Err(format!("{label} failed its integrity check and was discarded."));
    }
    fs::rename(&partial, target).map_err(|err| err.to_string())
}

async fn ensure_ready(app: &tauri::AppHandle, window: &tauri::Window) -> Result<(PathBuf, PathBuf), String> {
    let _guard = SETUP.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
    let dir = speech_dir(app)?;
    #[cfg(target_os = "windows")]
    if engine_path(&dir).is_none() {
        let archive = dir.join("whisper-engine.zip");
        download_verified(window, ENGINE_URL, ENGINE_SHA256, &archive, "Speech engine").await?;
        let engine_dir = dir.join("engine");
        fs::create_dir_all(&engine_dir).map_err(|err| err.to_string())?;
        let output = background_command("tar").arg("-xf").arg(&archive).arg("-C").arg(&engine_dir).output()
            .map_err(|err| format!("Could not unpack the speech engine: {err}"))?;
        let _ = fs::remove_file(&archive);
        if !output.status.success() {
            return Err(format!("Could not unpack the speech engine: {}", String::from_utf8_lossy(&output.stderr).trim()));
        }
    }
    let engine = engine_path(&dir).ok_or_else(missing_engine_hint)?;
    let model = model_path(&dir);
    if !model.is_file() {
        download_verified(window, MODEL_URL, MODEL_SHA256, &model, "Speech model").await?;
    }
    let _ = window.emit("speech_setup_progress", serde_json::json!({"stage":"ready","percent":100}));
    Ok((engine, model))
}

#[tauri::command]
pub async fn speech_prepare(app: tauri::AppHandle, window: tauri::Window) -> Result<(), String> {
    ensure_ready(&app, &window).await.map(|_| ())
}

fn wav_bytes(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples { bytes.extend_from_slice(&sample.to_le_bytes()); }
    bytes
}

/// Joins whisper's output lines and drops non-speech markers such as [BLANK_AUDIO] or (music).
fn clean_transcript(raw: &str) -> String {
    let marker = Regex::new(r"\[[^\]]*\]|\([^)]*\)|\*[^*]*\*").expect("valid transcript marker pattern");
    marker.replace_all(raw, " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

#[tauri::command]
pub async fn speech_transcribe(app: tauri::AppHandle, window: tauri::Window, pcm_base64: String, sample_rate: u32) -> Result<String, String> {
    if sample_rate != 16_000 { return Err("Recordings must be 16 kHz mono.".into()); }
    let pcm = base64::engine::general_purpose::STANDARD.decode(pcm_base64.as_bytes()).map_err(|err| format!("Invalid recording data: {err}"))?;
    let samples: Vec<i16> = pcm.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect();
    if samples.len() < (sample_rate as usize) / 4 { return Ok(String::new()); }
    if samples.len() > sample_rate as usize * MAX_SECONDS { return Err(format!("Recordings are limited to {} minutes.", MAX_SECONDS / 60)); }
    let (engine, model) = ensure_ready(&app, &window).await?;
    let wav = std::env::temp_dir().join(format!("iris_dictation_{}_{}.wav", std::process::id(), now_ts()));
    fs::write(&wav, wav_bytes(&samples, sample_rate)).map_err(|err| err.to_string())?;
    let threads = std::thread::available_parallelism().map(|count| count.get()).unwrap_or(4).clamp(2, 8).to_string();
    let wav_for_run = wav.clone();
    let outcome = tokio::time::timeout(Duration::from_secs(120), tokio::task::spawn_blocking(move || {
        background_command(&engine)
            .args(["-l", "en", "-nt", "-np", "-t", &threads, "--prompt", "Iris, Godot, GDScript, scene, node, MCP."])
            .arg("-m").arg(&model).arg("-f").arg(&wav_for_run)
            .output()
    })).await;
    let _ = fs::remove_file(&wav);
    let output = outcome.map_err(|_| "Transcription took too long and was stopped.".to_string())?
        .map_err(|err| err.to_string())?
        .map_err(|err| format!("Could not start the speech engine: {err}"))?;
    if !output.status.success() {
        return Err(format!("The speech engine failed: {}", excerpt(String::from_utf8_lossy(&output.stderr).trim(), 300)));
    }
    Ok(clean_transcript(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_describes_16khz_mono_pcm() {
        let bytes = wav_bytes(&[0, 1000, -1000], 16_000);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 6);
        assert_eq!(bytes.len(), 50);
    }

    #[test]
    fn transcript_markers_are_removed() {
        assert_eq!(clean_transcript(" Hello there.\n [BLANK_AUDIO]\n How are you? (music)\n"), "Hello there. How are you?");
        assert_eq!(clean_transcript("[BLANK_AUDIO]"), "");
    }
}
