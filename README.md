# Whisper Overlay

A lightweight desktop speech-to-text overlay for Windows. Hold a key, speak, release — transcribed text is typed into any active application.

Built with **Tauri v2**, **React**, and **Rust**.

## Download

**[Download latest release (Windows)](https://github.com/Msparihar/whisper-overlay/releases/latest)**

No installation needed — just download and run the exe.

## Features

- **Two transcription backends**
  - **Deepgram Nova-3** (default) — streaming via WebSocket, fast and accurate
  - **OpenAI Whisper** — batch API, reliable fallback
- **Hold-to-record** — Hold Left Alt, speak, release to transcribe
- **Types anywhere** — Transcribed text is typed into whatever app is focused
- **Live waveform overlay** — Visual feedback while recording
- **Processing animation** — Overlay stays visible during transcription
- **System tray controls** — Switch backend, microphone, and language from the tray
- **Tray state icons** — Gray (idle), Green (listening), Orange (transcribing)
- **Microphone selection** — Pick any connected input device
- **Language support** — English, Hindi, or auto-detect
- **Tech vocabulary boosting** — Optimized for developer terminology

## Requirements

You need at least one API key set in your environment or `~/.env.local`:

- `DEEPGRAM_API_KEY` — for Deepgram backend ([get one here](https://console.deepgram.com/))
- `OPENAI_API_KEY` — for Whisper backend ([get one here](https://platform.openai.com/api-keys))

Example `~/.env.local`:
```
DEEPGRAM_API_KEY=your_key_here
OPENAI_API_KEY=your_key_here
```

## Usage

1. Run `whisper-overlay.exe`
2. The app starts minimized to the system tray
3. **Hold Left Alt** to record, **release** to transcribe
4. Text is typed into the active window
5. Right-click the tray icon to switch backend, microphone, or language

## Build from Source

### Prerequisites

- [Rust](https://rustup.rs/)
- [Bun](https://bun.sh/)
- Windows 10/11 with WebView2 (included in Windows 11)

### Steps

```bash
git clone https://github.com/Msparihar/whisper-overlay.git
cd whisper-overlay
bun install
bun tauri build
```

The built exe is at `src-tauri/target/release/app.exe`.

## Tech Stack

| Layer | Tech |
|-------|------|
| Desktop framework | Tauri v2 |
| Frontend | React 19, TypeScript, Tailwind CSS v4 |
| Backend | Rust |
| Audio capture | Windows waveIn API (raw FFI) |
| Text input | Windows SendInput API (Unicode) |
| Transcription | Deepgram WebSocket / OpenAI Whisper API |
| Build tool | Vite 7, Bun |

## License

MIT
