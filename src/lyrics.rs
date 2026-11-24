//! Lyrics from LRCLIB (a free open database, often with per-line timestamps).
//! SoundCloud has no lyrics of its own, so we search by artist and title.

use crate::api::Track;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Lyrics {
    /// lines with their start time (s), if synced
    pub synced: Vec<(f64, String)>,
    /// plain lyrics, if not synced
    pub plain: Option<String>,
    /// where the lyrics came from: "LRCLIB", "Genius", "own"
    pub source: &'static str,
    /// the timestamps aren't real but estimated from line lengths (the lyrics had none)
    pub approx: bool,
}

/// Re-upload tags that get in the way of the lyrics search.
const NOISE: [&str; 14] = [
    "bass boosted", "bass boost", "slowed + reverb", "slowed+reverb", "slowed", "reverb",
    "sped up", "speed up", "nightcore", "official video", "official audio", "lyrics", "remastered",
    "8d audio",
];

/// A re-upload with changed speed: the original timestamps need stretching.
fn is_tempo_variant(t: &Track) -> bool {
    let l = t.title().to_lowercase();
    ["slowed", "sped up", "speed up", "nightcore", "speed"].iter().any(|k| l.contains(k))
}

fn strip_brackets(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Remove feat/prod and junk tags (case-insensitive, without breaking UTF-8).
fn strip_noise(s: &str) -> String {
    let mut out = strip_brackets(s);
    for k in [" feat.", " feat ", " ft.", " ft ", " prod.", " prod ", " | "] {
        if let Some(i) = find_ci(&out, k) {
            out.truncate(i);
        }
    }
    for k in NOISE {
        while let Some(i) = find_ci(&out, k) {
            out.replace_range(i..i + k.len(), " ");
        }
    }
    out.replace(['«', '»', '"', '“', '”'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == '-' || c == '–' || c == '—' || c == '|' || c.is_whitespace())
        .to_string()
}

/// Case-insensitive substring search; returns the byte index in the original string.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let n: Vec<char> = needle.to_lowercase().chars().collect();
    let idx: Vec<(usize, char)> = hay.char_indices().collect();
    'outer: for s in 0..idx.len() {
        let mut k = 0;
        for &(_, c) in &idx[s..] {
            if k == n.len() {
                break;
            }
            if c.to_lowercase().next() != Some(n[k]) {
                continue 'outer;
            }
            k += 1;
        }
        if k == n.len() {
            // the byte length must match the needle (true for ASCII keys)
            return Some(idx[s].0);
        }
    }
    None
}

/// "Artist - Track (feat. X) [prod. Y]" → ("Artist", "Track")
fn clean_names(t: &Track) -> (String, String) {
    let title = t.title();
    let (artist, song) = [" - ", " – ", " — ", " -- ", "-", "–", "—"]
        .iter()
        .find_map(|sep| title.split_once(sep))
        .filter(|(a, b)| !a.trim().is_empty() && !b.trim().is_empty())
        .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
        .unwrap_or_else(|| (t.artist(), title.clone()));
    let song = strip_noise(&song);
    let artist = strip_noise(&artist);
    let artist = artist
        .replace(" x ", ",")
        .replace(" X ", ",")
        .replace(" × ", ",");
    let artist = artist
        .split([',', '&'])
        .next()
        .map(str::trim)
        .filter(|a| a.len() > 1)
        .unwrap_or(artist.as_str())
        .trim()
        .to_string();
    (artist, if song.is_empty() { strip_noise(&title) } else { song })
}

pub fn parse_lrc(s: &str) -> Vec<(f64, String)> {
    let mut lines = Vec::new();
    for raw in s.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            let tag = &stripped[..end];
            rest = &stripped[end + 1..];
            let mut parts = tag.split(':');
            let (Some(m), Some(sec)) = (parts.next(), parts.next()) else {
                continue;
            };
            if let (Ok(m), Ok(sec)) = (m.trim().parse::<f64>(), sec.trim().parse::<f64>()) {
                times.push(m * 60.0 + sec);
            }
        }
        for t in times {
            lines.push((t, rest.trim().to_string()));
        }
    }
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    lines
}

