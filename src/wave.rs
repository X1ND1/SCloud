//! "Wave": an endless recommendation feed based on what you listen to.
//!
//! Where the candidates come from:
//!   1. related tracks for tracks you played / liked;
//!   2. artists from features: "A & B", "A x B", "feat./ft.", "prod. by": search their tracks;
//!   3. other tracks by the uploaders you listen to most.
//! Then everything is ranked by your taste (artist frequency, likes, skips, 👎),
//! recently played tracks and Go+ previews are dropped, and no more than 2 tracks by one artist in a row.

use crate::api::{Client, Track};
use crate::library::Library;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WaveKind {
    /// from recent plays, more new artists
    Day,
    /// from likes and favorite artists, more familiar music
    Taste,
    /// "Track radio": everything around one chosen track
    Radio,
}

impl WaveKind {
    pub fn title(self) -> &'static str {
        match self {
            WaveKind::Day => crate::i18n::t("Волна дня"),
            WaveKind::Taste => crate::i18n::t("Волна твоего вкуса"),
            WaveKind::Radio => crate::i18n::t("Радио по треку"),
        }
    }
}

/// Wave mood (like the "My Wave" modes): changes what gets picked and how it's ranked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaveMode {
    /// a bit of everything: the regular wave
    Mix,
    /// unfamiliar: new artists
    Discover,
    /// favorites: your artists and their features
    Favorite,
    /// popular
    Popular,
    Calm,
    Energetic,
    /// no words: instrumentals, lo-fi, ambient
    NoWords,
}

impl WaveMode {
    pub const ALL: [WaveMode; 7] = [
        WaveMode::Mix,
        WaveMode::Discover,
        WaveMode::Favorite,
        WaveMode::Popular,
        WaveMode::Calm,
        WaveMode::Energetic,
        WaveMode::NoWords,
    ];

    pub fn title(self) -> &'static str {
        crate::i18n::t(match self {
            WaveMode::Mix => "Всё вперемешку",
            WaveMode::Discover => "Незнакомое",
            WaveMode::Favorite => "Любимое",
            WaveMode::Popular => "Популярное",
            WaveMode::Calm => "Спокойное",
            WaveMode::Energetic => "Бодрое",
            WaveMode::NoWords => "100% музыки, 0% слов",
        })
    }

    /// Which kind of wave the mode is based on.
    pub fn kind(self) -> WaveKind {
        match self {
            WaveMode::Discover => WaveKind::Day,
            _ => WaveKind::Taste,
        }
    }

    /// Genre/title keywords for the mood modes.
    fn keywords(self) -> &'static [&'static str] {
        match self {
            WaveMode::Calm => &[
                "ambient", "chill", "lofi", "lo-fi", "acoustic", "piano", "soul", "r&b", "rnb", "jazz",
                "sad", "slowed", "dream", "indie", "classical", "лирика",
            ],
            WaveMode::Energetic => &[
                "phonk", "drill", "trap", "edm", "dance", "house", "techno", "drum", "dnb", "hardstyle",
                "rock", "metal", "hyperpop", "sped up", "speed up", "club", "rage", "workout",
            ],
            WaveMode::NoWords => &[
                "instrumental", "instrumentals", "ambient", "lofi", "lo-fi", "beat", "beats", "classical",
                "piano", "soundtrack", "score", "minus", "минус", "инструментал", "orchestral",
            ],
            _ => &[],
        }
    }

    /// Search queries so the wave surely has tracks of the right mood.
    fn queries(self) -> &'static [&'static str] {
        match self {
            WaveMode::Calm => &["chill lofi", "ambient chill", "sad slowed", "acoustic chill", "night drive chill"],
            WaveMode::Energetic => &["phonk", "drill", "edm festival", "rage trap", "drum and bass", "hardstyle"],
            WaveMode::NoWords => &["lofi beats instrumental", "ambient instrumental", "piano instrumental", "phonk instrumental", "chillhop"],
            _ => &[],
        }
    }

    fn matches(self, t: &Track) -> bool {
        let hay = format!(
            "{} {}",
            t.genre.as_deref().unwrap_or("").to_lowercase(),
            t.title().to_lowercase()
        );
        self.keywords().iter().any(|k| hay.contains(k))
    }
}

