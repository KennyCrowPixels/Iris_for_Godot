# Iris Companion (Android)

A thin-client Android companion for the Iris desktop app. The desktop does 100% of the LLM
inference, tool execution, and state management; the phone only renders the chat and talks to the
desktop over your Wi‑Fi (LAN). Speech-to-text and text-to-speech run entirely on the phone's native
OS APIs, so no audio is ever sent over the network and the desktop GPU stays free for inference.

## What it does

- **Zero-config discovery**: finds the desktop over mDNS (`_iris-companion._tcp`) on the same Wi‑Fi,
  or you can type the host IP and port manually.
- **PIN pairing**: enter the 6-digit PIN shown in Iris → Settings → Server to receive a session token.
- **Chat**: submit turns, see replies, attach photos (resized + JPEG-compressed on the phone first).
- **Voice**: mic dictation (auto-stops after 10s of silence) and spoken replies using the phone's
  native voices; starting the mic or pressing stop cancels any active speech.

## Prerequisites

- Node 18+ and the desktop Iris app running with the companion server enabled
  (Settings → Server → "Allow companion devices on this Wi‑Fi" + optionally "Auto-discovery").
- Android Studio + SDK (for a local dev build) or an Expo EAS account (for a cloud build).

## Run it

Because discovery (`react-native-zeroconf`) and on-device speech recognition
(`expo-speech-recognition`) use native modules, this app needs a **dev build** — it does not run in
plain Expo Go. TTS, image attach, and manual-IP connection do work in Expo Go.

```bash
cd iris-android-client
npm install

# One-time native project generation:
npx expo prebuild --platform android

# Build + run on a connected device/emulator (needs Android Studio/SDK):
npx expo run:android
# …or build a dev client in the cloud:
#   npx eas build -p android --profile development
```

Then: on the phone, pick the discovered desktop (or enter its LAN IP + port `1435`), type the PIN
from Iris, and start chatting.

## Notes

- The desktop and phone must be on the **same Wi‑Fi network** (this is LAN discovery, not Bluetooth).
- The desktop Iris window must be open — it confirms each remote turn and shows tool approvals there.
- Keep "Allow companion devices" off when you're on an untrusted network.