fn from_record(v: &Value, scale: f64) -> Option<Lyrics> {
    let mut synced = v["syncedLyrics"].as_str().map(parse_lrc).unwrap_or_default();
    for l in &mut synced {
        l.0 *= scale;
    }
    let plain = v["plainLyrics"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            (!synced.is_empty()).then(|| synced.iter().map(|l| l.1.as_str()).collect::<Vec<_>>().join("\n"))
        });
    (!synced.is_empty() || plain.is_some()).then_some(Lyrics { synced, plain, source: "LRCLIB", approx: false })
}

fn http() -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) SCloud/0.2")
        .timeout(Duration::from_secs(12))
        .build()
        .ok()
}

/// Find lyrics (blocking, call it in the background). None = not found.
/// Order: own lyrics → LRCLIB (several query variants) → Genius (plain text only).
pub fn fetch(t: &Track) -> Option<Lyrics> {
    let dur = (t.full_duration.unwrap_or(t.duration) as f64 / 1000.0).clamp(1.0, 3600.0);
    let mut l = fetch_raw(t)?;
    // no timestamps: spread the lines over the track length so karaoke works
    if l.synced.is_empty() {
        if let Some(p) = &l.plain {
            l.synced = estimate(p, dur);
            l.approx = !l.synced.is_empty();
        }
    }
    Some(l)
}

/// Approximate timestamps: the intro is ~6% of the track, then lines follow in proportion to their length.
/// Subheadings like [Chorus] are skipped.
pub fn estimate(plain: &str, dur: f64) -> Vec<(f64, String)> {
    let lines: Vec<&str> = plain
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !(l.starts_with('[') && l.ends_with(']')))
        .collect();
    if lines.len() < 2 || dur < 20.0 {
        return Vec::new();
    }
    let weight = |l: &str| l.chars().count().max(10) as f64 + 8.0;
    let total: f64 = lines.iter().map(|l| weight(l)).sum();
    let (start, end) = (dur * 0.06, dur * 0.93);
    let mut t = start;
    let mut out = Vec::with_capacity(lines.len());
    for l in lines {
        out.push((t, l.to_string()));
        t += (end - start) * weight(l) / total;
    }
    out
}

