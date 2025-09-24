
import * as vscode from 'vscode';
import { ChatPanel } from './webviews/ChatPanel';
import { IrisChatTerminal } from './webviews/IrisChatTerminal';
import * as https from 'https';
import * as http from 'http';

export function activate(context: vscode.ExtensionContext) {


  // New always-open Iris chat terminal
  const openIrisChatTerminal = vscode.commands.registerCommand('iris.openChatTerminal', () => {
    IrisChatTerminal.open(context.extensionUri);
  });

  // Keep the old chat for now, but recommend the new one
  const openChat = vscode.commands.registerCommand('iris.openChat', () => {
    vscode.window.showInformationMessage('Try the new Iris Chat Terminal for a better experience!');
    ChatPanel.open(context.extensionUri);
  });

  const toggleOnline = vscode.commands.registerCommand('iris.toggleOnline', async () => {
    const cfg = vscode.workspace.getConfiguration('iris');
    const current = cfg.get<boolean>('onlineAssistance', false)
      ?? cfg.get<boolean>('iris.onlineAssistance', false); // tolerate either key
    await cfg.update('iris.onlineAssistance', !current, vscode.ConfigurationTarget.Workspace);
    vscode.window.showInformationMessage(`Iris online assistance: ${!current ? 'ON' : 'OFF'}`);
  });

  context.subscriptions.push(openChat, openIrisChatTerminal, toggleOnline);
}

export function deactivate() {}
