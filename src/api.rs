//! Client for the unofficial SoundCloud API (api-v2.soundcloud.com).
//!
//! client_id is taken from the site's JS bundle (yt-dlp and other clients do the same),
//! cached on disk and refreshed automatically when the API answers 401/403.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

const API: &str = "https://api-v2.soundcloud.com";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/131.0 Safari/537.36";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub username: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Format {
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub mime_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcoding {
    pub url: String,
    #[serde(default)]
    pub snipped: bool,
    pub format: Format,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Media {
    #[serde(default)]
    pub transcodings: Vec<Transcoding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    pub title: Option<String>,
    pub user: Option<User>,
    #[serde(default)]
    pub duration: u64,
    pub full_duration: Option<u64>,
    pub artwork_url: Option<String>,
    /// track waveform (JSON with amplitudes), drawn instead of a plain progress bar
    pub waveform_url: Option<String>,
    pub permalink_url: Option<String>,
    pub policy: Option<String>,
    #[serde(default)]
    pub media: Media,
    pub track_authorization: Option<String>,
    pub playback_count: Option<u64>,
    pub likes_count: Option<u64>,
    pub reposts_count: Option<u64>,
    pub comment_count: Option<u64>,
    pub genre: Option<String>,
}

impl Track {
    pub fn title(&self) -> String {
        fix_text(self.title.as_deref().unwrap_or(crate::i18n::t("Без названия")))
    }

    pub fn artist(&self) -> String {
        fix_text(self.user.as_ref().map(|u| u.username.as_str()).unwrap_or(""))
    }

    /// Go+ track: without a subscription only a 30-second preview is available.
    pub fn is_preview(&self) -> bool {
        self.policy.as_deref() == Some("SNIP")
            || (!self.media.transcodings.is_empty() && self.media.transcodings.iter().all(|t| t.snipped))
    }

    /// Whether the track has a DRM-free stream that can be played.
    pub fn is_playable(&self) -> bool {
        self.media
            .transcodings
            .iter()
            .any(|t| t.format.protocol == "progressive" || t.format.protocol == "hls")
    }

    /// Artwork of the requested size ("t67x67", "t300x300", "t500x500"), otherwise the uploader's avatar.
    pub fn artwork(&self, size: &str) -> Option<String> {
        self.artwork_url
            .clone()
            .or_else(|| self.user.as_ref().and_then(|u| u.avatar_url.clone()))
            .map(|u| u.replace("-large.", &format!("-{size}.")))
    }
}

/// Merge "и + breve" → "й", "е + diaeresis" → "ё": egui's font does not render combining marks.
fn fix_text(s: &str) -> String {
    s.replace("\u{438}\u{306}", "й")
        .replace("\u{418}\u{306}", "Й")
        .replace("\u{435}\u{308}", "ё")
        .replace("\u{415}\u{308}", "Ё")
        .replace('\u{306}', "")
        .replace('\u{308}', "")
}

pub struct SearchPage {
    pub tracks: Vec<Track>,
    pub next_offset: Option<u32>,
}

pub struct Client {
    http: reqwest::blocking::Client,
    client_id: Mutex<Option<String>>,
    /// SoundCloud account token (after sign-in)
    oauth: Mutex<Option<String>>,
}

/// Profile of the signed-in user.
#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    pub id: u64,
    #[serde(default)]
    pub username: String,
    pub avatar_url: Option<String>,
}

fn cache_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("scloud").join("client_id.txt"))
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

