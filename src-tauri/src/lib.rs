use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};
use tauri::{
    image::Image,
    menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder},
    tray::TrayIconBuilder,
    Emitter, Manager,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
};

const VK_LMENU: i32 = 0xA4;
const SAMPLE_RATE: u32 = 16000;
const CHANNELS: u16 = 1;
const BITS_PER_SAMPLE: u16 = 16;
const AVG_BYTES_PER_SEC: u32 = SAMPLE_RATE * (BITS_PER_SAMPLE as u32 / 8) * CHANNELS as u32;
const MIN_DURATION_SECS: f64 = 0.3;

mod vocab;
mod deepgram;

// ── Backend enum ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Backend {
    Deepgram,
    Whisper,
}

// ── Shared app state ────────────────────────────────────────────────

struct AppState {
    backend: Backend,
    language: String,
    device_id: u32,
    deepgram_key: Option<String>,
    whisper_key: Option<String>,
    /// Active Deepgram session (created per recording, dropped after).
    dg_session: Option<deepgram::DgSession>,
}

// ── Env reader ──────────────────────────────────────────────────────

fn read_env_key(name: &str) -> Option<String> {
    if let Ok(val) = std::env::var(name) {
        if !val.is_empty() {
            return Some(val);
        }
    }
    let home = dirs::home_dir()?;
    let data = std::fs::read_to_string(home.join(".env.local")).ok()?;
    for line in data.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix(&format!("{}=", name)) {
            let val = val.trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

// ── Microphone enumeration (waveIn FFI) ─────────────────────────────

#[repr(C)]
struct WaveInCapsW {
    manufacturer_id: u16,
    product_id: u16,
    driver_version: u32,
    product_name: [u16; 32], // MAXPNAMELEN = 32
    formats: u32,
    channels: u16,
    reserved: u16,
}

#[link(name = "winmm")]
extern "system" {
    fn waveInGetNumDevs() -> u32;
    fn waveInGetDevCapsW(device: u32, caps: *mut WaveInCapsW, size: u32) -> u32;
}

struct MicDevice {
    id: u32,
    name: String,
}

fn list_mics() -> Vec<MicDevice> {
    let num = unsafe { waveInGetNumDevs() };
    let mut mics = Vec::new();
    for i in 0..num {
        let mut caps = WaveInCapsW {
            manufacturer_id: 0,
            product_id: 0,
            driver_version: 0,
            product_name: [0u16; 32],
            formats: 0,
            channels: 0,
            reserved: 0,
        };
        let ret = unsafe {
            waveInGetDevCapsW(i, &mut caps, std::mem::size_of::<WaveInCapsW>() as u32)
        };
        if ret == 0 {
            let name = String::from_utf16_lossy(
                &caps.product_name[..caps.product_name.iter().position(|&c| c == 0).unwrap_or(32)],
            );
            mics.push(MicDevice { id: i, name });
        }
    }
    mics
}

// ── Audio recorder (waveIn via raw FFI) ─────────────────────────────

#[repr(C)]
struct WaveFormatEx {
    format_tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    cb_size: u16,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct WaveHdr {
    data: *mut u8,
    buffer_length: u32,
    bytes_recorded: u32,
    user: usize,
    flags: u32,
    loops: u32,
    next: *mut WaveHdr,
    reserved: usize,
}

const WHDR_DONE: u32 = 0x00000001;
const WHDR_PREPARED: u32 = 0x00000002;
const NUM_BUFS: usize = 4;
const BUF_SAMPLES: usize = 1600; // 100ms at 16kHz
const BUF_BYTES: usize = BUF_SAMPLES * 2;

#[link(name = "winmm")]
extern "system" {
    fn waveInOpen(
        phwi: *mut usize,
        device: u32,
        fmt: *const WaveFormatEx,
        callback: usize,
        instance: usize,
        flags: u32,
    ) -> u32;
    fn waveInClose(hwi: usize) -> u32;
    fn waveInPrepareHeader(hwi: usize, hdr: *mut WaveHdr, size: u32) -> u32;
    fn waveInUnprepareHeader(hwi: usize, hdr: *mut WaveHdr, size: u32) -> u32;
    fn waveInAddBuffer(hwi: usize, hdr: *mut WaveHdr, size: u32) -> u32;
    fn waveInStart(hwi: usize) -> u32;
    fn waveInStop(hwi: usize) -> u32;
    fn waveInReset(hwi: usize) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateEventW(
        attrs: *const std::ffi::c_void,
        manual: i32,
        initial: i32,
        name: *const u16,
    ) -> usize;
    fn WaitForSingleObject(handle: usize, ms: u32) -> u32;
    fn ResetEvent(handle: usize) -> i32;
    fn CloseHandle(handle: usize) -> i32;
}

struct Recorder {
    hwi: usize,
    event: usize,
    bufs: Vec<Box<[u8; BUF_BYTES]>>,
    hdrs: Vec<Box<WaveHdr>>,
    all_data: Vec<u8>,
}

unsafe impl Send for Recorder {}

impl Recorder {
    fn new(device_id: u32) -> Result<Self, String> {
        let fmt = WaveFormatEx {
            format_tag: 1,
            channels: CHANNELS,
            samples_per_sec: SAMPLE_RATE,
            avg_bytes_per_sec: AVG_BYTES_PER_SEC,
            block_align: CHANNELS * BITS_PER_SAMPLE / 8,
            bits_per_sample: BITS_PER_SAMPLE,
            cb_size: 0,
        };

        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if event == 0 {
            return Err("CreateEvent failed".into());
        }

        let mut hwi: usize = 0;
        let ret = unsafe {
            waveInOpen(
                &mut hwi,
                device_id,
                &fmt,
                event,
                0,
                0x00050000, // CALLBACK_EVENT
            )
        };
        if ret != 0 {
            unsafe { CloseHandle(event); }
            return Err(format!("waveInOpen MMRESULT {}", ret));
        }

        let mut bufs: Vec<Box<[u8; BUF_BYTES]>> = Vec::with_capacity(NUM_BUFS);
        let mut hdrs: Vec<Box<WaveHdr>> = Vec::with_capacity(NUM_BUFS);

        for _i in 0..NUM_BUFS {
            bufs.push(Box::new([0u8; BUF_BYTES]));
            hdrs.push(Box::new(WaveHdr {
                data: std::ptr::null_mut(),
                buffer_length: BUF_BYTES as u32,
                bytes_recorded: 0,
                user: 0,
                flags: 0,
                loops: 0,
                next: std::ptr::null_mut(),
                reserved: 0,
            }));
        }

        for i in 0..NUM_BUFS {
            hdrs[i].data = bufs[i].as_mut_ptr();
        }

        let hdr_size = std::mem::size_of::<WaveHdr>() as u32;

        for i in 0..NUM_BUFS {
            let hdr_ptr: *mut WaveHdr = &mut *hdrs[i];
            let ret = unsafe { waveInPrepareHeader(hwi, hdr_ptr, hdr_size) };
            if ret != 0 {
                return Err(format!("waveInPrepareHeader[{}] MMRESULT {}", i, ret));
            }
            let ret = unsafe { waveInAddBuffer(hwi, hdr_ptr, hdr_size) };
            if ret != 0 {
                return Err(format!("waveInAddBuffer[{}] MMRESULT {}", i, ret));
            }
        }

        let ret = unsafe { waveInStart(hwi) };
        if ret != 0 {
            return Err(format!("waveInStart MMRESULT {}", ret));
        }

        Ok(Recorder {
            hwi,
            event,
            bufs,
            hdrs,
            all_data: Vec::with_capacity(SAMPLE_RATE as usize * 2 * 30),
        })
    }

    fn collect(&mut self) {
        let hdr_size = std::mem::size_of::<WaveHdr>() as u32;
        for i in 0..NUM_BUFS {
            let flags = self.hdrs[i].flags;
            if flags & WHDR_DONE != 0 {
                let recorded = self.hdrs[i].bytes_recorded as usize;
                if recorded > 0 {
                    let src = self.hdrs[i].data;
                    let slice = unsafe { std::slice::from_raw_parts(src, recorded) };
                    self.all_data.extend_from_slice(slice);
                }

                let hdr_ptr: *mut WaveHdr = &mut *self.hdrs[i];
                unsafe {
                    waveInUnprepareHeader(self.hwi, hdr_ptr, hdr_size);
                }
                self.hdrs[i].bytes_recorded = 0;
                self.hdrs[i].flags = 0;
                self.hdrs[i].data = self.bufs[i].as_mut_ptr();
                self.hdrs[i].buffer_length = BUF_BYTES as u32;
                unsafe {
                    waveInPrepareHeader(self.hwi, hdr_ptr, hdr_size);
                    waveInAddBuffer(self.hwi, hdr_ptr, hdr_size);
                }
            }
        }
    }

    fn wait_and_collect(&mut self) {
        unsafe {
            WaitForSingleObject(self.event, 200);
            ResetEvent(self.event);
        }
        self.collect();
    }

    /// Collect audio data from a single buffer pass and return it for streaming.
    fn collect_chunks(&mut self) -> Vec<u8> {
        let hdr_size = std::mem::size_of::<WaveHdr>() as u32;
        let mut chunk = Vec::new();
        for i in 0..NUM_BUFS {
            let flags = self.hdrs[i].flags;
            if flags & WHDR_DONE != 0 {
                let recorded = self.hdrs[i].bytes_recorded as usize;
                if recorded > 0 {
                    let src = self.hdrs[i].data;
                    let slice = unsafe { std::slice::from_raw_parts(src, recorded) };
                    chunk.extend_from_slice(slice);
                    self.all_data.extend_from_slice(slice);
                }

                let hdr_ptr: *mut WaveHdr = &mut *self.hdrs[i];
                unsafe {
                    waveInUnprepareHeader(self.hwi, hdr_ptr, hdr_size);
                }
                self.hdrs[i].bytes_recorded = 0;
                self.hdrs[i].flags = 0;
                self.hdrs[i].data = self.bufs[i].as_mut_ptr();
                self.hdrs[i].buffer_length = BUF_BYTES as u32;
                unsafe {
                    waveInPrepareHeader(self.hwi, hdr_ptr, hdr_size);
                    waveInAddBuffer(self.hwi, hdr_ptr, hdr_size);
                }
            }
        }
        chunk
    }

    fn wait_and_collect_chunks(&mut self) -> Vec<u8> {
        unsafe {
            WaitForSingleObject(self.event, 200);
            ResetEvent(self.event);
        }
        self.collect_chunks()
    }

    fn stop(mut self) -> Vec<u8> {
        unsafe {
            waveInStop(self.hwi);
            waveInReset(self.hwi);
        }
        self.collect();

        let hdr_size = std::mem::size_of::<WaveHdr>() as u32;
        for i in 0..NUM_BUFS {
            let hdr_ptr: *mut WaveHdr = &mut *self.hdrs[i];
            if self.hdrs[i].flags & WHDR_PREPARED != 0 {
                unsafe { waveInUnprepareHeader(self.hwi, hdr_ptr, hdr_size); }
            }
        }
        unsafe {
            waveInClose(self.hwi);
            CloseHandle(self.event);
        }
        std::mem::take(&mut self.all_data)
    }
}

// ── PCM to WAV ──────────────────────────────────────────────────────

fn pcm_to_wav(pcm: &[u8]) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let mut wav = Cursor::new(Vec::with_capacity(pcm.len() + 44));
    let _ = wav.write_all(b"RIFF");
    let _ = wav.write_all(&(36 + data_len).to_le_bytes());
    let _ = wav.write_all(b"WAVE");
    let _ = wav.write_all(b"fmt ");
    let _ = wav.write_all(&16u32.to_le_bytes());
    let _ = wav.write_all(&1u16.to_le_bytes());
    let _ = wav.write_all(&CHANNELS.to_le_bytes());
    let _ = wav.write_all(&SAMPLE_RATE.to_le_bytes());
    let byte_rate = SAMPLE_RATE * (BITS_PER_SAMPLE as u32 / 8) * CHANNELS as u32;
    let _ = wav.write_all(&byte_rate.to_le_bytes());
    let block_align = CHANNELS * BITS_PER_SAMPLE / 8;
    let _ = wav.write_all(&block_align.to_le_bytes());
    let _ = wav.write_all(&BITS_PER_SAMPLE.to_le_bytes());
    let _ = wav.write_all(b"data");
    let _ = wav.write_all(&data_len.to_le_bytes());
    let _ = wav.write_all(pcm);
    wav.into_inner()
}

// ── Whisper API ─────────────────────────────────────────────────────

fn transcribe_whisper(pcm: &[u8], api_key: &str, lang: &str) -> Result<String, String> {
    let wav = pcm_to_wav(pcm);

    let part = reqwest::blocking::multipart::Part::bytes(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;

    let form = reqwest::blocking::multipart::Form::new()
        .part("file", part)
        .text("model", "whisper-1")
        .text("language", lang.to_string())
        .text("prompt", vocab::whisper_prompt());

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("API {}: {}", status, body));
    }

    let body: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let text = body["text"].as_str().unwrap_or("").trim().to_string();
    Ok(text)
}

// ── Type text via SendInput ─────────────────────────────────────────

fn type_text(text: &str) {
    std::thread::sleep(std::time::Duration::from_millis(50));
    for ch in text.chars() {
        let inputs = [
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0),
                        wScan: ch as u16,
                        dwFlags: KEYEVENTF_UNICODE,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0),
                        wScan: ch as u16,
                        dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
        ];
        unsafe {
            SendInput(
                &inputs,
                std::mem::size_of::<INPUT>() as i32,
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

// ── Tray icon generation ────────────────────────────────────────────

/// Generate a 16x16 RGBA icon with a filled anti-aliased circle.
fn make_icon_rgba(r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
    const SIZE: usize = 16;
    let mut pixels = Vec::with_capacity(SIZE * SIZE * 4);
    let cx = (SIZE - 1) as f64 / 2.0;
    let cy = cx;
    let radius = SIZE as f64 / 2.0 - 1.0;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f64 - cx;
            let dy = y as f64 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= radius - 0.5 {
                pixels.extend_from_slice(&[r, g, b, a]);
            } else if dist <= radius + 0.5 {
                let aa = (a as f64 * (radius + 0.5 - dist)) as u8;
                pixels.extend_from_slice(&[r, g, b, aa]);
            } else {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    pixels
}

// ── App ─────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let dg_key = read_env_key("DEEPGRAM_API_KEY");
    let whisper_key = read_env_key("OPENAI_API_KEY");

    let default_backend = if dg_key.is_some() {
        Backend::Deepgram
    } else if whisper_key.is_some() {
        Backend::Whisper
    } else {
        panic!("No API keys found. Set DEEPGRAM_API_KEY or OPENAI_API_KEY in env or ~/.env.local");
    };

    log::info!("Starting with backend: {}", match default_backend {
        Backend::Deepgram => "Deepgram",
        Backend::Whisper => "Whisper",
    });

    let state = Arc::new(Mutex::new(AppState {
        backend: default_backend,
        language: "en".to_string(),
        device_id: u32::MAX, // WAVE_MAPPER
        deepgram_key: dg_key,
        whisper_key,
        dg_session: None,
    }));

    // Auto-grant microphone permission in WebView2 (skips permission prompt)
    std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", "--use-fake-ui-for-media-stream");

    let state_for_setup = state.clone();

    tauri::Builder::default()
        .setup(move |app| {
            // Always enable logging
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    .build(),
            )?;

            // Hide window on startup
            let window = app.get_webview_window("main").unwrap();
            let _ = window.hide();
            let _ = window.set_shadow(false);

            // ── Build tray menu ─────────────────────────────────
            let current_backend = state_for_setup.lock().unwrap().backend;
            let backend_label = match current_backend {
                Backend::Deepgram => "Deepgram Nova-3",
                Backend::Whisper => "Whisper",
            };

            let info = MenuItemBuilder::with_id("info", format!("Whisper Overlay ({})", backend_label))
                .enabled(false)
                .build(app)?;

            // Backend submenu
            let backend_dg = MenuItemBuilder::with_id("backend_deepgram", "Deepgram Nova-3").build(app)?;
            let backend_whisper = MenuItemBuilder::with_id("backend_whisper", "Whisper").build(app)?;
            let backend_sub = SubmenuBuilder::with_id(app, "backend_menu", "Backend")
                .item(&backend_dg)
                .item(&backend_whisper)
                .build()?;

            // Microphone submenu
            let mics = list_mics();
            let mut mic_items = Vec::new();
            let mic_sub_builder = SubmenuBuilder::with_id(app, "mic_menu", "Microphone");
            let mut mic_sub_builder = mic_sub_builder;
            for mic in &mics {
                let item = MenuItemBuilder::with_id(format!("mic_{}", mic.id), &mic.name).build(app)?;
                mic_sub_builder = mic_sub_builder.item(&item);
                mic_items.push((mic.id, item));
            }
            if mics.is_empty() {
                let no_mic = MenuItemBuilder::with_id("no_mic", "No microphones found")
                    .enabled(false)
                    .build(app)?;
                mic_sub_builder = mic_sub_builder.item(&no_mic);
            }
            let mic_sub = mic_sub_builder.build()?;

            // Language submenu
            let lang_en = MenuItemBuilder::with_id("lang_en", "English").build(app)?;
            let lang_hi = MenuItemBuilder::with_id("lang_hi", "Hindi").build(app)?;
            let lang_auto = MenuItemBuilder::with_id("lang_auto", "Auto-detect").build(app)?;
            let lang_sub = SubmenuBuilder::with_id(app, "lang_menu", "Language")
                .item(&lang_en)
                .item(&lang_hi)
                .item(&lang_auto)
                .build()?;

            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

            let menu = MenuBuilder::new(app)
                .item(&info)
                .separator()
                .item(&backend_sub)
                .item(&mic_sub)
                .item(&lang_sub)
                .separator()
                .item(&quit)
                .build()?;

            // Generate tray icons
            let icon_idle = make_icon_rgba(128, 128, 128, 255);
            let icon_listen = make_icon_rgba(76, 175, 80, 255);
            let icon_transcribe = make_icon_rgba(255, 152, 0, 255);

            let tray = TrayIconBuilder::new()
                .icon(Image::new_owned(icon_idle.clone(), 16, 16))
                .tooltip("Whisper Overlay — Idle")
                .menu(&menu)
                .build(app)?;

            // ── Menu event handler ──────────────────────────────
            let state_menu = state_for_setup.clone();
            let tray_id = tray.id().clone();

            app.on_menu_event(move |app_handle, event| {
                let id = event.id().as_ref();

                if id == "quit" {
                    app_handle.exit(0);
                    return;
                }

                if id == "backend_deepgram" {
                    let mut st = state_menu.lock().unwrap();
                    if st.backend != Backend::Deepgram {
                        if st.deepgram_key.is_some() {
                            st.backend = Backend::Deepgram;
                            log::info!("Switched to Deepgram backend");
                        } else {
                            log::error!("DEEPGRAM_API_KEY not available");
                        }
                    }
                    return;
                }

                if id == "backend_whisper" {
                    let mut st = state_menu.lock().unwrap();
                    if st.backend != Backend::Whisper {
                        if st.whisper_key.is_some() {
                            st.backend = Backend::Whisper;
                            log::info!("Switched to Whisper backend");
                        } else {
                            log::error!("OPENAI_API_KEY not available");
                        }
                    }
                    return;
                }

                if id.starts_with("mic_") {
                    if let Ok(mic_id) = id.strip_prefix("mic_").unwrap().parse::<u32>() {
                        let mut st = state_menu.lock().unwrap();
                        st.device_id = mic_id;
                        log::info!("Switched microphone to device {}", mic_id);
                    }
                    return;
                }

                if id.starts_with("lang_") {
                    let lang = match id {
                        "lang_en" => "en",
                        "lang_hi" => "hi",
                        "lang_auto" => "auto",
                        _ => return,
                    };
                    let mut st = state_menu.lock().unwrap();
                    st.language = lang.to_string();
                    log::info!("Switched language to {}", lang);
                    return;
                }
            });

            // ── Hotkey + recording thread ───────────────────────
            let handle = app.handle().clone();
            let state_hotkey = state_for_setup.clone();

            std::thread::spawn(move || {
                let recorder: Arc<Mutex<Option<Recorder>>> = Arc::new(Mutex::new(None));
                let mut was_pressed = false;

                loop {
                    let pressed =
                        unsafe { GetAsyncKeyState(VK_LMENU) } & (0x8000u16 as i16) != 0;

                    if pressed && !was_pressed {
                        // Key down — show overlay, update tray, start recording
                        let (device_id, backend, dg_key, lang) = {
                            let st = state_hotkey.lock().unwrap();
                            (st.device_id, st.backend, st.deepgram_key.clone(), st.language.clone())
                        };

                        // Update tray icon to listening (green)
                        if let Some(tray) = handle.tray_by_id(&tray_id) {
                            let _ = tray.set_icon(Some(Image::new_owned(icon_listen.clone(), 16, 16)));
                            let _ = tray.set_tooltip(Some("Whisper Overlay — Listening..."));
                        }

                        let w = handle.get_webview_window("main").unwrap();
                        let _ = w.show();
                        let _ = handle.emit("hotkey-press", true);

                        // If Deepgram, create a new session (connects in background)
                        if backend == Backend::Deepgram {
                            if let Some(ref key) = dg_key {
                                let session = deepgram::DgSession::start(key, &lang);
                                state_hotkey.lock().unwrap().dg_session = Some(session);
                            }
                        }

                        match Recorder::new(device_id) {
                            Ok(rec) => {
                                *recorder.lock().unwrap() = Some(rec);
                            }
                            Err(e) => {
                                log::error!("Recording failed: {}", e);
                            }
                        }
                    } else if pressed {
                        // Key held — collect audio buffers, stream to Deepgram
                        let backend = state_hotkey.lock().unwrap().backend;
                        if let Some(ref mut rec) = *recorder.lock().unwrap() {
                            if backend == Backend::Deepgram {
                                let chunks = rec.wait_and_collect_chunks();
                                if !chunks.is_empty() {
                                    let st = state_hotkey.lock().unwrap();
                                    if let Some(ref session) = st.dg_session {
                                        session.send(&chunks);
                                    }
                                }
                            } else {
                                rec.wait_and_collect();
                            }
                        }
                    } else if !pressed && was_pressed {
                        // Key up — keep overlay for transcription, update tray
                        let _ = handle.emit("hotkey-release", false);

                        // Update tray icon to transcribing (orange)
                        if let Some(tray) = handle.tray_by_id(&tray_id) {
                            let _ = tray.set_icon(Some(Image::new_owned(icon_transcribe.clone(), 16, 16)));
                            let _ = tray.set_tooltip(Some("Whisper Overlay — Transcribing..."));
                        }

                        let pcm = {
                            let mut guard = recorder.lock().unwrap();
                            guard.take().map(|r| r.stop())
                        };

                        // Take the DgSession out of state (it's consumed per recording)
                        let dg_session = state_hotkey.lock().unwrap().dg_session.take();

                        let handle_c = handle.clone();
                        let state_c = state_hotkey.clone();
                        let icon_idle_c = icon_idle.clone();
                        let tray_id_c = tray_id.clone();

                        std::thread::spawn(move || {
                            let st = state_c.lock().unwrap();
                            let backend = st.backend;
                            let lang = st.language.clone();
                            drop(st);

                            let mut text = String::new();

                            if let Some(pcm) = pcm {
                                let duration = pcm.len() as f64 / AVG_BYTES_PER_SEC as f64;

                                if duration >= MIN_DURATION_SECS {
                                    match backend {
                                        Backend::Deepgram => {
                                            if let Some(session) = dg_session {
                                                text = session.finalize();
                                                // Session is dropped here — WS thread closes
                                            }
                                        }
                                        Backend::Whisper => {
                                            let st = state_c.lock().unwrap();
                                            if let Some(ref api_key) = st.whisper_key {
                                                match transcribe_whisper(&pcm, api_key, &lang) {
                                                    Ok(t) => text = t,
                                                    Err(e) => log::error!("Whisper failed: {}", e),
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    log::info!("Too short ({:.1}s), ignoring", duration);
                                }
                            }

                            if !text.is_empty() {
                                type_text(&text);
                            }

                            // Done — hide overlay, reset tray
                            let _ = handle_c.emit("transcribe-done", true);
                            let w = handle_c.get_webview_window("main").unwrap();
                            let _ = w.hide();

                            if let Some(tray) = handle_c.tray_by_id(&tray_id_c) {
                                let _ = tray.set_icon(Some(Image::new_owned(icon_idle_c, 16, 16)));
                                let _ = tray.set_tooltip(Some("Whisper Overlay — Idle"));
                            }
                        });
                    }

                    was_pressed = pressed;
                    std::thread::sleep(std::time::Duration::from_millis(16));
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
