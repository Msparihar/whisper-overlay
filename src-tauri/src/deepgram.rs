use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

use crate::vocab::TECH_TERMS;

const SAMPLE_RATE: u32 = 16000;
const CHANNELS: u32 = 1;
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// Deepgram JSON result (subset we care about).
#[derive(serde::Deserialize, Default)]
struct DgResult {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    is_final: bool,
    #[serde(default)]
    speech_finalized: bool,
    #[serde(default)]
    channel: DgChannel,
}

#[derive(serde::Deserialize, Default)]
struct DgChannel {
    #[serde(default)]
    alternatives: Vec<DgAlt>,
}

#[derive(serde::Deserialize, Default)]
struct DgAlt {
    #[serde(default)]
    transcript: String,
}

/// Messages queued for the WS thread.
enum WsCmd {
    Audio(Vec<u8>),
    Finalize,
    Shutdown,
}

/// Shared state between the caller and the WS thread.
struct SessionState {
    parts: Vec<String>,
    done: bool,
    finalizing: bool,
}

/// An on-demand Deepgram session. Created per recording, dropped after.
/// Connect + stream + finalize + close in one lifecycle.
pub struct DgSession {
    write_queue: Arc<Mutex<Vec<WsCmd>>>,
    state: Arc<(Mutex<SessionState>, Condvar)>,
}

impl DgSession {
    /// Start a new session — connects to Deepgram in the background.
    /// Audio can be sent immediately; it queues until the connection is ready.
    pub fn start(api_key: &str, lang: &str) -> Self {
        let mut params = vec![
            ("model", "nova-3".to_string()),
            ("language", lang.to_string()),
            ("encoding", "linear16".to_string()),
            ("sample_rate", SAMPLE_RATE.to_string()),
            ("channels", CHANNELS.to_string()),
            ("punctuate", "true".to_string()),
            ("smart_format", "true".to_string()),
            ("interim_results", "true".to_string()),
            ("vad_events", "true".to_string()),
            ("endpointing", "300".to_string()),
            ("utterance_end_ms", "1000".to_string()),
        ];
        for term in TECH_TERMS {
            params.push(("keyterm", term.to_string()));
        }

        let query: String = params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding(v)))
            .collect::<Vec<_>>()
            .join("&");

        let ws_url = format!("wss://api.deepgram.com/v1/listen?{}", query);

        let write_queue = Arc::new(Mutex::new(Vec::<WsCmd>::new()));
        let state = Arc::new((
            Mutex::new(SessionState {
                parts: Vec::new(),
                done: false,
                finalizing: false,
            }),
            Condvar::new(),
        ));
        let wq = write_queue.clone();
        let st = state.clone();
        let api_key = api_key.to_string();

        std::thread::spawn(move || {
            ws_thread(wq, st, &ws_url, &api_key);
        });

        log::info!("Deepgram session: connecting in background...");

        DgSession {
            write_queue,
            state,
        }
    }

    /// Queue a PCM audio chunk. Safe to call before connection is established.
    pub fn send(&self, data: &[u8]) {
        let mut queue = self.write_queue.lock().unwrap();
        queue.push(WsCmd::Audio(data.to_vec()));
    }

    /// Signal end of recording. Waits up to 1.5s for the final transcript.
    /// After this call, the session is done — the WS thread will close.
    pub fn finalize(&self) -> String {
        // Queue finalize command
        {
            let mut queue = self.write_queue.lock().unwrap();
            queue.push(WsCmd::Finalize);
        }

        // Mark finalizing
        {
            let (lock, _cv) = &*self.state;
            let mut st = lock.lock().unwrap();
            st.finalizing = true;
        }

        log::info!("Deepgram session: finalize requested");

        // Wait for done (up to 1.5s)
        let (lock, cv) = &*self.state;
        let st = lock.lock().unwrap();
        let (mut st, timeout_result) = cv
            .wait_timeout_while(st, Duration::from_millis(1500), |s| !s.done)
            .unwrap();

        if timeout_result.timed_out() {
            log::info!("Deepgram session: finalize timed out (no speech)");
            st.done = true;
        }

        let text = st.parts.join(" ");
        if !text.is_empty() {
            log::info!("Deepgram transcription: \"{}\"", text);
        } else {
            log::info!("Deepgram: no speech detected");
        }

        // Signal shutdown to the WS thread
        {
            let mut queue = self.write_queue.lock().unwrap();
            queue.push(WsCmd::Shutdown);
        }

        text
    }
}