fn fetch_raw(t: &Track) -> Option<Lyrics> {
    if let Some(own) = load_custom(t.id) {
        return Some(own);
    }
    // lyrics already aligned by the AI
    if let Some(lines) = crate::align::load_cache(t.id) {
        let plain = lines.iter().map(|l| l.1.as_str()).collect::<Vec<_>>().join("\n");
        return Some(Lyrics { synced: lines, plain: Some(plain), source: "AI", approx: false });
    }
    let http = http()?;
    let (artist, song) = clean_names(t);
    let dur = (t.full_duration.unwrap_or(t.duration) as f64 / 1000.0).clamp(1.0, 3600.0);
    let variant = is_tempo_variant(t);

    // 1) exact match by artist, title and duration
    if !variant {
        if let Ok(r) = http
            .get("https://lrclib.net/api/get")
            .query(&[
                ("artist_name", artist.as_str()),
                ("track_name", song.as_str()),
                ("duration", &format!("{}", dur.round() as u64)),
            ])
            .send()
        {
            if r.status().is_success() {
                if let Some(l) = r.json::<Value>().ok().and_then(|v| from_record(&v, 1.0)) {
                    return Some(l);
                }
            }
        }
    }

    // 2) search with several queries, collect candidates and pick the best
    let uploader = strip_noise(&t.artist());
    let full = strip_noise(&t.title());
    let mut queries: Vec<Vec<(&str, String)>> = vec![
        vec![("track_name", song.clone()), ("artist_name", artist.clone())],
        vec![("q", format!("{artist} {song}"))],
        vec![("track_name", song.clone()), ("artist_name", uploader.clone())],
        vec![("q", full.clone())],
    ];
    if song.chars().count() >= 4 {
        queries.push(vec![("track_name", song.clone())]);
    }
    let our_keys = our_artist_keys(t, &artist);
    let mut seen = std::collections::HashSet::new();
    let mut best: Option<(i64, Value, f64, bool)> = None;
    let song_l = song.to_lowercase();
    for q in queries {
        let qkey = format!("{q:?}");
        if !seen.insert(qkey) {
            continue;
        }
        let Ok(r) = http.get("https://lrclib.net/api/search").query(&q).send() else {
            continue;
        };
        let Ok(list) = r.json::<Vec<Value>>() else { continue };
        let only_title = q.len() == 1 && q[0].0 == "track_name";
        for v in list {
            if v["instrumental"].as_bool().unwrap_or(false) {
                continue;
            }
            let cd = v["duration"].as_f64().unwrap_or(0.0);
            if cd <= 0.0 {
                continue;
            }
            // make sure it's the same track: a similar title, and the artist matches
            // (LRCLIB search is fuzzy and easily returns someone else's song with the same word)
            let name = v["trackName"].as_str().unwrap_or("");
            let name_ok = title_sim(name, &song).max(title_sim(name, &full)) >= 0.75;
            let their = key(v["artistName"].as_str().unwrap_or(""));
            let artist_ok = !their.is_empty()
                && our_keys.iter().any(|a| their.contains(a.as_str()) || a.contains(&their));
            if !name_ok || !(artist_ok || (cd - dur).abs() <= 1.5) {
                continue;
            }
            let _ = &song_l;
            let diff = (cd - dur).abs();
            let ratio = dur / cd;
            // (acceptable, timestamp stretch, "inexact")
            let (ok, scale, loose) = if diff <= 4.0 {
                (true, 1.0, false) // the same version of the track
            } else if variant && (0.6..=1.5).contains(&ratio) {
                (true, ratio, false) // slowed / sped up: stretch
            } else if diff <= 30.0 {
                // a different version (live, another intro): stretch, but mark as inexact
                (true, ratio, true)
            } else {
                // very different duration: only usable as plain text
                (!only_title, 0.0, false)
            };
            if !ok {
                continue;
            }
            let synced = v["syncedLyrics"].as_str().is_some() && scale > 0.0;
            let mut score = (diff * 10.0) as i64;
            if loose {
                score += 20_000;
            }
            if !synced {
                score += 100_000;
            }
            if scale == 0.0 {
                score += 200_000;
            }
            if best.as_ref().map_or(true, |b| score < b.0) {
                best = Some((score, v, scale, loose));
            }
        }
        if best.as_ref().is_some_and(|b| b.0 < 20_000) {
            break; // there is already an exact synced match
        }
    }
    if let Some((_, v, scale, loose)) = best {
        let mut l = from_record(&v, if scale > 0.0 { scale } else { 1.0 })?;
        if scale == 0.0 {
            l.synced.clear(); // timestamps from a different version of the track: text only
        }
        l.approx = loose && !l.synced.is_empty();
        return Some(l);
    }

    // 3) Genius: no timestamps, but it has almost everything
    genius(&http, t, &artist, &song)
}

/// All of the track's artists as comparison keys: uploader, from the title, features.
fn our_artist_keys(t: &Track, artist: &str) -> Vec<String> {
    let mut v: Vec<String> = crate::wave::track_artist_names(t)
        .iter()
        .map(|a| key(a))
        .chain(std::iter::once(key(artist)))
        .filter(|a| a.len() > 1)
        .collect();
    v.dedup();
    v
}

