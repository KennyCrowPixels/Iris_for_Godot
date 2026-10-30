# Iris Controller API

Settings > Server controls an opt-in REST API for trusted local agents, tools and future companion clients. This is not an MCP server. An MCP adapter or native mobile client can call the same versioned HTTP endpoints.

## Start and Pair

1. Keep Iris open and select Settings > Server.
2. Choose a free port, normally 1435. Read access is required; enable control only for trusted clients.
3. Start the server and copy its session token into the client. Never paste the token into chat prompts or commit it to source control.
4. Send `Authorization: Bearer <session-token>` with every request, including health checks.

The listener binds only to `127.0.0.1`. Another machine must use an authenticated encrypted tunnel or a properly secured HTTPS reverse proxy. Do not expose this plain HTTP listener directly on the Internet. Native clients are supported; browser-origin requests are rejected and no permissive CORS policy is installed.

Tokens are generated per server session, kept in memory, and revoked on rotation, stop or app exit. Rotating a token or stopping the server also requests cancellation of active remote jobs. Already-started external effects cannot be undone. The server does not automatically restart with Iris.

## Endpoints

| Method | Path | Permission | Result |
| --- | --- | --- | --- |
| GET | `/v1/health` | Valid token | Service version and enabled scopes |
| GET | `/v1/chats` | Read | Open chat ids, titles and project associations |
| POST | `/v1/chats` | Control | Create a dedicated chat; eight-chat limit applies |
| GET | `/v1/chats/{id}` | Read | Saved transcript and agent task state |
| POST | `/v1/chats/{id}/turns` | Control | Submit one turn and receive a job id |
| GET | `/v1/jobs/{id}` | Read | Job status and task progress/result |
| POST | `/v1/jobs/{id}/cancel` | Control | Request cancellation |

Create a chat:

```json
{"title":"Controller workspace"}
```

Submit a turn:

```json
{"input":"Summarize the linked project without making changes.","networkAllowed":false}
```

The turn endpoint returns HTTP 202 with `jobId`, `tabId` and `statusUrl`. Poll that status URL with the same bearer token, then fetch the chat transcript when the job finishes. This first API version uses job polling, not SSE/WebSocket streaming. Unknown body fields are rejected, requests are capped at 64 KB, and prompt text is limited to 16,000 characters.

Read/control scopes do not bypass tool permissions. Network defaults to off per request and still requires the global Network setting. Project associations are managed locally in Iris. The API does not expose arbitrary Tauri commands, direct shell execution, global settings changes or automatic permission approval. Remote turns wait for the desktop UI to attach its stream and permission listeners before execution. If the UI is busy/unavailable, the job is blocked rather than running without its approval surface.

Only one remote turn runs at a time. HTTP 401 means missing/invalid credentials or a rejected Origin; 403 means the requested scope is disabled; 409 indicates a busy chat or capacity limit. Job records are in-memory and bounded; transcripts and agent task notes use normal Iris persistence.

## Development Check

The Windows native harness can verify the API without printing credentials:

```sh
node tests/desktop-smoke.mjs --tab 1 --controller
```

It requires the temporary WebView2 debug setup documented in [AGENT_RUNTIME_REVIEW.md](AGENT_RUNTIME_REVIEW.md), uses ports 1446/1447, refuses to disturb an already-running controller, and cleans up its own server/chat. Disable debugging after the test. Native macOS and remote tunnel/mobile-client interoperability still require on-device testing.