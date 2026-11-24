//! Aligns lyrics to the track with a neural network (whisper.cpp, runs locally and for free).
//!
//! 1. ffmpeg extracts the track audio into a 16 kHz mono WAV.
//! 2. whisper-cli recognizes words with a timestamp for every word.
//! 3. Lyric words are matched against recognized words (a diff-like alignment),
//!    each line takes the time of its first matched word, gaps are interpolated.
//!
//! Recognizing rap over beats is imperfect, but alignment only needs "anchors":
//! even 30–40% of recognized words give accurate line timing.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Folders where whisper may live: next to the app or in the setup cache.
fn whisper_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            v.push(d.join("whisper"));
        }
    }
    if let Some(d) = dirs::data_local_dir() {
        v.push(d.join("SCloud").join("deps").join("whisper"));
    }
    v
}

/// (whisper-cli, model) if installed.
pub fn whisper_paths() -> Option<(PathBuf, PathBuf)> {
    for d in whisper_dirs() {
        let cli = ["whisper-cli.exe", "whisper-cli", "main.exe"]
            .iter()
            .map(|n| d.join(n))
            .find(|p| p.exists());
        let Some(cli) = cli else { continue };
        // model: pick the largest ggml-*.bin (small is more accurate than base)
        let model = std::fs::read_dir(&d)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                n.starts_with("ggml-") && n.ends_with(".bin")
            })
            .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
        if let Some(model) = model {
            return Some((cli, model));
        }
    }
    None
}

pub fn available() -> bool {
    whisper_paths().is_some()
}

fn ffmpeg_path() -> PathBuf {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    if let Ok(exe) = std::env::current_exe() {
        let p = exe.with_file_name(name);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from(name)
}

fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // no console window
    }
    c
}

/// Recognition language, guessed from the lyrics' alphabet.
fn language(lines: &[String]) -> &'static str {
    let text: String = lines.concat();
    let total = text.chars().filter(|c| c.is_alphabetic()).count().max(1);
    let cyr = text.chars().filter(|c| ('а'..='я').contains(&c.to_lowercase().next().unwrap_or(' ')) || "ёіїєґ".contains(c.to_lowercase().next().unwrap_or(' '))).count();
    let uk = text.chars().filter(|c| "іїєґІЇЄҐ".contains(*c)).count();
    if cyr * 10 >= total * 3 {
        if uk >= 3 {
            "uk"
        } else {
            "ru"
        }
    } else {
        "en"
    }
}

struct Word {
    key: String,
    t: f64,
}

