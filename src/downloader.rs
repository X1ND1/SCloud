//! Downloading tracks as MP3 with ffmpeg: artwork and tags inside the file,
//! with FX enabled the effects are baked in (slowed, reverb, bass boost…).

use crate::api::{Client, Track};
use crate::fx::Fx;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub enum DlEvent {
    Progress(u64, f32),
    Done(u64, PathBuf),
    Failed(u64, String),
}

pub enum DlState {
    Running(f32),
    Done(PathBuf),
    Failed(String),
}

/// Default folder: Music\\SCloud.
pub fn default_download_dir() -> PathBuf {
    dirs::audio_dir()
        .or_else(dirs::download_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("SCloud")
}

fn ffmpeg_path() -> PathBuf {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    if let Ok(exe) = std::env::current_exe() {
        let p = exe.with_file_name(name);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from(name) // from PATH
}

fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no black console window
    }
    c
}

fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if r#"<>:"/\|?*"#.contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    cleaned.chars().take(150).collect()
}

pub fn download(api: &Client, t: &Track, fx: Fx, dir: PathBuf, notify: &dyn Fn(DlEvent)) {
    match run(api, t, fx, dir, notify) {
        Ok(p) => notify(DlEvent::Done(t.id, p)),
        Err(e) => notify(DlEvent::Failed(t.id, e)),
    }
}

fn run(api: &Client, t: &Track, fx: Fx, dir: PathBuf, notify: &dyn Fn(DlEvent)) -> Result<PathBuf, String> {
    let url = api.stream_url_fresh(t)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let mut title = t.title();
    if let Some(l) = fx.label() {
        title = format!("{title} ({l})");
    }
    let artist = t.artist();
    let out = dir.join(format!("{}.mp3", sanitize(&format!("{artist} - {title}"))));

    // artwork into a temp file
    let cover: Option<PathBuf> = t.artwork("t500x500").and_then(|u| {
        let bytes = api.fetch_bytes(&u).ok()?;
        let p = std::env::temp_dir().join(format!("scloud-cover-{}.jpg", t.id));
        std::fs::write(&p, bytes).ok()?;
        Some(p)
    });

    let mut cmd = command(ffmpeg_path());
    cmd.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostats",
        "-progress",
        "pipe:1",
    ]);
    cmd.arg("-i").arg(&url);
    if let Some(c) = &cover {
        cmd.arg("-i").arg(c);
    }
    cmd.args(["-map", "0:a:0"]);
    if cover.is_some() {
        cmd.args([
            "-map",
            "1:0",
            "-c:v",
            "copy",
            "-disposition:v:0",
            "attached_pic",
            "-metadata:s:v",
            "title=Album cover",
            "-metadata:s:v",
            "comment=Cover (front)",
        ]);
    }
    if let Some(f) = fx.ffmpeg_filter() {
        cmd.arg("-af").arg(f);
    }
    cmd.args(["-c:a", "libmp3lame", "-b:a", "192k", "-id3v2_version", "3"]);
    cmd.arg("-metadata").arg(format!("title={title}"));
    cmd.arg("-metadata").arg(format!("artist={artist}"));
    if let Some(g) = t.genre.as_deref().filter(|g| !g.is_empty()) {
        cmd.arg("-metadata").arg(format!("genre={g}"));
    }
    if let Some(u) = &t.permalink_url {
        cmd.arg("-metadata").arg(format!("comment={u}"));
    }
    cmd.arg(&out);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        crate::i18n::tf(
            "ffmpeg не найден ({e}). Запусти setup.bat ещё раз — он его скачает",
            &[("{e}", format!("{e}"))],
        )
    })?;

    // progress: ffmpeg writes out_time_us=… to stdout
    let total = (t.full_duration.unwrap_or(t.duration) as f64 / 1000.0) / fx.speed.max(0.1) as f64;
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let v = line
                .strip_prefix("out_time_us=")
                .or_else(|| line.strip_prefix("out_time_ms="));
            if let (Some(v), true) = (v, total > 0.0) {
                if let Ok(us) = v.trim().parse::<f64>() {
                    notify(DlEvent::Progress(
                        t.id,
                        ((us / 1e6) / total).clamp(0.0, 1.0) as f32,
                    ));
                }
            }
        }
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if let Some(c) = cover {
        let _ = std::fs::remove_file(c);
    }
    if output.status.success() {
        Ok(out)
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(err
            .lines()
            .last()
            .unwrap_or(crate::i18n::t("ошибка ffmpeg"))
            .to_string())
    }
}

/// Open a folder (or reveal a file) in Explorer.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        let _ = if path.is_file() {
            Command::new("explorer")
                .arg(format!("/select,{}", path.display()))
                .spawn()
        } else {
            Command::new("explorer").arg(path).spawn()
        };
    }
    #[cfg(not(windows))]
    {
        let dir = if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        let _ = Command::new("xdg-open").arg(dir).spawn();
    }
}
