//! Windows system media controls (SMTC): ⏯ ⏭ ⏮ keys on keyboards and headphones,
//! the overlay with artwork next to the volume flyout and on the lock screen.

use std::sync::mpsc::{channel, Receiver};
#[cfg(windows)]
use std::time::{Duration, Instant};

pub enum Cmd {
    Play,
    Pause,
    Toggle,
    Next,
    Prev,
    SeekTo(f64),
    SeekBy(f64),
}

pub struct Media {
    #[cfg(windows)]
    controls: Option<souvlaki::MediaControls>,
    rx: Receiver<Cmd>,
    #[cfg(windows)]
    last: Option<(u64, bool, bool)>,
    #[cfg(windows)]
    last_sent: Instant,
}

impl Media {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        #[allow(unused_variables)]
        let (tx, rx) = channel::<Cmd>();
        #[cfg(windows)]
        {
            let controls = create(cc, tx, cc.egui_ctx.clone());
            Self {
                controls,
                rx,
                last: None,
                last_sent: Instant::now(),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = cc;
            Self { rx }
        }
    }

    /// Commands from the keyboard/system accumulated since the last frame.
    pub fn poll(&mut self) -> Vec<Cmd> {
        self.rx.try_iter().collect()
    }

    /// Update the overlay: on track change, pause, when the duration appears and every 5 s (position).
    #[allow(clippy::too_many_arguments, unused_variables)]
    pub fn update(
        &mut self,
        id: u64,
        title: &str,
        artist: &str,
        cover: Option<&str>,
        dur: f64,
        pos: f64,
        paused: bool,
    ) {
        #[cfg(windows)]
        {
            use souvlaki::{MediaMetadata, MediaPlayback, MediaPosition};
            let Some(c) = self.controls.as_mut() else { return };
            let key = (id, paused, dur > 0.0);
            if self.last == Some(key) && self.last_sent.elapsed() < Duration::from_secs(5) {
                return;
            }
            let track_changed = self.last.map(|l| (l.0, l.2)) != Some((id, dur > 0.0));
            if track_changed && id != 0 {
                let _ = c.set_metadata(MediaMetadata {
                    title: Some(title),
                    artist: Some(artist),
                    album: Some("SCloud"),
                    cover_url: cover,
                    duration: (dur > 0.0).then(|| Duration::from_secs_f64(dur)),
                });
            }
            let progress = Some(MediaPosition(Duration::from_secs_f64(pos.max(0.0))));
            let playback = if id == 0 {
                MediaPlayback::Stopped
            } else if paused {
                MediaPlayback::Paused { progress }
            } else {
                MediaPlayback::Playing { progress }
            };
            let _ = c.set_playback(playback);
            self.last = Some(key);
            self.last_sent = Instant::now();
        }
    }
}

#[cfg(windows)]
fn create(
    cc: &eframe::CreationContext<'_>,
    tx: std::sync::mpsc::Sender<Cmd>,
    ctx: eframe::egui::Context,
) -> Option<souvlaki::MediaControls> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use souvlaki::{MediaControlEvent as E, MediaControls, PlatformConfig, SeekDirection};

    let handle = cc.window_handle().ok()?;
    let hwnd = match handle.as_raw() {
        RawWindowHandle::Win32(w) => w.hwnd.get() as *mut std::ffi::c_void,
        _ => return None,
    };
    let config = PlatformConfig {
        dbus_name: "scloud",
        display_name: "SCloud",
        hwnd: Some(hwnd),
    };
    let mut controls = MediaControls::new(config).ok()?;
    controls
        .attach(move |e: E| {
            let secs = |dir: SeekDirection, d: f64| {
                if matches!(dir, SeekDirection::Forward) {
                    d
                } else {
                    -d
                }
            };
            let cmd = match e {
                E::Play => Cmd::Play,
                E::Pause => Cmd::Pause,
                E::Toggle => Cmd::Toggle,
                E::Next => Cmd::Next,
                E::Previous => Cmd::Prev,
                E::SetPosition(p) => Cmd::SeekTo(p.0.as_secs_f64()),
                E::SeekBy(dir, d) => Cmd::SeekBy(secs(dir, d.as_secs_f64())),
                E::Seek(dir) => Cmd::SeekBy(secs(dir, 10.0)),
                _ => return,
            };
            let _ = tx.send(cmd);
            ctx.request_repaint();
        })
        .ok()?;
    Some(controls)
}