fn read_transcript(json: &Path) -> Result<Vec<Word>, String> {
    let s = std::fs::read_to_string(json).map_err(|e| format!("whisper не выдал результат: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&s).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for seg in v["transcription"].as_array().cloned().unwrap_or_default() {
        let from = seg["offsets"]["from"].as_f64().unwrap_or(0.0) / 1000.0;
        let text = seg["text"].as_str().unwrap_or("");
        // with -ml 1 a segment is usually one word, but split just in case
        for w in text.split_whitespace() {
            let k = crate::lyrics::key(w);
            if !k.is_empty() && !k.starts_with('[') {
                out.push(Word { key: k, t: from });
            }
        }
    }
    Ok(out)
}

/// Word similarity 0..1 via bigrams (robust to recognition errors).
fn sim(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    let bi = |s: &str| -> Vec<(char, char)> {
        let c: Vec<char> = s.chars().collect();
        c.windows(2).map(|w| (w[0], w[1])).collect()
    };
    let (x, y) = (bi(a), bi(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    let mut used = vec![false; y.len()];
    let mut common = 0;
    for p in &x {
        if let Some(i) = (0..y.len()).find(|&i| !used[i] && y[i] == *p) {
            used[i] = true;
            common += 1;
        }
    }
    2.0 * common as f32 / (x.len() + y.len()) as f32
}

/// Alignment (Needleman–Wunsch): for every lyric word, the index of the recognized word.
fn align_words(lyr: &[String], tr: &[Word]) -> Vec<Option<usize>> {
    let (n, m) = (lyr.len(), tr.len());
    let gap = -0.4_f32;
    let w = m + 1;
    let mut score = vec![0f32; (n + 1) * (m + 1)];
    let mut back = vec![0u8; (n + 1) * (m + 1)]; // 1 = diagonal, 2 = up, 3 = left
    for i in 1..=n {
        score[i * w] = gap * i as f32;
        back[i * w] = 2;
    }
    for j in 1..=m {
        score[j] = 0.0; // the start of the recognition (intro) is skipped for free
        back[j] = 3;
    }
    for i in 1..=n {
        for j in 1..=m {
            let s = sim(&lyr[i - 1], &tr[j - 1].key);
            let m_score = if s >= 0.5 { 1.0 + s } else { -0.6 };
            let d = score[(i - 1) * w + j - 1] + m_score;
            let u = score[(i - 1) * w + j] + gap;
            let l = score[i * w + j - 1] + if i == n { 0.0 } else { gap * 0.5 };
            let (best, b) = if d >= u && d >= l {
                (d, 1)
            } else if u >= l {
                (u, 2)
            } else {
                (l, 3)
            };
            score[i * w + j] = best;
            back[i * w + j] = b;
        }
    }
    let mut out = vec![None; n];
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        match back[i * w + j] {
            1 => {
                if sim(&lyr[i - 1], &tr[j - 1].key) >= 0.5 {
                    out[i - 1] = Some(j - 1);
                }
                i -= 1;
                j -= 1;
            }
            2 => i -= 1,
            _ => j -= 1,
        }
    }
    out
}

/// Line times from the alignment; lines without matches are interpolated by length.
fn line_times(lines: &[String], tr: &[Word], dur: f64) -> Result<Vec<f64>, String> {
    // lyric words with their line number and position in the line
    let mut lw: Vec<(usize, usize, String)> = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        for (pi, w) in l.split_whitespace().enumerate() {
            let k = crate::lyrics::key(w);
            if !k.is_empty() {
                lw.push((li, pi, k));
            }
        }
    }
    let keys: Vec<String> = lw.iter().map(|x| x.2.clone()).collect();
    let map = align_words(&keys, tr);

    // line anchor: median of "word time − position × 0.28 s" estimates
    let mut anchors: Vec<Option<f64>> = vec![None; lines.len()];
    for li in 0..lines.len() {
        let mut est: Vec<f64> = lw
            .iter()
            .zip(&map)
            .filter(|(x, _)| x.0 == li)
            .filter_map(|(x, m)| m.map(|j| tr[j].t - x.1 as f64 * 0.28))
            .collect();
        if est.is_empty() {
            continue;
        }
        est.sort_by(|a, b| a.total_cmp(b));
        anchors[li] = Some(est[est.len() / 2].max(0.0));
    }
    // drop anchors that break the order (false matches in repeated choruses)
    let mut last = -1.0;
    for a in anchors.iter_mut() {
        if let Some(t) = *a {
            if t <= last + 0.3 {
                *a = None;
            } else {
                last = t;
            }
        }
    }
    let found = anchors.iter().filter(|a| a.is_some()).count();
    let need = (lines.len() as f32 * 0.25).ceil() as usize;
    if found < need.max(2) {
        return Err(format!(
            "нейросеть узнала слишком мало строк ({found} из {})",
            lines.len()
        ));
    }

    // fill the gaps: between anchors, proportionally to line length
    let weight = |l: &str| l.chars().count().max(8) as f64 + 6.0;
    let mut out = vec![0.0; lines.len()];
    let idx: Vec<usize> = (0..lines.len()).filter(|&i| anchors[i].is_some()).collect();
    for &i in &idx {
        out[i] = anchors[i].unwrap_or(0.0);
    }
    // before the first anchor: step backwards by line length
    let first = idx[0];
    for i in (0..first).rev() {
        out[i] = (out[i + 1] - weight(&lines[i]) * 0.09).max(0.0);
    }
    for pair in idx.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let total: f64 = (a..b).map(|k| weight(&lines[k])).sum();
        let mut t = out[a];
        for k in a + 1..b {
            t += (out[b] - out[a]) * weight(&lines[k - 1]) / total;
            out[k] = t;
        }
    }
    let lastk = *idx.last().unwrap_or(&0);
    for i in lastk + 1..lines.len() {
        out[i] = (out[i - 1] + weight(&lines[i - 1]) * 0.09).min(dur.max(out[i - 1]));
    }
    Ok(out)
}