/// Cyrillic → Latin (rough): so that «Платина» matches «Platina» on Genius.
pub fn translit(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let r = match c {
            'а' => "a", 'б' => "b", 'в' => "v", 'г' => "g", 'д' => "d", 'е' | 'ё' | 'э' => "e",
            'ж' => "zh", 'з' => "z", 'и' | 'й' | 'ы' => "i", 'к' => "k", 'л' => "l", 'м' => "m",
            'н' => "n", 'о' => "o", 'п' => "p", 'р' => "r", 'с' => "s", 'т' => "t", 'у' => "u",
            'ф' => "f", 'х' => "h", 'ц' => "c", 'ч' => "ch", 'ш' | 'щ' => "sh", 'ю' => "yu",
            'я' => "ya", 'ь' | 'ъ' => "", 'і' | 'ї' => "i", 'є' => "e", 'ґ' => "g",
            _ => {
                out.push(c);
                continue;
            }
        };
        out.push_str(r);
    }
    out
}

/// Normalization for comparison: lowercase, transliteration, letters and digits only.
pub fn key(s: &str) -> String {
    translit(&s.to_lowercase()).chars().filter(|c| c.is_alphanumeric()).collect()
}

fn words(s: &str) -> std::collections::HashSet<String> {
    translit(&s.to_lowercase())
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_string)
        .collect()
}

/// Title similarity 0..1.
fn title_sim(a: &str, b: &str) -> f32 {
    let (ka, kb) = (key(a), key(b));
    if ka.is_empty() || kb.is_empty() {
        return 0.0;
    }
    if ka == kb {
        return 1.0;
    }
    if ka.contains(&kb) || kb.contains(&ka) {
        return 0.8;
    }
    let (wa, wb) = (words(a), words(b));
    let inter = wa.intersection(&wb).count() as f32;
    let union = wa.union(&wb).count().max(1) as f32;
    inter / union
}

/// 64-bit dHash: a 9×8 grayscale image, compare neighboring pixels.
fn dhash(bytes: &[u8]) -> Option<u64> {
    let img = image::load_from_memory(bytes).ok()?;
    let g = image::imageops::resize(&img.to_luma8(), 9, 8, image::imageops::FilterType::Triangle);
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            h <<= 1;
            if g.get_pixel(x, y)[0] < g.get_pixel(x + 1, y)[0] {
                h |= 1;
            }
        }
    }
    Some(h)
}

fn fetch_hash(http: &reqwest::blocking::Client, url: &str) -> Option<u64> {
    let b = http.get(url).send().ok()?.bytes().ok()?;
    dhash(&b)
}