impl Client {
    pub fn new() -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent(UA)
            .timeout(Duration::from_secs(20))
            .build()
            .expect(crate::i18n::t("не удалось создать HTTP-клиент"));
        let cached = cache_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() == 32);
        Self {
            http,
            client_id: Mutex::new(cached),
            oauth: Mutex::new(None),
        }
    }

    fn client_id(&self) -> Result<String, String> {
        let cached = self.client_id.lock().unwrap().clone();
        match cached {
            Some(id) => Ok(id),
            None => self.refresh_client_id(),
        }
    }

    /// Extract client_id from the JS scripts of the soundcloud.com home page.
    fn refresh_client_id(&self) -> Result<String, String> {
        let html = self
            .http
            .get("https://soundcloud.com/")
            .send()
            .and_then(|r| r.text())
            .map_err(err)?;

        let script_re =
            Regex::new(r#"<script[^>]+src="(https://a-v2\.sndcdn\.com/assets/[^"]+\.js)""#).unwrap();
        let id_re = Regex::new(r#"client_id\s*[:=]\s*"?([a-zA-Z0-9]{32})"#).unwrap();

        let scripts: Vec<String> = script_re.captures_iter(&html).map(|c| c[1].to_string()).collect();

        // client_id usually sits in one of the last bundles
        for url in scripts.iter().rev() {
            let Ok(js) = self.http.get(url).send().and_then(|r| r.text()) else {
                continue;
            };
            if let Some(c) = id_re.captures(&js) {
                let id = c[1].to_string();
                if let Some(p) = cache_path() {
                    let _ = std::fs::create_dir_all(p.parent().unwrap());
                    let _ = std::fs::write(p, &id);
                }
                *self.client_id.lock().unwrap() = Some(id.clone());
                return Ok(id);
            }
        }
        Err(crate::i18n::t("Не удалось получить client_id с soundcloud.com").into())
    }

    pub fn set_token(&self, token: Option<String>) {
        *self.oauth.lock().unwrap() = token;
    }

    fn get_json(&self, url: &str, query: &[(&str, String)]) -> Result<serde_json::Value, String> {
        self.request(reqwest::Method::GET, url, query, None)
    }

    /// Any api-v2 request: client_id in the query, the account token (if signed in) in a header.
    fn request(
        &self,
        method: reqwest::Method,
        url: &str,
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        for attempt in 0..2 {
            let id = self.client_id()?;
            let mut req = self
                .http
                .request(method.clone(), url)
                .query(query)
                .query(&[("client_id", id.as_str())]);
            let token = self.oauth.lock().unwrap().clone();
            if let Some(tok) = token {
                req = req.header("Authorization", format!("OAuth {tok}"));
            }
            let write = method != reqwest::Method::GET;
            if write {
                // SoundCloud only accepts write requests that look like they come from the site
                req = req
                    .header("Origin", "https://soundcloud.com")
                    .header("Referer", "https://soundcloud.com/")
                    .header("Accept", "application/json, text/javascript, */*; q=0.01");
            }
            match &body {
                Some(b) => req = req.json(b),
                None if write => req = req.header("Content-Type", "application/json").body(Vec::new()),
                None => {}
            }
            let resp = req.send().map_err(err)?;
            let status = resp.status();
            // for reads, 401/403 usually means "client_id expired": fetch a new one and retry
            if (status.as_u16() == 401 || status.as_u16() == 403) && attempt == 0 && !write {
                // client_id may have expired: fetch a new one and retry
                *self.client_id.lock().unwrap() = None;
                continue;
            }
            if status.as_u16() == 401 {
                return Err(crate::i18n::t("401: вход в SoundCloud устарел — войди заново").into());
            }
            if !status.is_success() {
                let body = resp.text().unwrap_or_default();
                let hint = if body.contains("datadome") || body.contains("captcha") {
                    crate::i18n::t(" (защита от ботов SoundCloud)")
                } else {
                    ""
                };
                return Err(crate::i18n::tf(
                    "SoundCloud ответил {status}{hint}",
                    &[("{status}", format!("{status}")), ("{hint}", format!("{hint}"))],
                ));
            }
            let text = resp.text().map_err(err)?;
            if text.trim().is_empty() {
                return Ok(serde_json::Value::Null);
            }
            return serde_json::from_str(&text).map_err(err);
        }
        Err(crate::i18n::t("Ошибка авторизации в API SoundCloud").into())
    }

    // ---------- account ----------

    pub fn me(&self) -> Result<Me, String> {
        let v = self.get_json(&format!("{API}/me"), &[])?;
        serde_json::from_value(v).map_err(err)
    }

    /// User playlists: (id, title, tracks). For long playlists SoundCloud only returns
    /// the first tracks in full; the rest are fetched by id in batches.
    pub fn my_playlists(&self, user_id: u64) -> Result<Vec<(u64, String, Vec<Track>)>, String> {
        let v = self.get_json(
            &format!("{API}/users/{user_id}/playlists"),
            &[("limit", "50".to_string()), ("linked_partitioning", "1".to_string())],
        )?;
        let mut out = Vec::new();
        for p in v["collection"].as_array().cloned().unwrap_or_default() {
            let Some(id) = p["id"].as_u64() else { continue };
            let title = p["title"].as_str().unwrap_or("Playlist").to_string();
            let items = p["tracks"].as_array().cloned().unwrap_or_default();
            let ids: Vec<u64> = items.iter().filter_map(|t| t["id"].as_u64()).collect();
            let mut full: std::collections::HashMap<u64, Track> = items
                .iter()
                .filter(|t| t["title"].is_string())
                .filter_map(|t| serde_json::from_value::<Track>(t.clone()).ok())
                .map(|t| (t.id, t))
                .collect();
            let missing: Vec<u64> = ids.iter().copied().filter(|i| !full.contains_key(i)).collect();
            for chunk in missing.chunks(50) {
                let list = chunk.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
                if let Ok(v) = self.get_json(&format!("{API}/tracks"), &[("ids", list)]) {
                    for t in parse_tracks(&v) {
                        full.insert(t.id, t);
                    }
                }
            }
            let tracks = ids.iter().filter_map(|i| full.remove(i)).collect();
            out.push((id, title, tracks));
        }
        Ok(out)
    }

    /// All liked tracks (paged, up to `max`).
    pub fn my_likes(&self, user_id: u64, max: usize) -> Result<Vec<Track>, String> {
        let mut out = Vec::new();
        let mut url = format!("{API}/users/{user_id}/track_likes");
        let mut query = vec![
            ("limit", "200".to_string()),
            ("linked_partitioning", "1".to_string()),
        ];
        loop {
            let v = self.get_json(&url, &query)?;
            if let Some(a) = v["collection"].as_array() {
                out.extend(
                    a.iter()
                        .filter_map(|x| serde_json::from_value::<Track>(x["track"].clone()).ok()),
                );
            }
            match v["next_href"].as_str() {
                Some(next) if out.len() < max => {
                    url = next.to_string();
                    query.clear();
                }
                _ => break,
            }
        }
        out.truncate(max);
        Ok(out)
    }

    /// Like/unlike. Goes through the hidden browser (see login::browser_fetch):
    /// direct requests get blocked by SoundCloud's bot protection.
    pub fn set_like(&self, user_id: u64, track_id: u64, liked: bool) -> Result<(), String> {
        let method = if liked { "PUT" } else { "DELETE" };
        self.browser_write(
            method,
            &format!("{API}/users/{user_id}/track_likes/{track_id}"),
            None,
        )
    }

    fn browser_write(&self, method: &str, url: &str, body: Option<serde_json::Value>) -> Result<(), String> {
        let cid = self.client_id()?;
        let token = self
            .oauth
            .lock()
            .unwrap()
            .clone()
            .ok_or(crate::i18n::t("не выполнен вход в SoundCloud"))?;
        let sep = if url.contains('?') { '&' } else { '?' };
        let full = format!("{url}{sep}client_id={cid}");
        match crate::login::browser_fetch(method, &full, &token, body)? {
            200..=299 => Ok(()),
            401 => Err(crate::i18n::t("401: вход устарел — войди заново").into()),
            s => Err(crate::i18n::tf(
                "SoundCloud ответил {s}",
                &[("{s}", format!("{s}"))],
            )),
        }
    }

    /// The account's listening history (the same as on the site).
    pub fn play_history(&self) -> Result<Vec<Track>, String> {
        let v = self.get_json(
            &format!("{API}/me/play-history/tracks"),
            &[("limit", "100".into()), ("linked_partitioning", "1".into())],
        )?;
        Ok(v["collection"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| serde_json::from_value::<Track>(x["track"].clone()).ok())
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The whole history SoundCloud returns: (start time in ms, track), newest first.
    /// `progress` is called after every page with the number of records fetched so far.
    pub fn play_history_full(
        &self,
        max_pages: usize,
        progress: &dyn Fn(usize),
    ) -> Result<Vec<(i64, Track)>, String> {
        let mut out = Vec::new();
        let mut url = format!("{API}/me/play-history/tracks");
        let mut query = vec![
            ("limit", "200".to_string()),
            ("linked_partitioning", "1".to_string()),
        ];
        for _ in 0..max_pages {
            let v = self.get_json(&url, &query)?;
            let items = v["collection"].as_array().cloned().unwrap_or_default();
            if items.is_empty() {
                break;
            }
            for x in &items {
                let at = x["played_at"].as_i64().unwrap_or(0);
                if let Ok(t) = serde_json::from_value::<Track>(x["track"].clone()) {
                    out.push((at, t));
                }
            }
            progress(out.len());
            match v["next_href"].as_str() {
                Some(next) => {
                    url = next.to_string();
                    query.clear();
                }
                None => break,
            }
        }
        Ok(out)
    }

    /// Record a play in the SoundCloud history (also through the browser).
    pub fn record_play(&self, track_id: u64) -> Result<(), String> {
        let body = serde_json::json!({ "track_urn": format!("soundcloud:tracks:{track_id}") });
        self.browser_write("POST", &format!("{API}/me/play-history"), Some(body))
    }

    /// Track search: the same endpoint the site uses.
    pub fn search_tracks(&self, q: &str, offset: u32) -> Result<SearchPage, String> {
        let v = self.get_json(
            &format!("{API}/search/tracks"),
            &[
                ("q", q.to_string()),
                ("limit", "30".into()),
                ("offset", offset.to_string()),
                ("linked_partitioning", "1".into()),
            ],
        )?;
        let raw = v["collection"].as_array().cloned().unwrap_or_default();
        let count = raw.len() as u32;
        let tracks = raw
            .into_iter()
            .filter_map(|t| serde_json::from_value::<Track>(t).ok())
            .collect();
        let has_next = v["next_href"].as_str().is_some();
        Ok(SearchPage {
            tracks,
            next_offset: (has_next && count > 0).then_some(offset + count),
        })
    }

    /// Search suggestions (like the dropdown on the site).
    pub fn suggestions(&self, q: &str) -> Result<Vec<String>, String> {
        let v = self.get_json(
            &format!("{API}/search/queries"),
            &[("q", q.to_string()), ("limit", "8".into())],
        )?;
        Ok(v["collection"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x["output"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Direct audio stream URL. Only DRM-free formats are used:
    /// progressive MP3 → HLS MP3 → any other unencrypted HLS.
    pub fn stream_url(&self, t: &Track) -> Result<String, String> {
        // Since November 2025 SoundCloud disabled MP3 (progressive / HLS MP3) and moved to HLS AAC.
        // Old formats can still show up in the list but return 404, so try them one by one.
        let rank = |x: &&Transcoding| -> u8 {
            let (p, m) = (x.format.protocol.as_str(), x.format.mime_type.as_str());
            match (p, m) {
                ("hls", m) if m.contains("mp4") => 0,  // AAC 160k
                ("hls", m) if m.contains("mpeg") => 1, // old MP3 HLS
                ("progressive", _) => 2,               // old MP3
                ("hls", _) => 3,                       // opus and the rest
                _ => 9,                                // encrypted (DRM): skip
            }
        };
        let mut candidates: Vec<&Transcoding> = t.media.transcodings.iter().filter(|x| rank(x) < 9).collect();
        candidates.sort_by_key(|x| rank(x));
        if candidates.is_empty() {
            return Err(crate::i18n::t("У трека нет потока без DRM (или он недоступен в регионе)").into());
        }

        let mut q = Vec::new();
        if let Some(a) = &t.track_authorization {
            q.push(("track_authorization", a.clone()));
        }
        let mut last_err = String::new();
        for tc in candidates {
            match self.get_json(&tc.url, &q) {
                Ok(v) => {
                    if let Some(u) = v["url"].as_str() {
                        return Ok(u.to_string());
                    }
                    last_err = crate::i18n::t("SoundCloud не вернул ссылку на поток").into();
                }
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    /// Trending by genre. If the charts endpoint fails, fall back to a plain search for the genre name.
    pub fn charts(&self, genre: &str, fallback: &str) -> Result<Vec<Track>, String> {
        if let Ok(v) = self.get_json(
            &format!("{API}/charts"),
            &[
                ("kind", "trending".into()),
                ("genre", format!("soundcloud:genres:{genre}")),
                ("limit", "30".into()),
            ],
        ) {
            let tracks: Vec<Track> = v["collection"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| serde_json::from_value(x["track"].clone()).ok())
                        .collect()
                })
                .unwrap_or_default();
            if !tracks.is_empty() {
                return Ok(tracks);
            }
        }
        Ok(self.search_tracks(fallback, 0)?.tracks)
    }

    /// Waveform amplitudes 0..1 (SoundCloud returns ~1800 points).
    pub fn waveform(&self, url: &str) -> Result<Vec<f32>, String> {
        let url = if url.ends_with(".png") {
            url.replace("w1.sndcdn.com", "wave.sndcdn.com")
                .replace(".png", ".json")
        } else {
            url.to_string()
        };
        let v: serde_json::Value = self.http.get(&url).send().map_err(err)?.json().map_err(err)?;
        let h = v["height"].as_f64().unwrap_or(140.0).max(1.0);
        Ok(v["samples"]
            .as_array()
            .map(|a| a.iter().map(|x| (x.as_f64().unwrap_or(0.0) / h) as f32).collect())
            .unwrap_or_default())
    }

    /// Tracks by an uploader (for the wave: other tracks by artists you listen to).
    pub fn user_tracks(&self, user_id: u64, limit: u32) -> Result<Vec<Track>, String> {
        let v = self.get_json(
            &format!("{API}/users/{user_id}/tracks"),
            &[("limit", limit.to_string()), ("linked_partitioning", "1".into())],
        )?;
        Ok(parse_tracks(&v["collection"]))
    }

    /// Related tracks (like "Related tracks" on the site).
    pub fn related(&self, id: u64) -> Result<Vec<Track>, String> {
        let v = self.get_json(&format!("{API}/tracks/{id}/related"), &[("limit", "20".into())])?;
        Ok(parse_tracks(&v["collection"]))
    }

    pub fn track(&self, id: u64) -> Result<Track, String> {
        let v = self.get_json(&format!("{API}/tracks/{id}"), &[])?;
        serde_json::from_value(v).map_err(err)
    }

    /// Like stream_url, but if the stored track data is stale (likes, history),
    /// re-fetch the track and try again.
    pub fn stream_url_fresh(&self, t: &Track) -> Result<String, String> {
        match self.stream_url(t) {
            Ok(u) => Ok(u),
            Err(e) => match self.track(t.id) {
                Ok(fresh) => self.stream_url(&fresh),
                Err(_) => Err(e),
            },
        }
    }

    pub fn fetch_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        let r = self.http.get(url).send().map_err(err)?;
        if !r.status().is_success() {
            return Err(format!("HTTP {}", r.status()));
        }
        r.bytes().map(|b| b.to_vec()).map_err(err)
    }
}

fn parse_tracks(v: &serde_json::Value) -> Vec<Track> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| serde_json::from_value(t.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}
