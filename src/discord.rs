//! Discord Rich Presence "Listening to …" over Discord's local IPC (no third-party crates).
//! Protocol: frames [op: u32 LE][len: u32 LE][JSON]; op 0 is the handshake, op 1 a command.

use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq)]
pub struct Presence {
    pub title: String,
    pub artist: String,
    pub artwork: Option<String>,
    pub url: Option<String>,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    /// playback speed (slowed/nightcore), for a correct time bar
    pub speed: f64,
    /// "slowed + reverb" and the like
    pub fx_label: Option<String>,
}

enum Cmd {
    Set(Presence),
    Clear,
    AppId(String),
}

pub struct Discord {
    tx: Sender<Cmd>,
}

impl Discord {
    pub fn start(app_id: String) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || worker(app_id, rx));
        Self { tx }
    }
    pub fn set(&self, p: Presence) {
        let _ = self.tx.send(Cmd::Set(p));
    }
    pub fn clear(&self) {
        let _ = self.tx.send(Cmd::Clear);
    }
    pub fn set_app_id(&self, id: String) {
        let _ = self.tx.send(Cmd::AppId(id));
    }
}

#[cfg(windows)]
type Conn = std::fs::File;
#[cfg(unix)]
type Conn = std::os::unix::net::UnixStream;

#[cfg(windows)]
fn open_pipe() -> Option<Conn> {
    (0..10).find_map(|i| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!(r"\\.\pipe\discord-ipc-{i}"))
            .ok()
    })
}

#[cfg(unix)]
fn open_pipe() -> Option<Conn> {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .or_else(|_| std::env::var("TMPDIR"))
        .unwrap_or_else(|_| "/tmp".into());
    for i in 0..10 {
        for sub in ["", "app/com.discordapp.Discord/", "snap.discord/"] {
            if let Ok(s) = Conn::connect(format!("{base}/{sub}discord-ipc-{i}")) {
                return Some(s);
            }
        }
    }
    None
}

fn write_frame(c: &mut Conn, op: u32, v: &Value) -> io::Result<()> {
    let data = v.to_string().into_bytes();
    let mut buf = Vec::with_capacity(8 + data.len());
    buf.extend_from_slice(&op.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(&data);
    c.write_all(&buf)?;
    c.flush()
}

fn read_frame(c: &mut Conn) -> io::Result<(u32, Value)> {
    let mut h = [0u8; 8];
    c.read_exact(&mut h)?;
    let op = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
    let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
    let mut b = vec![0u8; len];
    c.read_exact(&mut b)?;
    Ok((op, serde_json::from_slice(&b).unwrap_or(Value::Null)))
}

fn connect(app_id: &str) -> Option<Conn> {
    let mut c = open_pipe()?;
    write_frame(&mut c, 0, &json!({ "v": 1, "client_id": app_id })).ok()?;
    let (op, v) = read_frame(&mut c).ok()?;
    // op 2 = close (e.g. an invalid Application ID)
    if op == 2 || v["evt"] == "ERROR" {
        return None;
    }
    Some(c)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Discord requires 2..=128 characters in details/state.
fn clamp(s: &str) -> String {
    let mut s: String = s.chars().take(128).collect();
    while s.chars().count() < 2 {
        s.push(' ');
    }
    s
}

fn build_activity(p: &Presence) -> Value {
    let state = if p.paused {
        crate::i18n::tf("{} · пауза", &[("{}", format!("{}", p.artist))])
    } else {
        p.artist.clone()
    };
    let mut a = json!({
        "type": 2, // Listening
        "details": clamp(&match &p.fx_label {
            Some(l) => format!("{} ({l})", p.title),
            None => p.title.clone(),
        }),
        "state": clamp(&state),
    });
    if let Some(img) = &p.artwork {
        a["assets"] = json!({ "large_image": img, "large_text": clamp(&p.title) });
    }
    if !p.paused && p.duration > 0.0 {
        let speed = if p.speed > 0.1 { p.speed } else { 1.0 };
        let start = now_ms().saturating_sub((p.position / speed * 1000.0) as u64);
        let end = start + (p.duration / speed * 1000.0) as u64;
        a["timestamps"] = json!({ "start": start, "end": end });
    }
    if let Some(url) = &p.url {
        a["buttons"] = json!([{ "label": crate::i18n::t("Слушать на SoundCloud"), "url": url }]);
    }
    a
}

fn worker(mut app_id: String, rx: Receiver<Cmd>) {
    let mut conn: Option<Conn> = None;
    let mut wanted: Option<Presence> = None;
    let mut dirty = false;
    let mut last_try: Option<Instant> = None;
    let mut nonce: u64 = 0;

    loop {
        let first = match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // coalesce a burst of commands to stay under Discord's limit (5 updates / 20 s)
        for cmd in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            match cmd {
                Cmd::Set(p) => wanted = Some(p),
                Cmd::Clear => wanted = None,
                Cmd::AppId(id) => {
                    app_id = id;
                    conn = None;
                    last_try = None;
                }
            }
            dirty = true;
        }
        if !dirty || app_id.trim().is_empty() {
            continue;
        }
        if conn.is_none() {
            // Discord may not be running yet: retry every 15 seconds
            if last_try.is_some_and(|t| t.elapsed() < Duration::from_secs(15)) {
                continue;
            }
            last_try = Some(Instant::now());
            conn = connect(app_id.trim());
            if conn.is_none() {
                continue;
            }
        }
        nonce += 1;
        let payload = json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": std::process::id(), "activity": wanted.as_ref().map(build_activity) },
            "nonce": nonce.to_string(),
        });
        let c = conn.as_mut().unwrap();
        match write_frame(c, 1, &payload).and_then(|_| read_frame(c)) {
            Ok(_) => dirty = false,
            Err(_) => conn = None,
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