/// Everything the generator needs (a snapshot, so it can run on a background thread).
pub struct Profile {
    kind: WaveKind,
    mode: WaveMode,
    seeds: Vec<Track>,
    /// artist → taste weight
    artists: HashMap<String, f32>,
    /// artists found in the features of your tracks
    feats: HashMap<String, f32>,
    /// ids of uploaders you listen to → weight
    uploaders: HashMap<u64, f32>,
    exclude: HashSet<u64>,
    /// artists that just played: don't repeat them back to back
    recent_artists: HashSet<String>,
}

fn norm(s: &str) -> String {
    s.trim()
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '$')
        .to_lowercase()
}

fn re_parens() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[\(\[]([^\)\]]*)[\)\]]").unwrap())
}

fn re_feat() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)\b(?:feat\.?|ft\.?|featuring|prod\.?\s*by|prod\.?|при\s+уч\.?|w/)\s*(.+)$").unwrap()
    })
}

fn re_split() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)\s*(?:,|&|\+|\band\b|\bи\b|\bx\b|\bх\b|\bfeat\.?|\bft\.?|\bwith\b|\bw/|\bvs\.?)\s*")
            .unwrap()
    })
}

/// Names in their original case; duplicates are dropped by their normalized form.
fn split_names(s: &str, out: &mut Vec<String>) {
    for part in re_split().split(s) {
        let display = part
            .trim()
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '$')
            .to_string();
        let n = norm(&display);
        let len = n.chars().count();
        if (2..=40).contains(&len) && !out.iter().any(|x| norm(x) == n) {
            out.push(display);
        }
    }
}

/// All of the track's artists, normalized (for comparison).
pub fn track_artists(t: &Track) -> Vec<String> {
    track_artist_names(t).iter().map(|a| norm(a)).collect()
}

/// All of the track's artists as written: the uploader + features and producers from the title.
pub fn track_artist_names(t: &Track) -> Vec<String> {
    let mut out = Vec::new();
    split_names(&t.artist(), &mut out);
    let title = t.title();

    // "Artists - Title"
    let (left, song) = ["-", "–", "—"]
        .iter()
        .find_map(|sep| title.split_once(&format!(" {sep} ")))
        .unwrap_or(("", title.as_str()));
    if !left.is_empty() {
        split_names(left, &mut out);
    }
    // (feat. X), [prod. by Y]
    for cap in re_parens().captures_iter(&title) {
        if let Some(f) = re_feat().captures(&cap[1]) {
            split_names(&f[1], &mut out);
        }
    }
    // "Title feat. X" without brackets
    let song_no_parens = re_parens().replace_all(song, "");
    if let Some(f) = re_feat().captures(&song_no_parens) {
        split_names(&f[1], &mut out);
    }
    out
}

/// Only "guest" artists (not the uploader).
fn feat_artists(t: &Track) -> Vec<String> {
    let owner = norm(&t.artist());
    track_artists(t).into_iter().filter(|a| *a != owner).collect()
}

fn rand_f32() -> f32 {
    // xorshift seeded by the time: no external crates, good enough for us
    static SEED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut x = SEED.load(std::sync::atomic::Ordering::Relaxed);
    if x == 0 {
        x = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42)
            | 1;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    SEED.store(x, std::sync::atomic::Ordering::Relaxed);
    (x % 1_000_000) as f32 / 1_000_000.0
}

/// Pick up to `n` items with probability proportional to their weight.
fn weighted_pick<T: Clone>(items: &[(T, f32)], n: usize) -> Vec<T> {
    let mut pool: Vec<(T, f32)> = items.iter().filter(|x| x.1 > 0.0).cloned().collect();
    let mut out = Vec::new();
    while out.len() < n && !pool.is_empty() {
        let total: f32 = pool.iter().map(|x| x.1).sum();
        let mut r = rand_f32() * total;
        let mut idx = pool.len() - 1;
        for (i, x) in pool.iter().enumerate() {
            if r < x.1 {
                idx = i;
                break;
            }
            r -= x.1;
        }
        out.push(pool.remove(idx).0);
    }
    out
}

