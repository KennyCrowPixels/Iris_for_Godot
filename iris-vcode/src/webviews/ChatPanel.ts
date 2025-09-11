import * as vscode from 'vscode';

export class ChatPanel {
  static current: ChatPanel | undefined;
  static readonly viewType = 'irisChat';

  static open(_extensionUri: vscode.Uri) {
    if (ChatPanel.current) { ChatPanel.current.panel.reveal(); return; }
    const panel = vscode.window.createWebviewPanel(
      ChatPanel.viewType, 'Iris • Chat', vscode.ViewColumn.Two, { enableScripts: true }
    );
    ChatPanel.current = new ChatPanel(panel);
  }

  private constructor(public panel: vscode.WebviewPanel) {
    this.panel.onDidDispose(() => ChatPanel.current = undefined);
    this.panel.webview.html = this.html();
  }

  private html() {
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
    <input id="q" type="text" placeholder="Ask Iris…" style="flex:1;padding:6px;">
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
}