/// Align lines to the track. `progress` is 0..1. Blocking, call it in the background.
pub fn align(
    stream_url: &str,
    lines: &[String],
    dur: f64,
    progress: &dyn Fn(f32),
) -> Result<Vec<(f64, String)>, String> {
    let (cli, model) = whisper_paths().ok_or("нейросеть не установлена — запусти setup.bat")?;
    let tmp = std::env::temp_dir().join(format!(
        "scloud-align-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
    ));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let wav = tmp.join("a.wav");
    let out_base = tmp.join("out");
    let result = (|| {
        progress(0.02);
        // audio: mono 16 kHz, cut the lows (bass gets in the way of voice recognition)
        let st = command(ffmpeg_path())
            .args(["-y", "-loglevel", "error", "-i", stream_url, "-vn", "-af"])
            .arg("highpass=f=180,lowpass=f=5000,dynaudnorm")
            .args(["-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
            .arg(&wav)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("ffmpeg не запустился: {e}"))?;
        if !st.success() || !wav.exists() {
            return Err("ffmpeg не смог скачать звук трека".to_string());
        }
        progress(0.1);
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
        let mut child = command(&cli)
            .arg("-m")
            .arg(&model)
            .arg("-f")
            .arg(&wav)
            .args(["-l", language(lines), "-ml", "1", "-sow", "-oj", "-pp", "-t"])
            .arg(threads.to_string())
            .arg("-of")
            .arg(&out_base)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("whisper не запустился: {e}"))?;
        if let Some(err) = child.stderr.take() {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                // «whisper_print_progress_callback: progress =  45%»
                if let Some(p) = line.split("progress =").nth(1) {
                    if let Ok(v) = p.trim().trim_end_matches('%').trim().parse::<f32>() {
                        progress(0.1 + 0.85 * v / 100.0);
                    }
                }
            }
        }
        let st = child.wait().map_err(|e| e.to_string())?;
        if !st.success() {
            return Err("whisper завершился с ошибкой".to_string());
        }
        let tr = read_transcript(&out_base.with_extension("json"))?;
        if tr.len() < 5 {
            return Err("нейросеть почти ничего не расслышала (может, трек без слов?)".to_string());
        }
        progress(0.97);
        let times = line_times(lines, &tr, dur)?;
        Ok(times.into_iter().zip(lines.iter().cloned()).collect())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

// ---------- install right from the app ----------

fn download(
    http: &reqwest::blocking::Client,
    url: &str,
    to: &Path,
    progress: &dyn Fn(f32),
) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut r = http.get(url).send().map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("{url}: HTTP {}", r.status()));
    }
    let total = r.content_length().unwrap_or(0) as f64;
    let part = to.with_extension("part");
    let mut f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 16];
    let mut done = 0f64;
    loop {
        let n = r.read(&mut buf).map_err(|e| format!("обрыв загрузки: {e}"))?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as f64;
        if total > 0.0 {
            progress((done / total) as f32);
        }
    }
    drop(f);
    std::fs::rename(&part, to).map_err(|e| e.to_string())
}