impl Profile {
    /// `in_queue`: tracks already in the wave queue (to avoid repeats).
    /// `fallback`: tracks to use if you haven't listened to anything yet (trending from the home page).
    /// `radio`: the track "Track radio" is built from.
    pub fn build(
        kind: WaveKind,
        lib: &Library,
        in_queue: &[Track],
        fallback: &[Track],
        radio: Option<&Track>,
    ) -> Self {
        let mut artists: HashMap<String, f32> = HashMap::new();
        let mut feats: HashMap<String, f32> = HashMap::new();
        let mut uploaders: HashMap<u64, f32> = HashMap::new();

        let (hist_w, like_w, hist_n) = match kind {
            WaveKind::Day | WaveKind::Radio => (1.0, 0.6, 30),
            WaveKind::Taste => (0.5, 1.5, 200),
        };
        for (i, t) in lib.history.iter().take(hist_n).enumerate() {
            // recent plays weigh more
            let w = hist_w * (1.0 - i as f32 / (hist_n as f32 * 1.5));
            for a in track_artists(t) {
                *artists.entry(a).or_default() += w;
            }
            for a in feat_artists(t) {
                *feats.entry(a).or_default() += w;
            }
            if let Some(uid) = t.user.as_ref().and_then(|u| u.id) {
                *uploaders.entry(uid).or_default() += w;
            }
        }
        for t in lib.likes.iter().take(300) {
            for a in track_artists(t) {
                *artists.entry(a).or_default() += like_w;
            }
            for a in feat_artists(t) {
                *feats.entry(a).or_default() += like_w;
            }
            if let Some(uid) = t.user.as_ref().and_then(|u| u.id) {
                *uploaders.entry(uid).or_default() += like_w;
            }
        }
        for (a, fb) in &lib.artist_feedback {
            *artists.entry(a.clone()).or_default() += fb;
        }
        // smoothing: an artist you played 50 times in a row shouldn't take over the whole wave
        for v in artists.values_mut().chain(feats.values_mut()) {
            *v = v.signum() * v.abs().sqrt();
        }
        for v in uploaders.values_mut() {
            *v = v.sqrt();
        }

        let seeds: Vec<Track> = match kind {
            WaveKind::Day | WaveKind::Radio => lib
                .history
                .iter()
                .take(60)
                .chain(lib.likes.iter().take(30))
                .cloned()
                .collect(),
            WaveKind::Taste => lib
                .likes
                .iter()
                .take(200)
                .chain(lib.history.iter().take(60))
                .cloned()
                .collect(),
        };

        // radio: start from one track; its artists and features matter most
        let seeds = match (kind, radio) {
            (WaveKind::Radio, Some(r)) => {
                for a in track_artists(r) {
                    *artists.entry(a).or_default() += 4.0;
                }
                for a in feat_artists(r) {
                    *feats.entry(a).or_default() += 4.0;
                }
                if let Some(uid) = r.user.as_ref().and_then(|u| u.id) {
                    *uploaders.entry(uid).or_default() += 4.0;
                }
                vec![r.clone(); 3] // take related tracks from it several times: different samples
            }
            _ => seeds,
        };
        let seeds = if seeds.is_empty() {
            fallback.iter().take(15).cloned().collect()
        } else {
            seeds
        };

        let mut exclude: HashSet<u64> = lib.disliked.iter().copied().collect();
        // don't repeat recently played tracks (stricter for "Daily wave")
        let recent = if kind == WaveKind::Day { 60 } else { 25 };
        exclude.extend(lib.history.iter().take(recent).map(|t| t.id));
        exclude.extend(in_queue.iter().map(|t| t.id));
        let recent_artists: HashSet<String> = in_queue
            .iter()
            .rev()
            .take(12)
            .chain(lib.history.iter().take(6))
            .flat_map(track_artists)
            .collect();

        Self {
            kind,
            mode: WaveMode::Mix,
            seeds,
            artists,
            feats,
            uploaders,
            exclude,
            recent_artists,
        }
    }

