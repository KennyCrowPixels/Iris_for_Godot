#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
MODE="dev"
PROFILE="balanced"
ASSUME_YES=0
CHECK_ONLY=0

usage() {
  printf '%s\n' 'Usage: bash setup-macos.sh [--dev|--build] [--models balanced|13b|14b|none] [--yes] [--check]'
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dev) MODE="dev" ;;
    --build) MODE="build" ;;
    --models) shift; PROFILE="${1:?--models needs balanced, 13b, 14b, or none}" ;;
    --yes) ASSUME_YES=1 ;;
    --check) CHECK_ONLY=1 ;;
    --help|-h) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
  shift
done
case "$PROFILE" in balanced|13b|14b|none) ;; *) usage; exit 2 ;; esac
if [ "$(uname -s)" != "Darwin" ]; then
  printf '%s\n' 'This launcher is for macOS. Use setup-windows.bat on Windows.' >&2
  exit 1
fi
ARCH="$(uname -m)"
case "$ARCH" in arm64) NODE_ARCH="arm64" ;; x86_64) NODE_ARCH="x64" ;; *) exit 1 ;; esac
export PATH="$HOME/.local/share/iris/node/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export OLLAMA_HOST="127.0.0.1:11434"
export OLLAMA_NUM_PARALLEL=1
export OLLAMA_MAX_LOADED_MODELS=1
cd "$ROOT"

find_ollama() {
  for candidate in "$HOME/Applications/Ollama.app/Contents/Resources/ollama" "/Applications/Ollama.app/Contents/Resources/ollama" "/opt/homebrew/bin/ollama" "/usr/local/bin/ollama"; do
    if [ -x "$candidate" ]; then printf '%s\n' "$candidate"; return 0; fi
  done
  command -v ollama
}

if [ "$CHECK_ONLY" -eq 1 ]; then
  xcode-select -p
  node --version
  cargo --version
  git --version
  OLLAMA_BIN="$(find_ollama)"
  "$OLLAMA_BIN" --version
  printf '%s\n' 'Prerequisite check passed. No installations or model downloads were performed.'
  exit 0
fi

if [ "$ASSUME_YES" -ne 1 ]; then
  printf '%s\n' 'Iris setup may install Node, Rust and Ollama in your user folders, npm dependencies, and the selected model profile.'
  printf '%s\n' 'Model downloads can exceed 10-20 GB. The 13B/14B profiles need additional memory and are optional.'
  read -r -p 'Continue? [y/N] ' answer
  case "$answer" in y|Y|yes|YES) ;; *) exit 0 ;; esac
fi

if ! xcode-select -p >/dev/null 2>&1; then
  printf '%s\n' 'Install Apple Command Line Tools with: xcode-select --install' 'Complete the Apple installer, then rerun this launcher.' >&2
  exit 1
fi
if ! xcrun --find clang >/dev/null 2>&1; then
  printf '%s\n' 'The Apple toolchain is not ready. Open Xcode or complete the Command Line Tools setup/license, then retry.' >&2
  exit 1
fi

TEMP="$(mktemp -d)"
trap 'rm -rf "$TEMP"' EXIT
NODE_OK=0
if command -v node >/dev/null 2>&1; then
  if node -e 'const [major,minor]=process.versions.node.split(".").map(Number); process.exit(major>22 || major===22 && minor>=12 || major===20 && minor>=19 ? 0 : 1)'; then NODE_OK=1; fi
fi
if [ "$NODE_OK" -eq 0 ]; then
  printf '%s\n' 'Installing a verified Node 22 LTS archive in ~/.local/share/iris/node...'
  curl --fail --location --retry 3 --proto '=https' --tlsv1.2 'https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt' -o "$TEMP/SHASUMS256.txt"
  ARCHIVE="$(awk -v suffix="darwin-${NODE_ARCH}.tar.gz" 'index($2,suffix) && substr($2,length($2)-length(suffix)+1)==suffix {print $2; exit}' "$TEMP/SHASUMS256.txt")"
  case "$ARCHIVE" in node-v22.*-darwin-*.tar.gz) ;; *) printf '%s\n' 'Could not resolve Node archive.' >&2; exit 1 ;; esac
  curl --fail --location --retry 3 --proto '=https' --tlsv1.2 "https://nodejs.org/dist/latest-v22.x/$ARCHIVE" -o "$TEMP/$ARCHIVE"
  (cd "$TEMP"; awk -v archive="$ARCHIVE" '$2==archive' SHASUMS256.txt | shasum -a 256 -c -)
  mkdir -p "$HOME/.local/share/iris/node"
  tar -xzf "$TEMP/$ARCHIVE" --strip-components=1 -C "$HOME/.local/share/iris/node"
fi

if ! command -v cargo >/dev/null 2>&1; then
  curl --fail --location --retry 3 --proto '=https' --tlsv1.2 https://sh.rustup.rs -o "$TEMP/rustup-init.sh"
  sh "$TEMP/rustup-init.sh" -y --profile minimal --default-toolchain stable --no-modify-path
fi
cargo --version
git --version

if ! OLLAMA_BIN="$(find_ollama)"; then
  printf '%s\n' 'Downloading the official Ollama application...'
  curl --fail --location --retry 3 --proto '=https' --tlsv1.2 https://ollama.com/download/Ollama-darwin.zip -o "$TEMP/Ollama.zip"
  ditto -xk "$TEMP/Ollama.zip" "$TEMP/ollama"
  codesign --verify --deep --strict "$TEMP/ollama/Ollama.app"
  spctl --assess --type execute "$TEMP/ollama/Ollama.app"
  mkdir -p "$HOME/Applications"
  ditto "$TEMP/ollama/Ollama.app" "$HOME/Applications/Ollama.app"
  OLLAMA_BIN="$(find_ollama)"
fi

if ! curl --fail --silent --max-time 2 http://127.0.0.1:11434/api/version >/dev/null; then
  mkdir -p "$HOME/Library/Logs/Iris"
  nohup "$OLLAMA_BIN" serve >"$HOME/Library/Logs/Iris/ollama.log" 2>&1 &
  ready=0
  for attempt in $(seq 1 30); do
    if curl --fail --silent --max-time 2 http://127.0.0.1:11434/api/version >/dev/null; then ready=1; break; fi
    sleep 1
  done
  if [ "$ready" -ne 1 ]; then printf '%s\n' 'Ollama did not start. See ~/Library/Logs/Iris/ollama.log.' >&2; exit 1; fi
fi

npm ci --no-audit --no-fund
npm run check
if [ "$PROFILE" != "none" ]; then
  node scripts/setup-models.mjs --profile "$PROFILE" --ollama "$OLLAMA_BIN"
fi
chmod +x launch-macos.command
if [ "$MODE" = "build" ]; then
  npm run tauri:build -- --bundles app,dmg
  printf '%s\n' "Bundles: $ROOT/src-tauri/target/release/bundle"
else
  printf '%s\n' 'Setup complete. Start Iris with: bash launch-macos.command'
fi