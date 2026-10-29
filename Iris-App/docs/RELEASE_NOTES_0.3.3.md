# Iris 0.3.3 Alpha

This release adds two major expansion tools — a **two-way Voice Mode** and an **Android remote companion** with zero-config secure networking — while keeping Iris local-first. The desktop still does 100% of inference, tool execution, and state; the phone is a thin mirror. Application version `0.3.3-alpha.0`; Windows MSI package version `0.3.3.0`.

## Voice Mode (desktop)

- A microphone button in the composer dictates your message (speech-to-text) and auto-stops after 10 seconds of silence or when clicked again.
- Turns started by voice have Iris's final reply read aloud (text-to-speech) once the turn completes.
- Starting the mic or pressing stop immediately cancels any active speech. Uses only built-in browser speech APIs — no new dependencies — and degrades gracefully where a webview lacks them.

## Android Remote Companion (new `iris-android-client/`)

A native-feeling Expo/React Native chat app (Android-only) that mirrors Iris over your Wi‑Fi:

- **Zero-config discovery**: the desktop advertises itself via mDNS (`_iris-companion._tcp`); the phone finds it automatically on the same network, or you can enter the host IP + port manually.
- **PIN pairing**: a 6-digit PIN shown on the desktop is exchanged once for a session token (constant-time check, attempt lockout, browser-origin rejection). The PIN is never broadcast.
- **Thin-client chat**: submit turns, see replies, and toggle web access per turn. All inference runs on the desktop.
- **Mobile Voice Mode**: speech-to-text and text-to-speech run on the phone's native OS APIs — no audio ever crosses the network, keeping the desktop GPU free. 10-second silence auto-stop and speak-on-reply, with the same interrupt safety.
- **Efficient images**: photos are resized (max 1280px) and JPEG-compressed on the phone before sending, so only small payloads travel over Wi‑Fi — never raw camera data.

## Desktop companion server (Controller)

- The existing controller server was extended (not replaced): optional LAN binding (`0.0.0.0`), mDNS advertising, a pairing PIN, and public `/v1/ping` + `/v1/pair` bootstrap routes. Existing bearer-token auth, read/control scopes, jobs, and the desktop-approval handshake are unchanged.
- Settings → Server now has: enable + LAN toggle, mDNS discovery toggle, live LAN IP, and the pairing PIN with a rotate button.
- Turn submissions now accept attached images (bounded count/size) and raise the request body limit accordingly.

## Secure remote-roaming roadmap

Local-first is preserved. The escalation path is: (1) LAN + PIN today; (2) a user-run WireGuard/Tailscale tunnel for off-network access with no cloud broker seeing content; (3) future peer-to-peer WebRTC data channels (DTLS, STUN for NAT traversal, TURN only as an encrypted fallback) with a content-blind signaling relay. Inference and data never leave the desktop.

## Verification and honest limits

Verified: 82 Rust tests (including mDNS advertise/discover and pairing), 31 frontend tests, the production build, and a live check (capital answered directly; current Durham weather with the real temperature and source in one ~2.5s lookup; game query routed to web search). The mDNS discovery test passes on a real LAN.

The Android client is a complete, type-correct scaffold. Because it uses native modules (zeroconf discovery, on-device speech recognition), it runs from a dev build (`npx expo prebuild` + `expo run:android`, or EAS) rather than plain Expo Go; `iris-android-client/README.md` has the steps. Full end-to-end phone↔desktop runtime testing is pending on a physical Android device. These Alpha builds are not code-signed/notarized.

## Install (desktop)

- Windows x64: `iris-app_0.3.3-alpha.0_x64-setup.exe` (recommended) or the `.msi`.
- Apple Silicon Mac: `iris-app_0.3.3-alpha.0_aarch64.dmg`.
- `SHA256SUMS.txt` and `release-artifacts.json` record the verified hashes, source revision, and CI run.

Close Iris before installing, then reopen your existing chat. This update does not require a memory reset. To use the phone companion, enable Settings → Server → "Allow companion devices on this Wi‑Fi".
