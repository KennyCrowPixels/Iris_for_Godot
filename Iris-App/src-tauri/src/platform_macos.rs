use std::{fs, path::PathBuf, process::Command};
use base64::Engine;
use serde_json::Value;

fn run_javascript(script: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new("/usr/bin/osascript").args(["-l", "JavaScript", "-e", script]).args(args)
        .output().map_err(|err| format!("macOS automation unavailable: {err}"))?;
    if !output.status.success() {
        return Err(format!("macOS denied automation. Check System Settings > Privacy & Security > Accessibility and Automation. {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn windows() -> Result<Value, String> {
    let raw = run_javascript(r#"
ObjC.import('CoreGraphics');
var windows = ObjC.deepUnwrap($.CGWindowListCopyWindowInfo(1, 0));
JSON.stringify(windows.filter(function(item) { return item.kCGWindowLayer === 0 && item.kCGWindowOwnerName; }).map(function(item) {
  return { id: item.kCGWindowNumber, title: item.kCGWindowName || item.kCGWindowOwnerName, processName: item.kCGWindowOwnerName };
}));
"#, &[])?;
    serde_json::from_str(&raw).map_err(|err| format!("Could not decode macOS windows: {err}"))
}

pub fn screenshot(window_id: Option<u32>) -> Result<String, String> {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let path = std::env::temp_dir().join(format!("iris_capture_{}_{nonce}.png",std::process::id()));
    let mut command = Command::new("/usr/sbin/screencapture");
    command.args(["-x","-t","png"]);
    if let Some(id) = window_id { command.arg("-l").arg(id.to_string()); }
    let output = command.arg(&path).output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        let _ = fs::remove_file(&path);
        return Err(format!("Screen capture failed. Grant Iris Screen Recording access in System Settings > Privacy & Security, then restart Iris. {}",String::from_utf8_lossy(&output.stderr).trim()));
    }
    let bytes = fs::read(&path).map_err(|err|err.to_string());
    let _ = fs::remove_file(&path);
    let bytes = bytes?;
    if bytes.len()<32 {return Err("Screen capture returned no image".into());}
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

pub fn focus(query: &str) -> Result<String, String> {
    run_javascript(r#"function run(args) {
  var system = Application('System Events');
  var names = system.applicationProcesses.whose({backgroundOnly:false}).name();
  var query = args[0].toLowerCase();
  var found = names.filter(function(name) { return name.toLowerCase().indexOf(query) !== -1; });
  if (found.length !== 1) throw Error('Expected one application match; list windows and use its exact application name.');
  Application(found[0]).activate();
  return 'Focused ' + found[0];
}"#, &[query])
}

pub fn launch(target: &str, args: &[String]) -> Result<String, String> {
    if target.trim().is_empty() {return Err("Application or path required".into());}
    let mut command = Command::new("/usr/bin/open");
    if PathBuf::from(target).exists() || target.starts_with("https://") || target.starts_with("http://") {
        command.arg("--").arg(target);
    } else {
        command.arg("-a").arg(target);
    }
    if !args.is_empty() {command.arg("--args").args(args);}
    let output = command.output().map_err(|err|err.to_string())?;
    if !output.status.success() {return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());}
    Ok(format!("Opened {target}"))
}

pub fn ollama_candidates(home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/Applications/Ollama.app/Contents/Resources/ollama"),PathBuf::from("/opt/homebrew/bin/ollama"),PathBuf::from("/usr/local/bin/ollama")];
    if let Some(home) = home {paths.insert(0,home.join("Applications/Ollama.app/Contents/Resources/ollama"));}
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ollama_discovery_includes_user_apps_and_both_homebrew_prefixes() {
        let paths = ollama_candidates(Some(PathBuf::from("/Users/test")));
        assert_eq!(paths[0],PathBuf::from("/Users/test/Applications/Ollama.app/Contents/Resources/ollama"));
        assert!(paths.contains(&PathBuf::from("/opt/homebrew/bin/ollama")));
        assert!(paths.contains(&PathBuf::from("/usr/local/bin/ollama")));
    }
}