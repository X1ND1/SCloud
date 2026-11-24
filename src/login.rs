//! SoundCloud sign-in through an embedded browser window (Edge WebView2).
//!
//! The window runs as a SEPARATE process (`scloud.exe --sc-login`) so its event loop
//! doesn't interfere with the main window. The user signs in as usual (email, Google, Apple…),
//! the process sees the `oauth_token` cookie, prints it to stdout and exits.

use std::path::PathBuf;
use std::process::{Command, Stdio};

pub const LOGIN_ARG: &str = "--sc-login";
/// Hidden background browser for write requests (likes, history).
pub const BRIDGE_ARG: &str = "--sc-bridge";

/// Profile folder of the embedded browser (sign-in cookies).
pub fn webview_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("SCloud")
        .join("webview")
}

/// Called from the main app: opens the sign-in window and waits for the token.
pub fn login_blocking() -> Result<String, String> {
    if !cfg!(windows) {
        return Err("Окно входа пока есть только в Windows-версии".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = Command::new(exe)
        .arg(LOGIN_ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|l| l.strip_prefix("TOKEN:"))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Окно входа закрыто без входа".to_string())
}

/// Delete the saved sign-in in the embedded browser (for "Sign out").
pub fn forget_webview_session() {
    let _ = std::fs::remove_dir_all(webview_data_dir());
}

/// Entry point of the `--sc-login` process.
#[cfg(windows)]
pub fn run_login_window() {
    use std::io::Write;
    use std::time::{Duration, Instant};
    use tao::{
        event::{Event, WindowEvent},
        event_loop::{ControlFlow, EventLoop},
        window::WindowBuilder,
    };
    use wry::{WebContext, WebViewBuilder};

    fn finish(token: &str) -> ! {
        let mut out = std::io::stdout();
        let _ = writeln!(out, "TOKEN:{token}");
        let _ = out.flush();
        std::process::exit(0);
    }

    let event_loop = EventLoop::new();
    let window = match WindowBuilder::new()
        .with_title("Вход в SoundCloud — SCloud")
        .with_inner_size(tao::dpi::LogicalSize::new(480.0, 720.0))
        .build(&event_loop)
    {
        Ok(w) => w,
        Err(_) => std::process::exit(1),
    };

    let mut context = WebContext::new(Some(webview_data_dir()));
    // fallback: if the cookie is readable from JS, the page sends the token itself
    let script = r#"
        (function () {
          function check() {
            if (!location.hostname.endsWith('soundcloud.com')) return;
            var m = document.cookie.match(/(?:^|;\s*)oauth_token=([^;]+)/);
            if (m && window.ipc) window.ipc.postMessage(m[1]);
          }
          setInterval(check, 1000);
        })();
    "#;
    let webview = match WebViewBuilder::new_with_web_context(&mut context)
        .with_url("https://soundcloud.com/signin")
        .with_initialization_script(script)
        .with_ipc_handler(|req| finish(req.body()))
        // Google/Apple/Facebook sign-in opens in a popup, so allow it
        .with_new_window_req_handler(|_url, _features| wry::NewWindowResponse::Allow)
        .build(&window)
    {
        Ok(w) => w,
        Err(_) => std::process::exit(2), // no WebView2 Runtime
    };

    let mut last_check = Instant::now();
    event_loop.run(move |event, _, control_flow| {
        let _keep_window_alive = &window;
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(800));
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            std::process::exit(1);
        }
        // main path: read the browser cookie directly (works for HttpOnly too)
        if last_check.elapsed() >= Duration::from_millis(700) {
            last_check = Instant::now();
            if let Ok(cookies) = webview.cookies_for_url("https://soundcloud.com") {
                if let Some(c) = cookies
                    .iter()
                    .find(|c| c.name() == "oauth_token" && !c.value().is_empty())
                {
                    finish(c.value());
                }
            }
        }
    });
}

#[cfg(not(windows))]
pub fn run_login_window() {
    std::process::exit(1);
}

