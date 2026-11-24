//! Statistics from the SoundCloud listening history (no server of our own).
//!
//! The history only has "when the track was started". The actual listening time is estimated as
//! min(track length, time until the next play). Skipped after 40 s → 40 s are counted.

use crate::api::Track;
use crate::wave::track_artist_names;
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone};
use std::collections::HashMap;

pub struct Stats {
    /// the earliest date SoundCloud returned
    pub oldest: Option<NaiveDate>,
    pub plays: usize,
    pub unique_tracks: usize,
    pub today: f64,
    pub week: f64,
    pub month: f64,
    pub year: f64,
    #[allow(dead_code)]
    pub total: f64,
    pub year_num: i32,
    /// the 12 months of the current year
    pub by_month: Vec<(&'static str, f64)>,
    /// the last 30 days
    pub last30: Vec<(NaiveDate, f64)>,
    pub streak: u32,
    pub top_artists: Vec<(String, f64)>,
    pub top_tracks: Vec<(Track, f64, u32)>,
}

const MONTHS: [&str; 12] = [
    "янв", "фев", "мар", "апр", "май", "июн", "июл", "авг", "сен", "окт", "ноя", "дек",
];

fn local(ms: i64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ms).single()
}

pub fn compute(mut plays: Vec<(i64, Track)>) -> Stats {
    plays.retain(|p| p.0 > 0);
    plays.sort_by_key(|p| p.0); // oldest to newest
    let now = Local::now();
    let now_ms = now.timestamp_millis();
    let today = now.date_naive();
    let week_start = today - Duration::days(6);
    let month_start = today.with_day(1).unwrap_or(today);
    let year = today.year();

    let mut by_day: HashMap<NaiveDate, f64> = HashMap::new();
    let mut by_month = [0.0f64; 12];
    let mut artists: HashMap<String, (String, f64)> = HashMap::new();
    let mut tracks: HashMap<u64, (Track, f64, u32)> = HashMap::new();
    let (mut s_today, mut s_week, mut s_month, mut s_year, mut s_total) = (0.0, 0.0, 0.0, 0.0, 0.0);

    for (i, (at, t)) in plays.iter().enumerate() {
        let len = t.full_duration.unwrap_or(t.duration) as f64 / 1000.0;
        let next = plays.get(i + 1).map(|p| p.0).unwrap_or(now_ms);
        let gap = ((next - at) as f64 / 1000.0).max(0.0);
        let secs = if len > 0.0 { len.min(gap) } else { gap.min(240.0) };
        if secs < 1.0 {
            continue;
        }
        let Some(dt) = local(*at) else { continue };
        let day = dt.date_naive();

        s_total += secs;
        *by_day.entry(day).or_default() += secs;
        if day.year() == year {
            s_year += secs;
            by_month[day.month0() as usize] += secs;
        }
        if day >= month_start {
            s_month += secs;
        }
        if day >= week_start {
            s_week += secs;
        }
        if day == today {
            s_today += secs;
        }

        // every participant (uploader + features) gets the full track time
        for name in track_artist_names(t) {
            let e = artists.entry(name.to_lowercase()).or_insert((name.clone(), 0.0));
            e.1 += secs;
        }
        let e = tracks.entry(t.id).or_insert((t.clone(), 0.0, 0));
        e.1 += secs;
        e.2 += 1;
    }

    // streak: consecutive days (up to today or yesterday) with at least a minute of music
    let active = |d: NaiveDate| by_day.get(&d).copied().unwrap_or(0.0) >= 60.0;
    let mut d = if active(today) {
        today
    } else {
        today - Duration::days(1)
    };
    let mut streak = 0;
    while active(d) {
        streak += 1;
        d -= Duration::days(1);
    }

    let last30 = (0..30)
        .rev()
        .map(|k| {
            let day = today - Duration::days(k);
            (day, by_day.get(&day).copied().unwrap_or(0.0))
        })
        .collect();

    let mut top_artists: Vec<(String, f64)> = artists.into_values().collect();
    top_artists.sort_by(|a, b| b.1.total_cmp(&a.1));
    top_artists.truncate(10);
    let unique_tracks = tracks.len();
    let mut top_tracks: Vec<(Track, f64, u32)> = tracks.into_values().collect();
    top_tracks.sort_by(|a, b| b.1.total_cmp(&a.1));
    top_tracks.truncate(10);

    Stats {
        oldest: plays.first().and_then(|p| local(p.0)).map(|d| d.date_naive()),
        plays: plays.len(),
        unique_tracks,
        today: s_today,
        week: s_week,
        month: s_month,
        year: s_year,
        total: s_total,
        year_num: year,
        by_month: MONTHS.iter().copied().zip(by_month).collect(),
        last30,
        streak,
        top_artists,
        top_tracks,
    }
}

pub fn fmt_hours(secs: f64) -> String {
    let m = (secs / 60.0).round() as u64;
    if m < 60 {
        crate::i18n::tf("{m} мин", &[("{m}", format!("{m}"))])
    } else {
        crate::i18n::tf(
            "{} ч {} мин",
            &[("{}", format!("{}", m / 60)), ("{}", format!("{}", m % 60))],
        )
    }
}
