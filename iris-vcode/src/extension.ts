import * as vscode from 'vscode';
import { ChatPanel } from './webviews/ChatPanel';

export function activate(context: vscode.ExtensionContext) {

  const openChat = vscode.commands.registerCommand('iris.openChat', () => {
    ChatPanel.open(context.extensionUri);

    const panel = (ChatPanel as any).current?.panel as vscode.WebviewPanel | undefined;
    panel?.webview.onDidReceiveMessage(async (msg) => {
      if (msg?.type === 'chat') {
        const q = String(msg.text || '');
        if (q.startsWith('/help')) {
          panel.webview.postMessage({ type: 'reply', text: 'Commands: /help, /docs <q>, /explain <text>' });
        } else {
          panel.webview.postMessage({ type: 'reply', text: 'Echo: ' + q });
        }
      }
    });
  });

  const toggleOnline = vscode.commands.registerCommand('iris.toggleOnline', async () => {
    const cfg = vscode.workspace.getConfiguration('iris');
    const current = cfg.get<boolean>('onlineAssistance', false)
      ?? cfg.get<boolean>('iris.onlineAssistance', false); // tolerate either key
    await cfg.update('iris.onlineAssistance', !current, vscode.ConfigurationTarget.Workspace);
    vscode.window.showInformationMessage(`Iris online assistance: ${!current ? 'ON' : 'OFF'}`);
  });

  context.subscriptions.push(openChat, toggleOnline);
}

export function deactivate() {}