    pub fn with_mode(mut self, mode: WaveMode) -> Self {
        self.mode = mode;
        self
    }
}

/// Generate a batch of tracks (blocking, call it in the background).
pub fn generate(api: &Client, p: &Profile, count: usize) -> Vec<Track> {
    // --- choose what to start from ---
    let seed_items: Vec<(Track, f32)> = p
        .seeds
        .iter()
        .map(|t| {
            let w: f32 = track_artists(t)
                .iter()
                .map(|a| p.artists.get(a).copied().unwrap_or(0.0))
                .sum();
            (t.clone(), w.max(0.2))
        })
        .collect();
    // seeds come from different artists: weights are smoothed, at most one seed track per artist
    let seed_items: Vec<(Track, f32)> = seed_items.into_iter().map(|(t, w)| (t, w.sqrt())).collect();
    let mut seeds: Vec<Track> = Vec::new();
    let mut seed_artists: HashSet<String> = HashSet::new();
    let radio = p.kind == WaveKind::Radio;
    for t in weighted_pick(&seed_items, seed_items.len().min(60)) {
        let arts = track_artists(&t);
        if !radio && arts.iter().any(|a| seed_artists.contains(a)) {
            continue;
        }
        seed_artists.extend(arts);
        seeds.push(t);
        if seeds.len() >= if radio { 3 } else { 8 } {
            break;
        }
    }

    let mut feat_items: Vec<(String, f32)> = p.feats.iter().map(|(k, v)| (k.clone(), *v)).collect();
    feat_items.retain(|x| p.artists.get(&x.0).copied().unwrap_or(0.0) > -1.0);
    let feat_names = weighted_pick(&feat_items, 3);

    let up_items: Vec<(u64, f32)> = p.uploaders.iter().map(|(k, v)| (*k, *v)).collect();
    let uploaders = weighted_pick(
        &up_items,
        match (p.kind, p.mode) {
            (_, WaveMode::Favorite) => 3,
            (WaveKind::Radio, _) => 2,
            _ => 1,
        },
    );

    // --- collect candidates in parallel ---
    // (track, source: 0 = related, 1 = feature, 2 = uploader)
    let mut candidates: Vec<(Track, u8)> = Vec::new();
    std::thread::scope(|s| {
        let mut handles = Vec::new();
        for t in &seeds {
            handles.push(s.spawn(move || (api.related(t.id).unwrap_or_default(), 0u8)));
        }
        for name in &feat_names {
            handles.push(s.spawn(move || {
                let found = api.search_tracks(name, 0).map(|p| p.tracks).unwrap_or_default();
                // take only tracks this artist actually appears on
                let tracks = found
                    .into_iter()
                    .filter(|t| track_artists(t).iter().any(|a| a == name))
                    .collect();
                (tracks, 1u8)
            }));
        }
        for uid in &uploaders {
            handles.push(s.spawn(move || (api.user_tracks(*uid, 20).unwrap_or_default(), 2u8)));
        }
        // mood mode: add tracks via genre search queries
        let qs = p.mode.queries();
        if !qs.is_empty() {
            let start = (rand_f32() * qs.len() as f32) as usize;
            for k in 0..2 {
                let q = qs[(start + k) % qs.len()];
                handles.push(s.spawn(move || {
                    (api.search_tracks(q, 0).map(|p| p.tracks).unwrap_or_default(), 3u8)
                }));
            }
        }
        for h in handles {
            if let Ok((tracks, src)) = h.join() {
                candidates.extend(tracks.into_iter().map(|t| (t, src)));
            }
        }
    });

    // --- ranking ---
    let max_aff = p.artists.values().copied().fold(1.0f32, f32::max);
    let mut seen = HashSet::new();
    let mut scored: Vec<(f32, Track)> = Vec::new();
    for (t, src) in candidates {
        if p.exclude.contains(&t.id) || !seen.insert(t.id) || t.is_preview() || !t.is_playable() {
            continue;
        }
        let arts = track_artists(&t);
        // the "familiarity" contribution is logarithmic so a favorite artist doesn't outweigh everything
        let raw: f32 = arts
            .iter()
            .map(|a| p.artists.get(a).copied().unwrap_or(0.0))
            .fold(0.0, f32::max);
        let affinity = raw.signum() * (1.0 + raw.abs()).ln() / (1.0 + max_aff).ln();
        let known = arts
            .iter()
            .any(|a| p.artists.get(a).copied().unwrap_or(0.0) > 0.3);
        let has_feat = arts.iter().any(|a| p.feats.contains_key(a));
        if arts
            .iter()
            .any(|a| p.artists.get(a).copied().unwrap_or(0.0) < -2.5)
        {
            continue; // you keep skipping this artist
        }

        let mut score = match src {
            0 => 1.0,
            1 => 0.9,
            2 => 0.7,
            _ => 0.6,
        };
        let plays = t.playback_count.unwrap_or(0) as f32;
        match p.mode {
            WaveMode::Discover if known => score -= 1.2,
            WaveMode::Discover => score += 0.4,
            WaveMode::Favorite if known || has_feat => score += 1.0,
            WaveMode::Favorite => score -= 0.4,
            WaveMode::Popular => score += (plays.max(1.0).log10() - 4.0).clamp(-1.0, 2.0) * 0.6,
            WaveMode::Calm | WaveMode::Energetic | WaveMode::NoWords => {
                if p.mode.matches(&t) {
                    score += 1.4;
                } else {
                    score -= 0.9;
                }
            }
            WaveMode::Mix => {}
        }
        score += affinity.clamp(-1.0, 1.0)
            * match (p.kind, p.mode) {
                (_, WaveMode::Favorite) => 1.4,
                (WaveKind::Taste, _) => 0.7,
                _ => 0.4,
            };
        // artists that just played: push them back
        if arts.iter().any(|a| p.recent_artists.contains(a)) {
            score -= 0.9;
        }
        if has_feat {
            score += 0.4;
        }
        if p.kind == WaveKind::Day && !known {
            score += 0.35; // discoveries of the day
        }
        // a bit of popularity so tracks with 12 plays don't creep in
        score += (plays.max(1.0).log10() / 7.0).min(0.5);
        score += rand_f32() * 0.8; // variety
        scored.push((score, t));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // --- variety: one track per artist per batch (up to two in "Favorites" and radio),
    // all of the track's artists count, features included ---
    let cap: u8 = if p.mode == WaveMode::Favorite || radio { 2 } else { 1 };
    let mut per_artist: HashMap<String, u8> = HashMap::new();
    let mut out = Vec::new();
    let mut rest = Vec::new();
    for (_, t) in scored {
        let mut arts = track_artists(&t);
        if arts.is_empty() {
            arts.push(norm(&t.artist()));
        }
        if arts.iter().any(|a| per_artist.get(a).copied().unwrap_or(0) >= cap) {
            rest.push(t);
            continue;
        }
        for a in arts {
            *per_artist.entry(a).or_default() += 1;
        }
        out.push(t);
        if out.len() >= count {
            break;
        }
    }
    // if there weren't enough different artists, fill up from the deferred ones
    if out.len() < count / 2 {
        out.extend(rest.into_iter().take(count / 2 - out.len()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(artist: &str, title: &str) -> Track {
        serde_json::from_value(serde_json::json!({
            "id": 1, "title": title, "user": { "username": artist }
        }))
        .unwrap()
    }

    #[test]
    fn parses_feats() {
        let t = track(
            "Платина",
            "Платина & OG Buda - Групи (feat. Big Baby Tape) [prod. by Heavy Keyzz]",
        );
        let a = track_artists(&t);
        for n in ["платина", "og buda", "big baby tape", "heavy keyzz"] {
            assert!(a.contains(&n.to_string()), "{n} not in {a:?}");
        }
        let t = track("LOVV66", "LOVV66 x cardinparis - Trap");
        let a = track_artists(&t);
        assert!(a.contains(&"cardinparis".to_string()), "{a:?}");
        let t = track("Aarne", "Не в тусе ft. Платина");
        assert!(track_artists(&t).contains(&"платина".to_string()));
    }
}