// =====================================================================
// Bridge through a hidden browser.
//
// SoundCloud protects write requests (likes, history entries) with
// DataDome: a plain HTTP request from the app gets a 403. So such
// requests are sent with fetch straight from the soundcloud.com page in a hidden
// WebView2 with the same profile you signed in with; to SoundCloud it's just the website.
//
// Protocol: the main process writes JSON lines to the bridge's stdin
//   {"id":1,"method":"PUT","url":"…","token":"…","body":null}
// the bridge answers on stdout: {"id":1,"status":200}
// =====================================================================

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write as _};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

pub struct Bridge {
    /// false once the bridge process has exited (it closes itself when idle)
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stdin: Mutex<std::process::ChildStdin>,
    pending: std::sync::Arc<Mutex<HashMap<u64, mpsc::Sender<Result<u16, String>>>>>,
    next: AtomicU64,
}

/// The current bridge; restarted if it closed due to idleness.
static BRIDGE: Mutex<Option<std::sync::Arc<Bridge>>> = Mutex::new(None);

/// How long the bridge lives without requests before closing (seconds).
const BRIDGE_IDLE_SECS: u64 = 45;

fn start_bridge() -> Result<Bridge, String> {
    if !cfg!(windows) {
        return Err("лайки через браузер пока только в Windows-версии".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg(BRIDGE_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdin = child.stdin.take().ok_or("нет stdin у моста")?;
    let stdout = child.stdout.take().ok_or("нет stdout у моста")?;
    let pending: std::sync::Arc<Mutex<HashMap<u64, mpsc::Sender<Result<u16, String>>>>> = Default::default();
    let p2 = pending.clone();
    let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let alive2 = alive.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let Some(id) = v["id"].as_u64() else { continue };
            let res = match v["error"].as_str() {
                Some(e) => Err(e.to_string()),
                None => Ok(v["status"].as_u64().unwrap_or(0) as u16),
            };
            if let Some(tx) = p2.lock().unwrap().remove(&id) {
                let _ = tx.send(res);
            }
        }
        // the bridge closed: wake everyone who is waiting
        alive2.store(false, Ordering::Relaxed);
        for (_, tx) in p2.lock().unwrap().drain() {
            let _ = tx.send(Err("фоновый браузер закрылся".into()));
        }
    });
    drop(child); // the process lives on its own and exits when idle or together with the app
    Ok(Bridge {
        alive,
        stdin: Mutex::new(stdin),
        pending,
        next: AtomicU64::new(1),
    })
}

fn bridge() -> Result<std::sync::Arc<Bridge>, String> {
    let mut slot = BRIDGE.lock().unwrap();
    if let Some(b) = slot.as_ref() {
        if b.alive.load(Ordering::Relaxed) {
            return Ok(b.clone());
        }
    }
    let b = std::sync::Arc::new(start_bridge()?);
    *slot = Some(b.clone());
    Ok(b)
}