/// Genius: search with several queries and score each candidate by title, artists
/// (with transliteration) and artwork similarity to the SoundCloud artwork. No timestamps there.
fn genius(http: &reqwest::blocking::Client, t: &Track, artist: &str, song: &str) -> Option<Lyrics> {
    if song.is_empty() {
        return None;
    }
    let uploader = strip_noise(&t.artist());
    let full = strip_noise(&t.title());
    let mut queries = vec![format!("{artist} {song}"), format!("{uploader} {song}"), full.clone(), song.to_string()];
    queries.dedup();
    let our_artists = our_artist_keys(t, artist);

    struct Hit {
        url: String,
        thumb: Option<String>,
        score: f32,
    }
    let mut hits: Vec<Hit> = Vec::new();
    for q in queries.iter().filter(|q| !q.trim().is_empty()) {
        let Ok(r) = http
            .get("https://genius.com/api/search/song")
            .query(&[("q", q.as_str()), ("per_page", "8")])
            .send()
        else {
            continue;
        };
        let Ok(v) = r.json::<Value>() else { continue };
        let Some(list) = v["response"]["sections"][0]["hits"].as_array() else { continue };
        for h in list {
            let r = &h["result"];
            let Some(url) = r["url"].as_str() else { continue };
            if hits.iter().any(|x| x.url == url) {
                continue;
            }
            let title = r["title"].as_str().unwrap_or("");
            let names = format!(
                "{} {}",
                r["artist_names"].as_str().unwrap_or(""),
                r["primary_artist"]["name"].as_str().unwrap_or("")
            );
            let names_k = key(&names);
            let ts = title_sim(title, song).max(title_sim(title, &full) * 0.9);
            let artist_ok = our_artists.iter().any(|a| names_k.contains(a.as_str()) || a.contains(&names_k));
            let mut score = ts * 2.0 + if artist_ok { 1.5 } else { 0.0 };
            // Genius translations and "romanizations" are not what we want
            let low = names.to_lowercase();
            if low.contains("genius") && (low.contains("translation") || low.contains("romaniz")) {
                score -= 3.0;
            }
            hits.push(Hit {
                url: url.to_string(),
                thumb: r["song_art_image_thumbnail_url"].as_str().map(str::to_string),
                score,
            });
        }
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(4);

    // compare artwork: a similar picture is a strong sign it's the same track
    if let Some(ours) = t.artwork_url.as_ref().map(|u| u.replace("-large.", "-t67x67.")) {
        if let Some(our_hash) = fetch_hash(http, &ours) {
            std::thread::scope(|s| {
                let hs: Vec<_> = hits
                    .iter()
                    .map(|h| {
                        let th = h.thumb.clone();
                        s.spawn(move || th.and_then(|u| fetch_hash(http, &u)))
                    })
                    .collect();
                for (h, j) in hits.iter_mut().zip(hs) {
                    if let Some(theirs) = j.join().ok().flatten() {
                        let d = (our_hash ^ theirs).count_ones();
                        h.score += match d {
                            0..=10 => 2.0,
                            11..=16 => 1.0,
                            _ => 0.0,
                        };
                    }
                }
            });
            hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        }
    }

    let best = hits.into_iter().find(|h| h.score >= 2.6)?;
    let html = http.get(&best.url).send().ok()?.text().ok()?;
    let text = genius_extract(&html)?;
    Some(Lyrics { synced: Vec::new(), plain: Some(text), source: "Genius", approx: false })
}

/// Extract the contents of a <div ...> starting at the opening tag (honoring nesting).
fn div_body(html: &str, open_at: usize) -> Option<(usize, usize)> {
    let start = open_at + html[open_at..].find('>')? + 1;
    let mut depth = 1;
    let mut i = start;
    while depth > 0 {
        let next_open = html[i..].find("<div").map(|x| x + i);
        let next_close = html[i..].find("</div").map(|x| x + i)?;
        match next_open {
            Some(o) if o < next_close => {
                depth += 1;
                i = o + 4;
            }
            _ => {
                depth -= 1;
                i = next_close + 5;
                if depth == 0 {
                    return Some((start, next_close));
                }
            }
        }
    }
    None
}

fn genius_extract(html: &str) -> Option<String> {
    let mut out = String::new();
    let mut from = 0;
    while let Some(p) = html[from..].find("data-lyrics-container=\"true\"") {
        let attr = from + p;
        let open = html[..attr].rfind("<div")?;
        let (a, b) = div_body(html, open)?;
        let mut chunk = html[a..b].to_string();
        // cut service blocks (the "N Contributors…" header)
        while let Some(x) = chunk.find("data-exclude-from-selection=\"true\"") {
            let Some(o) = chunk[..x].rfind("<div") else { break };
            let Some((_, e)) = div_body(&chunk, o) else { break };
            let end = e + chunk[e..].find('>').map_or(0, |k| k + 1);
            chunk.replace_range(o..end, "");
        }
        out.push_str(&chunk);
        out.push_str("<br/>");
        from = b;
    }
    if out.is_empty() {
        return None;
    }
    let out = regex::Regex::new(r"(?i)<br\s*/?>").ok()?.replace_all(&out, "\n");
    let out = regex::Regex::new(r"<[^>]+>").ok()?.replace_all(&out, "");
    let mut text = decode_entities(&out);
    // remove consecutive blank lines
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn decode_entities(s: &str) -> String {
    let re = regex::Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|amp|lt|gt|quot|apos|nbsp);").unwrap();
    re.replace_all(s, |c: &regex::Captures| {
        let e = &c[1];
        match e {
            "amp" => "&".into(),
            "lt" => "<".into(),
            "gt" => ">".into(),
            "quot" => "\"".into(),
            "apos" => "'".into(),
            "nbsp" => " ".into(),
            _ => {
                let n = if let Some(h) = e.strip_prefix("#x") {
                    u32::from_str_radix(h, 16).ok()
                } else {
                    e[1..].parse().ok()
                };
                n.and_then(char::from_u32).map(String::from).unwrap_or_default()
            }
        }
    })
    .into_owned()
}

// ---------- own lyrics ----------

fn custom_dirs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = dirs::data_dir() {
        v.push(d.join("SCloud").join("lyrics"));
    }
    // a copy in Documents survives a reinstall
    if let Some(d) = dirs::document_dir() {
        v.push(d.join("SCloud").join("lyrics"));
    }
    v
}

