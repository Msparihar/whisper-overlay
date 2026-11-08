# Overlay capture harness

This documentation-only Vite app imports the production `App` and waveform component. It replaces only `@tauri-apps/api/event` with timed `hotkey-press`, `hotkey-release`, and `transcribe-done` events so the real UI can be captured outside Tauri.

```bash
npm install
npx playwright install chromium
npm run capture
```

The capture script uses headless Chromium with its fake microphone source, takes the PNG frames, and asks ffmpeg to create `docs/overlay-demo.gif`. The harness is not included in the Tauri build.