/// WS thread: connect, then loop draining writes + reading results.
fn ws_thread(
    write_queue: Arc<Mutex<Vec<WsCmd>>>,
    state: Arc<(Mutex<SessionState>, Condvar)>,
    ws_url: &str,
    api_key: &str,
) {
    log::info!("Deepgram: connecting...");

    let request = tungstenite::http::Request::builder()
        .uri(ws_url)
        .header("Authorization", format!("Token {}", api_key))
        .header("Host", "api.deepgram.com")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();

    let mut ws = match connect(request) {
        Ok((ws, _)) => {
            log::info!("Deepgram: connected");
            ws
        }
        Err(e) => {
            log::error!("Deepgram connect failed: {}", e);
            // Mark done so finalize() doesn't hang
            let (lock, cv) = &*state;
            let mut st = lock.lock().unwrap();
            st.done = true;
            cv.notify_all();
            return;
        }
    };

    // Set read timeout so we can drain writes between reads
    set_read_timeout(&mut ws, Some(READ_TIMEOUT));

    // Main loop
    loop {
        // 1. Drain write queue
        let mut should_shutdown = false;
        {
            let mut queue = write_queue.lock().unwrap();
            for cmd in queue.drain(..) {
                match cmd {
                    WsCmd::Audio(data) => {
                        if let Err(e) = ws.send(Message::Binary(data.into())) {
                            log::error!("Deepgram audio send error: {}", e);
                            mark_done(&state);
                            return;
                        }
                    }
                    WsCmd::Finalize => {
                        log::info!("Deepgram: sending Finalize");
                        if let Err(e) =
                            ws.send(Message::Text(r#"{"type":"Finalize"}"#.into()))
                        {
                            log::error!("Deepgram Finalize send error: {}", e);
                            mark_done(&state);
                            return;
                        }
                    }
                    WsCmd::Shutdown => {
                        should_shutdown = true;
                    }
                }
            }
        }

        if should_shutdown {
            log::info!("Deepgram: session closing");
            let _ = ws.close(None);
            return;
        }

        // 2. Try to read (times out after READ_TIMEOUT)
        match ws.read() {
            Ok(Message::Text(text)) => {
                let r: DgResult = match serde_json::from_str(&text) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                if r.r#type == "Results" && r.is_final {
                    let (lock, cv) = &*state;
                    let mut st = lock.lock().unwrap();
                    if let Some(alt) = r.channel.alternatives.first() {
                        let t = alt.transcript.trim().to_string();
                        if !t.is_empty() {
                            log::info!("Deepgram final: \"{}\"", t);
                            st.parts.push(t);
                        }
                    }
                    if r.speech_finalized || st.finalizing {
                        log::info!("Deepgram: done (speech_finalized={}, finalizing={})",
                            r.speech_finalized, st.finalizing);
                        st.done = true;
                        cv.notify_all();
                    }
                } else if r.r#type == "Results" && !r.is_final {
                    if let Some(alt) = r.channel.alternatives.first() {
                        let t = alt.transcript.trim();
                        if !t.is_empty() {
                            log::debug!("Deepgram interim: \"{}\"", t);
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => {
                log::info!("Deepgram: server closed connection");
                mark_done(&state);
                return;
            }
            Ok(Message::Ping(data)) => {
                let _ = ws.send(Message::Pong(data));
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Read timeout — normal, loop back
                continue;
            }
            Err(e) => {
                log::error!("Deepgram read error: {}", e);
                mark_done(&state);
                return;
            }
        }
    }
}

fn mark_done(state: &Arc<(Mutex<SessionState>, Condvar)>) {
    let (lock, cv) = &**state;
    let mut st = lock.lock().unwrap();
    st.done = true;
    cv.notify_all();
}

fn set_read_timeout(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>, timeout: Option<Duration>) {
    match ws.get_mut() {
        MaybeTlsStream::Plain(tcp) => {
            let _ = tcp.set_read_timeout(timeout);
        }
        MaybeTlsStream::NativeTls(tls) => {
            let _ = tls.get_mut().set_read_timeout(timeout);
        }
        _ => {
            log::warn!("Deepgram: unknown stream type, can't set read timeout");
        }
    }
}

fn urlencoding(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            b' ' => result.push_str("%20"),
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}
