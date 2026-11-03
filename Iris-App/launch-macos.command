#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.local/share/iris/node/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export OLLAMA_HOST="127.0.0.1:11434"
export OLLAMA_NUM_PARALLEL=1
export OLLAMA_MAX_LOADED_MODELS=1
if [ "$(uname -s)" != "Darwin" ]; then printf '%s\n' 'Use launch-dev.bat on Windows.' >&2; exit 1; fi
if [ ! -d node_modules ]; then printf '%s\n' 'Run bash setup-macos.sh first.' >&2; exit 1; fi
exec npm run tauri:dev