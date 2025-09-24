"use strict";
var __create = Object.create;
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __getProtoOf = Object.getPrototypeOf;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};
var __copyProps = (to, from, except, desc) => {
  if (from && typeof from === "object" || typeof from === "function") {
    for (let key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
  }
  return to;
};
var __toESM = (mod, isNodeMode, target) => (target = mod != null ? __create(__getProtoOf(mod)) : {}, __copyProps(
  // If the importer is in node compatibility mode or this is not an ESM
  // file that has been converted to a CommonJS file using a Babel-
  // compatible transform (i.e. "__esModule" has not been set), then set
  // "default" to the CommonJS "module.exports" for node compatibility.
  isNodeMode || !mod || !mod.__esModule ? __defProp(target, "default", { value: mod, enumerable: true }) : target,
  mod
));
var __toCommonJS = (mod) => __copyProps(__defProp({}, "__esModule", { value: true }), mod);

// src/extension.ts
var extension_exports = {};
__export(extension_exports, {
  activate: () => activate,
  deactivate: () => deactivate
});
module.exports = __toCommonJS(extension_exports);
var vscode3 = __toESM(require("vscode"));

// src/webviews/ChatPanel.ts
var vscode = __toESM(require("vscode"));
var ChatPanel = class _ChatPanel {
  constructor(panel) {
    this.panel = panel;
    this.panel.onDidDispose(() => _ChatPanel.current = void 0);
    this.panel.webview.html = this.html();
  }
  static current;
  static viewType = "irisChat";
  static open(_extensionUri) {
    if (_ChatPanel.current) {
      _ChatPanel.current.panel.reveal();
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      _ChatPanel.viewType,
      "Iris \u2022 Chat",
      vscode.ViewColumn.Two,
      { enableScripts: true }
    );
    _ChatPanel.current = new _ChatPanel(panel);
  }
  html() {
    const nonce = String(Date.now());
    return `<!doctype html>
<html><head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy"
content="default-src 'none'; img-src https: data:; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';">
<title>Iris Chat</title>
</head>
<body style="font-family: system-ui, sans-serif; padding: 8px;">
  <h3 style="margin:0 0 8px;">Iris (local-first)</h3>
  <div id="log" style="height:60vh;border:1px solid #444;padding:8px;overflow:auto;"></div>
  <form id="f" style="margin-top:8px;display:flex;gap:8px;">
    <input id="q" type="text" placeholder="Ask Iris\u2026" style="flex:1;padding:6px;">
    <button type="submit">Send</button>
  </form>
  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();
    const log = document.getElementById('log');
    const f = document.getElementById('f');
    const q = document.getElementById('q');
    let ellipsisInterval = null;
    f.addEventListener('submit', (e) => {
      e.preventDefault();
      const t = q.value.trim();
      if (!t) return;
      log.innerHTML += '<div><b>You:</b> ' + t + '</div>';
      // Add animated ellipsis
      const ellipsisId = 'iris-ellipsis-' + Date.now();
      log.innerHTML += '<div id="' + ellipsisId + '"><b>Iris:</b> <span class="iris-ellipsis">...</span></div>';
      log.scrollTop = log.scrollHeight;
      let dots = 0;
      ellipsisInterval = setInterval(() => {
        dots = (dots + 1) % 4;
        const el = document.getElementById(ellipsisId)?.querySelector('.iris-ellipsis');
        if (el) el.textContent = '.'.repeat(dots || 1);
      }, 400);
      window.currentIrisEllipsisId = ellipsisId;
      vscode.postMessage({ type: 'chat', text: t, ellipsisId });
      q.value = '';
    });
    window.addEventListener('message', (e) => {
      if (e.data?.type === 'stream') {
        // Update ellipsis div with full text for smooth character-by-character updates
        const ellipsisId = window.currentIrisEllipsisId;
        const ellipsisDiv = document.getElementById(ellipsisId);
        if (ellipsisDiv) {
          const span = ellipsisDiv.querySelector('.iris-ellipsis');
          if (span) {
            const newText = e.data.text || '';
            if (span.textContent !== newText) {
              span.textContent = newText;
            }
          }
        }
      } else if (e.data?.type === 'reply') {
        // Remove ellipsis if present and add final text
        const ellipsisId = window.currentIrisEllipsisId;
        const ellipsisDiv = document.getElementById(ellipsisId);
        if (ellipsisDiv) ellipsisDiv.remove();
        if (ellipsisInterval) { clearInterval(ellipsisInterval); ellipsisInterval = null; }
        log.innerHTML += '<div><b>Iris:</b> ' + e.data.text + '</div>';
        log.scrollTop = log.scrollHeight;
        window.currentIrisEllipsisId = null;
      }
    });
  </script>
</body></html>`;
  }
};

// src/webviews/IrisChatTerminal.ts
var vscode2 = __toESM(require("vscode"));
var IrisChatTerminal = class _IrisChatTerminal {
  constructor(panel) {
    this.panel = panel;
    this.panel.onDidDispose(() => _IrisChatTerminal.current = void 0);
    this.panel.webview.html = this.html();
    this.panel.webview.onDidReceiveMessage(async (msg) => {
      if (msg?.type === "chat") {
        const q = String(msg.text || "");
        if (q.startsWith("/help")) {
          this.panel.webview.postMessage({ type: "reply", text: "Commands: /help, /docs <q>, /explain <text>" });
        } else {
          try {
            const http = require("http");
            const ollamaUrl = "http://localhost:11434/api/generate";
            const body = JSON.stringify({
              model: "qwen2.5-coder-lite",
              prompt: q,
              stream: true
            });
            const req = http.request(ollamaUrl, {
              method: "POST",
              headers: {
                "Content-Type": "application/json",
                "Content-Length": Buffer.byteLength(body)
              }
            }, (res) => {
              let buffer = "";
              let fullText = "";
              res.on("data", (chunk) => {
                buffer += chunk.toString();
                let lines = buffer.split("\n");
                buffer = lines.pop() || "";
                for (const line of lines) {
                  if (!line.trim()) {
                    continue;
                  }
                  try {
                    const json = JSON.parse(line);
                    if (json.response) {
                      fullText += json.response;
                      this.panel.webview.postMessage({ type: "stream", text: fullText });
                    }
                  } catch (e) {
                  }
                }
              });
              res.on("end", () => {
                this.panel.webview.postMessage({ type: "reply", text: fullText || "[No response]" });
              });
            });
            req.on("error", (err) => {
              this.panel.webview.postMessage({ type: "reply", text: "Ollama connection error: " + err.message });
            });
            req.write(body);
            req.end();
          } catch (err) {
            this.panel.webview.postMessage({ type: "reply", text: "Ollama error: " + err.message });
          }
        }
      }
    });
  }
  static current;
  static viewType = "irisChatTerminal";
  static open(extensionUri) {
    if (_IrisChatTerminal.current) {
      _IrisChatTerminal.current.panel.reveal();
      return;
    }
    const panel = vscode2.window.createWebviewPanel(
      _IrisChatTerminal.viewType,
      "Iris \u2022 Chat Terminal",
      { viewColumn: vscode2.ViewColumn.Beside, preserveFocus: false },
      {
        enableScripts: true,
        retainContextWhenHidden: true
      }
    );
    panel.iconPath = vscode2.Uri.joinPath(extensionUri, "media", "iris.png");
    _IrisChatTerminal.current = new _IrisChatTerminal(panel);
  }
  html() {
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Iris Chat Terminal</title>
  <style>
    body { background: #18181a; color: #eee; font-family: 'Segoe UI', system-ui, sans-serif; margin: 0; height: 100vh; display: flex; flex-direction: column; }
    .chat-header { text-align: center; padding: 32px 0 8px; }
    .chat-header img { width: 64px; height: 64px; border-radius: 50%; background: #222; }
    .chat-history { flex: 1; overflow-y: auto; padding: 24px 0 16px 0; display: flex; flex-direction: column; gap: 16px; }
    .bubble { max-width: 70%; padding: 12px 18px; border-radius: 18px; margin: 0 16px; font-size: 1.08em; word-break: break-word; }
    .bubble.user { background: #2d2d30; align-self: flex-end; color: #fff; border-bottom-right-radius: 4px; }
    .bubble.iris { background: #23232b; align-self: flex-start; color: #b6e3ff; border-bottom-left-radius: 4px; }
    .chat-input-row { display: flex; align-items: center; padding: 16px; background: #23232b; border-top: 1px solid #222; }
    .chat-input { flex: 1; padding: 10px 14px; border-radius: 8px; border: none; background: #19191c; color: #eee; font-size: 1em; }
    .chat-input:focus { outline: 2px solid #4e9eff; }
    .send-btn { margin-left: 12px; padding: 10px 18px; border-radius: 8px; border: none; background: #4e9eff; color: #fff; font-weight: bold; cursor: pointer; font-size: 1em; }
    .send-btn:active { background: #357fd6; }
    .error { color: #ff6b6b; margin: 0 16px; }
  </style>
</head>
<body>
  <div class="chat-header">
    <img src="https://raw.githubusercontent.com/KennyCrowPixels/Iris_for_Godot/main/media/iris.png" alt="Iris Logo">
  </div>
  <div class="chat-history" id="history"></div>
  <form class="chat-input-row" id="chat-form" autocomplete="off">
    <input class="chat-input" id="chat-input" type="text" placeholder="Send a message" autocomplete="off" />
    <button class="send-btn" type="submit">Send</button>
  </form>
  <script>
    const vscode = acquireVsCodeApi();
    const history = document.getElementById('history');
    const form = document.getElementById('chat-form');
    const input = document.getElementById('chat-input');
    let lastIrisMsg = null;
    form.addEventListener('submit', (e) => {
      e.preventDefault();
      const text = input.value.trim();
      if (!text) return;
      appendMsg('user', text);
      input.value = '';
      lastIrisMsg = appendMsg('iris', '...');
      vscode.postMessage({ type: 'chat', text });
    });
    function appendMsg(role, text) {
      const div = document.createElement('div');
      div.className = 'bubble ' + role;
      div.innerText = text;
      history.appendChild(div);
      history.scrollTop = history.scrollHeight;
      return div;
    }
    window.addEventListener('message', (e) => {
      if (e.data?.type === 'stream') {
        if (lastIrisMsg) lastIrisMsg.innerText = e.data.text;
      } else if (e.data?.type === 'reply') {
        if (lastIrisMsg) lastIrisMsg.innerText = e.data.text;
        lastIrisMsg = null;
      }
    });
  </script>
</body>
</html>`;
  }
};

// src/extension.ts
function activate(context) {
  const openIrisChatTerminal = vscode3.commands.registerCommand("iris.openChatTerminal", () => {
    IrisChatTerminal.open(context.extensionUri);
  });
  const openChat = vscode3.commands.registerCommand("iris.openChat", () => {
    vscode3.window.showInformationMessage("Try the new Iris Chat Terminal for a better experience!");
    ChatPanel.open(context.extensionUri);
  });
  const toggleOnline = vscode3.commands.registerCommand("iris.toggleOnline", async () => {
    const cfg = vscode3.workspace.getConfiguration("iris");
    const current = cfg.get("onlineAssistance", false) ?? cfg.get("iris.onlineAssistance", false);
    await cfg.update("iris.onlineAssistance", !current, vscode3.ConfigurationTarget.Workspace);
    vscode3.window.showInformationMessage(`Iris online assistance: ${!current ? "ON" : "OFF"}`);
  });
  context.subscriptions.push(openChat, openIrisChatTerminal, toggleOnline);
}
function deactivate() {
}
// Annotate the CommonJS export names for ESM import in node:
0 && (module.exports = {
  activate,
  deactivate
});
//# sourceMappingURL=extension.js.map
