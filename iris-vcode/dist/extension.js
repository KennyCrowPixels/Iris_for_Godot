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
var vscode2 = __toESM(require("vscode"));

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
    f.addEventListener('submit', (e) => {
      e.preventDefault();
      const t = q.value.trim();
      if (!t) return;
      log.innerHTML += '<div><b>You:</b> ' + t + '</div>';
      vscode.postMessage({ type: 'chat', text: t });
      q.value = '';
    });
    window.addEventListener('message', (e) => {
      if (e.data?.type === 'reply') {
        log.innerHTML += '<div><b>Iris:</b> ' + e.data.text + '</div>';
        log.scrollTop = log.scrollHeight;
      }
    });
  </script>
</body></html>`;
  }
};

// src/extension.ts
function activate(context) {
  const openChat = vscode2.commands.registerCommand("iris.openChat", () => {
    ChatPanel.open(context.extensionUri);
    const panel = ChatPanel.current?.panel;
    panel?.webview.onDidReceiveMessage(async (msg) => {
      if (msg?.type === "chat") {
        const q = String(msg.text || "");
        if (q.startsWith("/help")) {
          panel.webview.postMessage({ type: "reply", text: "Commands: /help, /docs <q>, /explain <text>" });
        } else {
          panel.webview.postMessage({ type: "reply", text: "Echo: " + q });
        }
      }
    });
  });
  const toggleOnline = vscode2.commands.registerCommand("iris.toggleOnline", async () => {
    const cfg = vscode2.workspace.getConfiguration("iris");
    const current = cfg.get("onlineAssistance", false) ?? cfg.get("iris.onlineAssistance", false);
    await cfg.update("iris.onlineAssistance", !current, vscode2.ConfigurationTarget.Workspace);
    vscode2.window.showInformationMessage(`Iris online assistance: ${!current ? "ON" : "OFF"}`);
  });
  context.subscriptions.push(openChat, toggleOnline);
}
function deactivate() {
}
// Annotate the CommonJS export names for ESM import in node:
0 && (module.exports = {
  activate,
  deactivate
});
//# sourceMappingURL=extension.js.map
