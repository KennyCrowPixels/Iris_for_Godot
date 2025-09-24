import * as vscode from 'vscode';

export class IrisChatTerminal {
  static current: IrisChatTerminal | undefined;
  static readonly viewType = 'irisChatTerminal';

  static open(extensionUri: vscode.Uri) {
    if (IrisChatTerminal.current) {
      IrisChatTerminal.current.panel.reveal();
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      IrisChatTerminal.viewType,
      'Iris • Chat Terminal',
      { viewColumn: vscode.ViewColumn.Beside, preserveFocus: false },
      {
        enableScripts: true,
        retainContextWhenHidden: true,
      }
    );
    panel.iconPath = vscode.Uri.joinPath(extensionUri, 'media', 'iris.png'); // Optional: add your logo
    IrisChatTerminal.current = new IrisChatTerminal(panel);
  }

  private constructor(public panel: vscode.WebviewPanel) {
    this.panel.onDidDispose(() => IrisChatTerminal.current = undefined);
    this.panel.webview.html = this.html();
    // Listen for chat messages
    this.panel.webview.onDidReceiveMessage(async (msg) => {
      if (msg?.type === 'chat') {
        const q = String(msg.text || '');
        if (q.startsWith('/help')) {
          this.panel.webview.postMessage({ type: 'reply', text: 'Commands: /help, /docs <q>, /explain <text>' });
        } else {
          // Call Ollama HTTP API with streaming
          try {
            const http = require('http');
            const ollamaUrl = 'http://localhost:11434/api/generate';
            const body = JSON.stringify({
              model: 'qwen2.5-coder-lite',
              prompt: q,
              stream: true
            });
            const req = http.request(ollamaUrl, {
              method: 'POST',
              headers: {
                'Content-Type': 'application/json',
                'Content-Length': Buffer.byteLength(body)
              }
            }, (res: any) => {
              let buffer = '';
              let fullText = '';
              res.on('data', (chunk: any) => {
                buffer += chunk.toString();
                let lines = buffer.split('\n');
                buffer = lines.pop() || '';
                for (const line of lines) {
                  if (!line.trim()) { continue; }
                  try {
                    const json = JSON.parse(line);
                    if (json.response) {
                      fullText += json.response;
                      this.panel.webview.postMessage({ type: 'stream', text: fullText });
                    }
                  } catch (e) {
                    // ignore parse errors for incomplete lines
                  }
                }
              });
              res.on('end', () => {
                this.panel.webview.postMessage({ type: 'reply', text: fullText || '[No response]' });
              });
            });
            req.on('error', (err: any) => {
              this.panel.webview.postMessage({ type: 'reply', text: 'Ollama connection error: ' + err.message });
            });
            req.write(body);
            req.end();
          } catch (err: any) {
            this.panel.webview.postMessage({ type: 'reply', text: 'Ollama error: ' + err.message });
          }
        }
      }
    });
  }

  private html() {
    // Modern chat UI inspired by Ollama/ChatGPT
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
}