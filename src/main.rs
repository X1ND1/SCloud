#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod align;
mod api;
mod app;
mod discord;
mod downloader;
mod fx;
mod i18n;
mod library;
mod login;
mod logo;
mod lyrics;
mod media;
mod player;
mod stats;
mod theme;
mod wave;

/// Write the error to %LOCALAPPDATA%\SCloud\crash.log and show a dialog with the text
/// (the app has no console; without this a crash looks like "the exe doesn't start").
fn report_crash(text: &str) {
    let path = dirs::data_local_dir().map(|d| d.join("SCloud").join("crash.log"));
    if let Some(p) = &path {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = std::fs::write(p, format!("[{stamp}]\n{text}\n"));
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let shown: String = text.chars().take(1500).collect();
        let body = format!(
            "SCloud crashed:\n\n{shown}\n\nFull log: {}",
            path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
        );
        let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (b, t) = (w(&body), w("SCloud error"));
        unsafe {
            MessageBoxW(std::ptr::null_mut(), b.as_ptr(), t.as_ptr(), MB_OK | MB_ICONERROR);
        }
    }
}

fn main() -> eframe::Result<()> {
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        report_crash(&format!("{info}\n\n{bt}"));
    }));
    // separate process for the SoundCloud sign-in window
    if std::env::args().any(|a| a == login::LOGIN_ARG) {
        login::run_login_window();
        return Ok(());
    }
    // separate process: the hidden browser for likes and history
    if std::env::args().any(|a| a == login::BRIDGE_ARG) {
        login::run_bridge();
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("SCloud")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 560.0])
            .with_transparent(true)
            // no system title bar: our own window buttons in the header, dragging by the header
            .with_decorations(false)
            .with_icon(logo::frames()[0].clone()),
        ..Default::default()
    };
    let r = eframe::run_native("SCloud", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))));
    if let Err(e) = &r {
        report_crash(&format!("Could not open the window: {e}"));
    }
    r
}