/// Run a request from the soundcloud.com page. Returns the HTTP status.
pub fn browser_fetch(
    method: &str,
    url: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> Result<u16, String> {
    // if the bridge closed from idleness right at the moment of the request, start a new one and retry
    match browser_fetch_once(method, url, token, body.clone()) {
        Err(e) if e.contains("закрылся") || e.contains("не отвечает") => {
            browser_fetch_once(method, url, token, body)
        }
        r => r,
    }
}

fn browser_fetch_once(
    method: &str,
    url: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> Result<u16, String> {
    let bridge = bridge()?;
    let id = bridge.next.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    bridge.pending.lock().unwrap().insert(id, tx);
    let line = serde_json::json!({ "id": id, "method": method, "url": url, "token": token, "body": body });
    {
        let mut stdin = bridge.stdin.lock().unwrap();
        writeln!(stdin, "{line}").map_err(|e| format!("фоновый браузер не отвечает: {e}"))?;
        let _ = stdin.flush();
    }
    rx.recv_timeout(Duration::from_secs(40)).unwrap_or_else(|_| {
        bridge.pending.lock().unwrap().remove(&id);
        Err("фоновый браузер не ответил за 40 с".into())
    })
}

/// Entry point of the `--sc-bridge` process.
#[cfg(windows)]
pub fn run_bridge() {
    use tao::{
        event::Event,
        event_loop::{ControlFlow, EventLoopBuilder},
        window::WindowBuilder,
    };
    use wry::{WebContext, WebViewBuilder};

    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // stdin → loop events; end of stdin = the main window closed → exit
    let p_in = proxy.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    let _ = p_in.send_event(l);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        std::process::exit(0);
    });

    let window = match WindowBuilder::new()
        .with_title("SCloud")
        .with_visible(false)
        .build(&event_loop)
    {
        Ok(w) => w,
        Err(_) => std::process::exit(1),
    };
    let mut context = WebContext::new(Some(webview_data_dir()));
    let ready_script = r#"
        (function () {
          if (window.top !== window || location.hostname !== 'soundcloud.com') return;
          var sent = false;
          function ready() { if (!sent && window.ipc) { sent = true; window.ipc.postMessage('__ready__'); } }
          if (document.readyState === 'complete') ready(); else window.addEventListener('load', ready);
          setTimeout(ready, 8000);
        })();
    "#;
    let p_ready = proxy.clone();
    let webview = match WebViewBuilder::new_with_web_context(&mut context)
        .with_url("https://soundcloud.com/discover")
        .with_initialization_script(ready_script)
        .with_ipc_handler(move |req| {
            let body = req.body();
            if body == "__ready__" {
                let _ = p_ready.send_event("__ready__".to_string());
            } else {
                let mut out = std::io::stdout();
                let _ = writeln!(out, "{body}");
                let _ = out.flush();
            }
        })
        .build(&window)
    {
        Ok(w) => w,
        Err(_) => std::process::exit(2),
    };

    fn script(cmd: &str) -> Option<String> {
        let v: serde_json::Value = serde_json::from_str(cmd).ok()?;
        let id = v["id"].as_u64()?;
        let method = serde_json::to_string(v["method"].as_str()?).ok()?;
        let url = serde_json::to_string(v["url"].as_str()?).ok()?;
        let auth = serde_json::to_string(&format!("OAuth {}", v["token"].as_str().unwrap_or(""))).ok()?;
        let body = if v["body"].is_null() {
            "undefined".to_string()
        } else {
            serde_json::to_string(&v["body"].to_string()).ok()?
        };
        Some(format!(
            r#"(async () => {{
                try {{
                  const r = await fetch({url}, {{
                    method: {method}, credentials: 'include',
                    headers: {{ 'Authorization': {auth}, 'Content-Type': 'application/json' }},
                    body: {body}
                  }});
                  window.ipc.postMessage(JSON.stringify({{ id: {id}, status: r.status }}));
                }} catch (e) {{
                  window.ipc.postMessage(JSON.stringify({{ id: {id}, error: String(e) }}));
                }}
              }})();"#
        ))
    }

    let mut ready = false;
    let mut queue: Vec<String> = Vec::new();
    let mut last_activity = std::time::Instant::now();
    let idle = Duration::from_secs(BRIDGE_IDLE_SECS);
    event_loop.run(move |event, _, control_flow| {
        let _keep_window_alive = &window;
        // close if there were no requests for a while: don't keep a browser in memory
        if last_activity.elapsed() >= idle && queue.is_empty() {
            std::process::exit(0);
        }
        *control_flow = ControlFlow::WaitUntil(last_activity + idle);
        if let Event::UserEvent(cmd) = event {
            last_activity = std::time::Instant::now();
            *control_flow = ControlFlow::WaitUntil(last_activity + idle);
            if cmd == "__ready__" {
                ready = true;
                for c in queue.drain(..) {
                    if let Some(js) = script(&c) {
                        let _ = webview.evaluate_script(&js);
                    }
                }
            } else if ready {
                if let Some(js) = script(&cmd) {
                    let _ = webview.evaluate_script(&js);
                }
            } else {
                queue.push(cmd);
            }
        }
    });
}

#[cfg(not(windows))]
pub fn run_bridge() {
    std::process::exit(1);
}
