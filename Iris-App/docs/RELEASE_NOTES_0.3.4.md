# Iris 0.3.4 Alpha

A reliability patch for the core agent loop: steadier **network research**, honest **desktop/MCP capability reporting**, clearer **weather** wording, and visible **feedback when launching a bridge**. No breaking changes; your existing chats, memory, and bridges are untouched. Application version `0.3.4-alpha.0`; Windows MSI package version `0.3.4.0`.

## Network research — stays on subject and keeps moving

- **Subject anchoring.** The investigation now locks onto the concrete named entity the moment a web lane opens, and that anchor is repeated in every discovery and review step. A follow-up like "what are the recent reviews for *it*" resolves "it" to the game you were actually discussing instead of drifting onto a different, similarly spelled title, and a descriptive word in the request (like "average" or "reviews") is never turned into a new proper name.
- **Reference resolution at intake.** Indirect references ("the game", "that show", "this book") are resolved to the exact name from the recent conversation before the query is formed. The place-copying rule that prevents weather mix-ups no longer blocks a legitimate research subject from being reused across turns.
- **Resume actually advances.** A source that already returned no usable content is refused on retry, so pressing **Resume** picks a *different* lead or runs a fresh query rather than replaying the same dead link.
- **Aggregator redirects are skipped.** Google News RSS redirect links (JavaScript/consent bounce pages with no readable article text) are no longer offered as read targets; Iris goes to the original publisher or searches again.

## Desktop & bridge tools — honest about capabilities

- **New "act" routing lane.** Requests to see the screen, take a screenshot, control an app, or run a linked bridge tool now keep their tools available instead of being misrouted to a tool-less text answer.
- **No more false "I'm just a text model."** When a capability is needed but its tool is off, Iris now says plainly which switch to flip (Desktop Tools, or link an MCP bridge/repository in Settings → Projects) instead of denying it can ever do it.
- **Smoother MCP inspection.** The tool-discovery page size was raised so the local model sees more of a bridge's tools at once, and the guidance now tells it to inspect with a read-effect call (no plan required) and to report a bridge error honestly rather than guessing a result.

## Weather — clearer summary

- The one-line summary now reads, e.g., *"Current weather in Durham, North Carolina: 48.2 F, clear, as of 14:05 local time."* The raw IANA timezone name and the confusing "daily highs and lows are forecasts" caveat were removed; the current reading is stated plainly with the place and local observation time.

## Bridge launch — visible feedback

- The **Launch** button now shows a "Launching…" spinner and disables while a stdio bridge connects, so the brief connect delay reads as intentional progress. The spinner is CSS-animated (compositor thread), so it keeps moving even during a momentary main-thread stall.

## Verification and honest limits

Verified: 82 Rust tests, 28 frontend tests, both type-checks, and the production build. The research, routing, and weather changes are prompt- and guard-level; they improve the small local model's behavior but do not make it infallible — a 3B model can still occasionally need a Resume. Deeper MCP-bridge reliability depends on the specific bridge and still benefits from on-device testing. These Alpha builds are not code-signed/notarized.

## Install (desktop)

- Windows x64: `iris-app_0.3.4-alpha.0_x64-setup.exe` (recommended) or the `.msi`.
- Apple Silicon Mac: `iris-app_0.3.4-alpha.0_aarch64.dmg`.
- `SHA256SUMS.txt` and `release-artifacts.json` record the verified hashes, source revision, and CI run.

Close Iris before installing, then reopen your existing chat. This update does not require a memory reset.