/// Whether the text has [mm:ss.xx] timestamps
pub fn looks_like_lrc(s: &str) -> bool {
    s.lines().filter(|l| !parse_lrc(l).is_empty()).count() >= 2
}

pub fn load_custom(id: u64) -> Option<Lyrics> {
    for d in custom_dirs() {
        if let Ok(s) = std::fs::read_to_string(d.join(format!("{id}.lrc"))) {
            let synced = parse_lrc(&s);
            if !synced.is_empty() {
                let plain = synced.iter().map(|l| l.1.as_str()).collect::<Vec<_>>().join("\n");
                return Some(Lyrics { synced, plain: Some(plain), source: "own", approx: false });
            }
        }
        if let Ok(s) = std::fs::read_to_string(d.join(format!("{id}.txt"))) {
            if !s.trim().is_empty() {
                return Some(Lyrics { synced: Vec::new(), plain: Some(s.trim().to_string()), source: "own", approx: false });
            }
        }
    }
    None
}

/// Save own lyrics. With timestamps they are synced (.lrc), otherwise plain (.txt).
pub fn save_custom(id: u64, text: &str) -> Option<Lyrics> {
    delete_custom(id);
    let lrc = looks_like_lrc(text);
    for d in custom_dirs() {
        let _ = std::fs::create_dir_all(&d);
        let name = format!("{id}.{}", if lrc { "lrc" } else { "txt" });
        let _ = std::fs::write(d.join(name), text);
    }
    load_custom(id)
}

pub fn delete_custom(id: u64) {
    for d in custom_dirs() {
        let _ = std::fs::remove_file(d.join(format!("{id}.lrc")));
        let _ = std::fs::remove_file(d.join(format!("{id}.txt")));
    }
}

fn offsets_path() -> Option<std::path::PathBuf> {
    dirs::data_dir().map(|d| d.join("SCloud").join("lyrics").join("offsets.json"))
}

/// Saved lyrics offset for a track (s).
pub fn load_offset(id: u64) -> f64 {
    offsets_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<std::collections::HashMap<String, f64>>(&s).ok())
        .and_then(|m| m.get(&id.to_string()).copied())
        .unwrap_or(0.0)
}

pub fn save_offset(id: u64, off: f64) {
    let Some(p) = offsets_path() else { return };
    let mut m: std::collections::HashMap<String, f64> = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if off.abs() < 0.05 {
        m.remove(&id.to_string());
    } else {
        m.insert(id.to_string(), (off * 100.0).round() / 100.0);
    }
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, serde_json::to_string(&m).unwrap_or_default());
}

/// Lines → LRC text.
pub fn to_lrc(lines: &[(f64, String)]) -> String {
    lines
        .iter()
        .map(|(t, s)| {
            let t = t.max(0.0);
            format!("[{:02}:{:05.2}] {s}", (t / 60.0) as u64, t % 60.0)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc() {
        assert_eq!(strip_noise("Кинолента (bass boosted) slowed"), "Кинолента");
        let l = parse_lrc("[00:12.50] Первая\n[01:02.00][01:30.00] Припев\nбез тега");
        assert_eq!(l.len(), 3);
        assert!((l[0].0 - 12.5).abs() < 1e-6);
        assert_eq!(l[2].1, "Припев");
    }
}