/// Download whisper.cpp and the `base` model into %LOCALAPPDATA%\SCloud\deps\whisper.
/// `progress` is 0..1 for the whole process. Blocking.
pub fn install(progress: &dyn Fn(f32)) -> Result<(), String> {
    let dir = dirs::data_local_dir()
        .ok_or("нет папки AppData")?
        .join("SCloud")
        .join("deps")
        .join("whisper");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let http = reqwest::blocking::Client::builder()
        .user_agent("scloud-app")
        .timeout(None)
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;

    if !dir.join("whisper-cli.exe").exists() {
        let rel: serde_json::Value = http
            .get("https://api.github.com/repos/ggml-org/whisper.cpp/releases/latest")
            .send()
            .and_then(|r| r.json())
            .map_err(|e| format!("GitHub: {e}"))?;
        let assets = rel["assets"].as_array().cloned().unwrap_or_default();
        let name_of = |a: &serde_json::Value| a["name"].as_str().unwrap_or("").to_string();
        let asset = assets
            .iter()
            .find(|a| name_of(a) == "whisper-bin-x64.zip")
            .or_else(|| {
                assets.iter().find(|a| {
                    let n = name_of(a);
                    n.contains("bin-x64") && n.ends_with(".zip") && !n.contains("blas")
                })
            })
            .ok_or("в релизе whisper.cpp нет whisper-bin-x64.zip")?;
        let url = asset["browser_download_url"].as_str().ok_or("нет ссылки на архив")?;
        let zip = std::env::temp_dir().join("scloud-whisper.zip");
        let unz = std::env::temp_dir().join("scloud-whisper");
        download(&http, url, &zip, &|p| progress(p * 0.08))?;
        let _ = std::fs::remove_dir_all(&unz);
        // extract with the built-in PowerShell (no zip crate needed)
        let st = command("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
                zip.display(),
                unz.display()
            ))
            .status()
            .map_err(|e| format!("не запустился PowerShell: {e}"))?;
        if !st.success() {
            return Err("не получилось распаковать whisper".into());
        }
        // find the folder with whisper-cli.exe and copy everything from it (exe + dll)
        fn find(dir: &Path) -> Option<PathBuf> {
            for e in std::fs::read_dir(dir).ok()?.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if let Some(x) = find(&p) {
                        return Some(x);
                    }
                } else if p.file_name().is_some_and(|n| n == "whisper-cli.exe") {
                    return p.parent().map(Path::to_path_buf);
                }
            }
            None
        }
        let bin = find(&unz).ok_or("в архиве нет whisper-cli.exe")?;
        for e in std::fs::read_dir(&bin).map_err(|e| e.to_string())?.flatten() {
            if e.path().is_file() {
                let _ = std::fs::copy(e.path(), dir.join(e.file_name()));
            }
        }
        let _ = std::fs::remove_file(&zip);
        let _ = std::fs::remove_dir_all(&unz);
    }
    progress(0.1);
    let model = dir.join("ggml-base.bin");
    let has_model = std::fs::read_dir(&dir)
        .map(|r| {
            r.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("ggml-") && n.ends_with(".bin")
            })
        })
        .unwrap_or(false);
    if !has_model {
        download(
            &http,
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
            &model,
            &|p| progress(0.1 + 0.9 * p),
        )?;
    }
    progress(1.0);
    if available() {
        Ok(())
    } else {
        Err("после установки whisper не найден".into())
    }
}

// ---------- cache ----------

fn cache_path(id: u64) -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("SCloud").join("lyrics").join(format!("{id}.ai.lrc")))
}

pub fn save_cache(id: u64, lines: &[(f64, String)]) {
    if let Some(p) = cache_path(id) {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, crate::lyrics::to_lrc(lines));
    }
}

pub fn load_cache(id: u64) -> Option<Vec<(f64, String)>> {
    let s = std::fs::read_to_string(cache_path(id)?).ok()?;
    let l = crate::lyrics::parse_lrc(&s);
    (!l.is_empty()).then_some(l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_lines() {
        let lines: Vec<String> = ["раз два три", "четыре пять", "шесть семь восемь"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let tr: Vec<Word> = [("раз", 5.0), ("два", 5.3), ("три", 5.6), ("четыре", 9.0), ("пят", 9.4), ("шест", 14.0), ("восемь", 14.6)]
            .iter()
            .map(|(k, t)| Word { key: crate::lyrics::key(k), t: *t })
            .collect();
        let t = line_times(&lines, &tr, 60.0).unwrap();
        assert!((t[0] - 5.0).abs() < 0.1);
        assert!((t[1] - 9.0).abs() < 0.2);
        assert!((t[2] - 14.0).abs() < 0.2);
    }
}
