use crate::api::{Client, Me, SearchPage, Track};
use crate::discord::{Discord, Presence};
use crate::downloader::{self, DlEvent, DlState};
use crate::fx::Fx;
use crate::library::Library;
use crate::login;
use crate::player::Player;
use crate::stats::{self, fmt_hours, Stats};
use crate::theme::{self, ThemeSettings};
use crate::wave::{self, WaveKind, WaveMode};
use eframe::egui::{
    self, Align, Color32, Layout, Margin, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, Vec2,
};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------- colors come from the current theme (see theme.rs) ----------
fn c_accent() -> Color32 {
    theme::pal().accent
}
fn c_text() -> Color32 {
    theme::pal().text
}
fn c_muted() -> Color32 {
    theme::pal().muted
}
fn c_surface() -> Color32 {
    let p = theme::pal();
    p.surface.gamma_multiply(p.opacity.max(0.6))
}
fn c_hover() -> Color32 {
    theme::pal().hover
}
fn c_wave_col() -> Color32 {
    theme::pal().wave
}
fn c_bar() -> Color32 {
    let p = theme::pal();
    p.bar.gamma_multiply(p.opacity)
}
/// dark color for icons on light circles/pills
const INK: Color32 = Color32::from_rgb(18, 18, 18);

/// Unbounded: headings, logo, big numbers.
fn fam_display() -> egui::FontFamily {
    egui::FontFamily::Name("display".into())
}

/// Onest SemiBold: track titles, buttons, emphasis in text.
fn fam_semi() -> egui::FontFamily {
    egui::FontFamily::Name("semibold".into())
}

const DEFAULT_DISCORD_APP_ID: &str = "1574767645213991063";
/// (SoundCloud genre key, label, fallback search query)
const GENRES: &[(&str, &str, &str)] = &[
    ("hiphoprap", "Хип-хоп", "hip hop"),
    ("all-music", "Все жанры", "top"),
    ("trap", "Трэп", "trap"),
    ("electronic", "Электроника", "electronic"),
    ("alternativerock", "Альт-рок", "alternative rock"),
    ("pop", "Поп", "pop"),
    ("rbsoul", "R&B", "r&b"),
    ("indie", "Инди", "indie"),
    ("house", "Хаус", "house"),
    ("drumbass", "Драм-н-бейс", "drum and bass"),
];

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Home,
    Search,
    Library,
    Stats,
}

#[derive(Clone, Copy, PartialEq)]
enum LibTab {
    Likes,
    History,
    Playlists,
}

/// Sleep timer: pause at a point in time or at the end of the current track.
#[derive(Clone, Copy, PartialEq)]
enum SleepTimer {
    At(Instant),
    EndOfTrack,
}

/// What was picked in a track's right-click menu.
#[derive(Clone, Copy, PartialEq)]
enum MenuAct {
    PlayNext,
    Enqueue,
    Radio,
    Like,
    Download,
    Copy,
    Open,
    AddTo(u64),
    NewPlaylist,
    RemoveFromPlaylist(u64),
}

#[derive(Clone, Copy, PartialEq)]
enum Repeat {
    Off,
    All,
    One,
}

#[derive(Clone, Copy)]
enum Source {
    Results,
    Genre,
    Related,
    Likes,
    History,
    Playlist(u64),
}

enum Msg {
    Playlists(Result<Vec<(u64, String, Vec<Track>)>, String>),
    Search {
        generation: u64,
        append: bool,
        result: Result<SearchPage, String>,
    },
    Suggest {
        query: String,
        result: Vec<String>,
    },
    Stream {
        track_id: u64,
        result: Result<String, String>,
    },
    Genre {
        key: String,
        result: Result<Vec<Track>, String>,
    },
    Related {
        id: u64,
        result: Vec<Track>,
    },
    Download(DlEvent),
    Status(String),
    Lyrics(u64, Option<crate::lyrics::Lyrics>),
    AlignProgress(u64, f32),
    WhisperProgress(f32),
    WhisperInstalled(Result<(), String>),
    Aligned(u64, Result<Vec<(f64, String)>, String>),
    Waveform(u64, Vec<f32>),
    StatsProgress(usize),
    StatsDone(Result<Stats, String>),
    Wave {
        kind: WaveKind,
        tracks: Vec<Track>,
        append: bool,
    },
    ScToken(Result<String, String>),
    ScMe(Result<Me, String>),
    ScSync {
        likes: Result<Vec<Track>, String>,
        history: Result<Vec<Track>, String>,
    },
}

/// Own-lyrics editor: plain input or "sync while the track plays".
struct LyricsEdit {
    track_id: u64,
    text: String,
    /// sync mode: lines, the times marked so far, index of the next line
    sync: Option<(Vec<String>, Vec<f64>)>,
}

enum Action {
    Search,
    LoadMore,
    Play(Source, usize),
    Prev,
    Next,
    TogglePause,
    Seek(f64),
    Volume(f32),
    Like(Track),
    Genre(&'static str),
    Go(Page),
    Shuffle,
    Repeat,
    Download(Track),
    Reveal(u64),
    CopyLink(Track),
    ScLogin,
    ScLogout,
    ScSync,
    #[allow(dead_code)]
    StartWave(WaveKind),
    PlayNext(Track),
    Enqueue(Track),
    Radio(Track),
    PlayQueue(usize),
    RemoveFromQueue(usize),
    Sleep(Option<SleepTimer>),
    ToggleMute,
    SeekBy(f64),
    VolumeBy(f32),
    LoadStats,
    Dislike,
    /// relax mode on/off
    Relax(bool),
    /// full screen over the taskbar
    Fullscreen(bool),
    /// pick a wave mode and start it right away
    WaveMode(WaveMode),
    /// reshuffle the wave
    WaveShake,
    /// the big button in "My Wave": start / pause
    WaveToggle,
    AddToPlaylist(u64, Track),
    NewPlaylistWith(Track),
    RemoveFromPlaylist(u64, u64),
    ImportPlaylists,
}

pub struct App {
    api: Arc<Client>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    booted: bool,
    page: Page,
    lib_tab: LibTab,
    /// the playlist open in the Library
    open_playlist: Option<u64>,
    /// renaming: (id, new name)
    playlist_rename: Option<(u64, String)>,
    playlist_confirm_delete: Option<u64>,
    playlists_importing: bool,

    // search
    query: String,
    last_query: String,
    results: Vec<Track>,
    next_offset: Option<u32>,
    searching: bool,
    generation: u64,
    suggestions: Vec<String>,
    show_suggestions: bool,
    query_edited_at: Option<Instant>,
    search_rect: Option<Rect>,

    // home
    genre: &'static str,
    genre_tracks: Vec<Track>,
    genre_loading: bool,
    related: Vec<Track>,
    related_for: Option<u64>,

    // player
    player: Option<Player>,
    queue: Vec<Track>,
    current: Option<usize>,
    loading_stream: bool,
    loaded_at: Option<Instant>,
    seek_drag: Option<f64>,
    volume: f32,
    shuffle: bool,
    repeat: Repeat,
    fail_streak: u32,

    // effects
    fx: Fx,
    applied_fx: Fx,
    show_fx: bool,

    library: Library,
    status: Option<String>,
    /// pop-up notifications (download, link copied, etc.)
    notes: Vec<(String, Instant)>,

    downloads: HashMap<u64, DlState>,
    /// empty = default folder (Music\SCloud)
    download_dir: String,

    // SoundCloud account
    sc_me: Option<Me>,
    sc_busy: bool,

    // statistics
    stats: Option<Stats>,
    stats_loading: bool,
    stats_progress: usize,

    /// plays not yet sent to the SoundCloud history (sent in batches)
    pending_plays: Vec<u64>,
    /// theme
    theme: ThemeSettings,
    /// for smooth tab transitions
    shown_page: Page,
    page_changed: Instant,
    /// the selected gradient key in the theme editor
    theme_sel: usize,
    /// language: "auto" | "ru" | "uk" | "en"
    lang_code: String,
    /// "My themes": (name, code)
    saved_themes: Vec<(String, String)>,
    theme_name_input: String,
    theme_code_input: String,
    /// window icon animation: current frame (None = static) and when it last changed
    icon_frame: Option<usize>,
    /// can be turned off if it bothers apps like TranslucentTB
    animate_taskbar: bool,
    icon_changed: Instant,
    /// waveform of the playing track
    waveform: Option<(u64, Vec<f32>)>,

    // wave
    wave: Option<WaveKind>,
    /// the track for "Track radio"
    radio_seed: Option<Track>,
    sleep: Option<SleepTimer>,
    /// lyrics screen
    show_lyrics: bool,
    /// (track id, None = searching, Some(None) = not found, Some(Some) = lyrics)
    lyrics: Option<(u64, Option<Option<crate::lyrics::Lyrics>>)>,
    /// lyrics timing offset (s) when lines are early/late
    lyrics_offset: f64,
    /// own-lyrics editor
    lyrics_edit: Option<LyricsEdit>,
    /// the AI is aligning lyrics: (track id, progress 0..1)
    aligning: Option<(u64, f32)>,
    align_after_click: bool,
    /// whisper is being installed: progress 0..1
    whisper_install: Option<f32>,
    install_after_click: bool,
    /// tracks already tried (to avoid looping)
    align_tried: std::collections::HashSet<u64>,
    show_queue: bool,
    show_help: bool,
    /// volume before mute
    muted_from: Option<f32>,
    /// "Now playing" card: the track and when it was shown
    toast: Option<(Track, Instant)>,
    /// Windows system media controls (⏯ ⏭ ⏮ keys and the overlay with artwork)
    media: crate::media::Media,
    wave_loading: bool,
    /// "My Wave" mode
    wave_mode: WaveMode,
    /// relax mode: full screen with just the artwork, lyrics and player
    relax: bool,
    /// last mouse movement in relax mode (controls hide while the mouse is idle)
    relax_mouse: Instant,
    /// whether we were full screen before relax mode
    relax_prev_full: bool,
    started: Instant,
    /// full-screen mode (F11), over the taskbar
    fullscreen: bool,

    // Discord
    discord: Discord,
    discord_enabled: bool,
    discord_app_id: String,
    last_presence: Option<(Presence, Instant)>,
    show_settings: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);

        let (tx, rx) = channel();
        let (player, status) = match Player::new() {
            Ok(p) => (Some(p), None),
            Err(e) => (
                None,
                Some(crate::i18n::tf(
                    "Плеер не запустился ({e}). Проверь, что libmpv установлен.",
                    &[("{e}", format!("{e}"))],
                )),
            ),
        };

        let settings = load_settings();
        let discord_enabled = settings["discord_enabled"].as_bool().unwrap_or(true);
        let discord_app_id = settings["discord_app_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_DISCORD_APP_ID)
            .to_string();
        let volume = settings["volume"].as_f64().unwrap_or(80.0) as f32;
        let fx: Fx = serde_json::from_value(settings["fx"].clone()).unwrap_or_default();
        let lang_code = settings["lang"].as_str().unwrap_or("auto").to_string();
        crate::i18n::apply_setting(&lang_code);
        let theme_settings: ThemeSettings =
            serde_json::from_value(settings["theme"].clone()).unwrap_or_default();
        theme::set(theme_settings.palette());
        apply_visuals(&cc.egui_ctx);

        // open full screen over the taskbar (if that's how it was last time)
        if settings["fullscreen"].as_bool().unwrap_or(true) {
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
        Self {
            api: Arc::new(Client::new()),
            tx,
            rx,
            booted: false,
            page: Page::Home,
            lib_tab: LibTab::Likes,
            open_playlist: None,
            playlist_rename: None,
            playlist_confirm_delete: None,
            playlists_importing: false,
            query: String::new(),
            last_query: String::new(),
            results: Vec::new(),
            next_offset: None,
            searching: false,
            generation: 0,
            suggestions: Vec::new(),
            show_suggestions: false,
            query_edited_at: None,
            search_rect: None,
            genre: GENRES[0].0,
            genre_tracks: Vec::new(),
            genre_loading: false,
            related: Vec::new(),
            related_for: None,
            player,
            queue: Vec::new(),
            current: None,
            loading_stream: false,
            loaded_at: None,
            seek_drag: None,
            volume,
            shuffle: false,
            repeat: Repeat::Off,
            fail_streak: 0,
            fx,
            applied_fx: Fx::ORIGINAL,
            show_fx: false,
            library: Library::load(),
            status,
            downloads: HashMap::new(),
            download_dir: settings["download_dir"].as_str().unwrap_or("").to_string(),
            sc_me: None,
            sc_busy: false,
            wave: None,
            radio_seed: None,
            sleep: None,
            show_lyrics: false,
            lyrics: None,
            lyrics_offset: 0.0,
            notes: Vec::new(),
            lyrics_edit: None,
            aligning: None,
            align_after_click: false,
            whisper_install: None,
            install_after_click: false,
            align_tried: std::collections::HashSet::new(),
            show_queue: false,
            show_help: false,
            muted_from: None,
            toast: None,
            media: crate::media::Media::new(cc),
            wave_loading: false,
            wave_mode: WaveMode::Mix,
            relax: false,
            relax_mouse: Instant::now(),
            relax_prev_full: false,
            started: Instant::now(),
            fullscreen: settings["fullscreen"].as_bool().unwrap_or(true),
            waveform: None,
            pending_plays: Vec::new(),
            theme: theme_settings,
            shown_page: Page::Home,
            page_changed: Instant::now(),
            theme_sel: 0,
            lang_code,
            saved_themes: serde_json::from_value(settings["saved_themes"].clone()).unwrap_or_default(),
            theme_name_input: String::new(),
            theme_code_input: String::new(),
            icon_frame: None,
            animate_taskbar: settings["animate_taskbar"].as_bool().unwrap_or(true),
            icon_changed: Instant::now(),
            stats: None,
            stats_loading: false,
            stats_progress: 0,
            discord: Discord::start(if discord_enabled {
                discord_app_id.clone()
            } else {
                String::new()
            }),
            discord_enabled,
            discord_app_id,
            last_presence: None,
            show_settings: false,
        }
    }

    // ================= background requests =================

    fn start_search(&mut self, ctx: &egui::Context, append: bool) {
        let q = if append {
            self.last_query.clone()
        } else {
            self.query.trim().to_string()
        };
        if q.is_empty() {
            return;
        }
        let offset = if append {
            match self.next_offset {
                Some(o) => o,
                None => return,
            }
        } else {
            self.generation += 1;
            self.results.clear();
            self.next_offset = None;
            0
        };
        self.last_query = q.clone();
        if !append {
            self.library.push_search(&q);
        }
        self.searching = true;
        self.show_suggestions = false;
        self.page = Page::Search;

        let (api, tx, ctx, generation) = (self.api.clone(), self.tx.clone(), ctx.clone(), self.generation);
        std::thread::spawn(move || {
            let result = api.search_tracks(&q, offset);
            let _ = tx.send(Msg::Search {
                generation,
                append,
                result,
            });
            ctx.request_repaint();
        });
    }

    fn start_suggest(&mut self, ctx: &egui::Context) {
        let q = self.query.trim().to_string();
        if q.chars().count() < 2 {
            self.suggestions.clear();
            return;
        }
        let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = api.suggestions(&q).unwrap_or_default();
            let _ = tx.send(Msg::Suggest { query: q, result });
            ctx.request_repaint();
        });
    }

    fn start_genre(&mut self, ctx: &egui::Context, key: &'static str) {
        self.genre = key;
        self.genre_loading = true;
        let fallback = GENRES.iter().find(|g| g.0 == key).map(|g| g.2).unwrap_or("top");
        let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = api.charts(key, fallback);
            let _ = tx.send(Msg::Genre {
                key: key.to_string(),
                result,
            });
            ctx.request_repaint();
        });
    }

    fn start_related(&mut self, ctx: &egui::Context, id: u64) {
        if self.related_for == Some(id) {
            return;
        }
        self.related_for = Some(id);
        // "More of what you like": related tracks from several tracks by different artists,
        // with at most one track per artist in the feed.
        let mut seeds = vec![id];
        let mut seen_artists: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(t) = self.library.history.iter().chain(&self.library.likes).find(|t| t.id == id) {
            seen_artists.insert(t.artist().to_lowercase());
        }
        for t in self.library.history.iter().take(30).chain(self.library.likes.iter().take(30)) {
            if seeds.len() >= 5 {
                break;
            }
            if seen_artists.insert(t.artist().to_lowercase()) {
                seeds.push(t.id);
            }
        }
        let known: std::collections::HashSet<u64> = self
            .library
            .history
            .iter()
            .take(40)
            .map(|t| t.id)
            .chain(self.library.disliked.iter().copied())
            .collect();
        let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let lists: Vec<Vec<Track>> = std::thread::scope(|s| {
                let hs: Vec<_> = seeds
                    .iter()
                    .map(|&sid| {
                        let api = &api;
                        s.spawn(move || api.related(sid).unwrap_or_default())
                    })
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
            });
            // round-robin over the lists, one track per artist
            let mut result = Vec::new();
            let mut artists: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut ids = std::collections::HashSet::new();
            let longest = lists.iter().map(Vec::len).max().unwrap_or(0);
            for i in 0..longest {
                for l in &lists {
                    let Some(t) = l.get(i) else { continue };
                    if known.contains(&t.id) || !ids.insert(t.id) {
                        continue;
                    }
                    // none of the track's artists (including features) has appeared in the feed yet
                    let names = crate::wave::track_artists(t);
                    if names.iter().all(|n| !artists.contains(n)) {
                        artists.extend(names);
                        result.push(t.clone());
                    }
                }
            }
            result.truncate(30);
            let _ = tx.send(Msg::Related { id, result });
            ctx.request_repaint();
        });
    }

    fn play_queue_index(&mut self, ctx: &egui::Context, idx: usize) {
        let Some(track) = self.queue.get(idx).cloned() else {
            return;
        };
        self.current = Some(idx);
        self.loading_stream = true;
        self.loaded_at = None;
        self.seek_drag = None;
        if let Some(p) = &self.player {
            p.stop();
        }
        let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = api.stream_url_fresh(&track);
            let _ = tx.send(Msg::Stream {
                track_id: track.id,
                result,
            });
            ctx.request_repaint();
        });
    }

    fn current_track(&self) -> Option<&Track> {
        self.current.and_then(|i| self.queue.get(i))
    }

    fn next_index(&self) -> Option<usize> {
        let i = self.current?;
        let n = self.queue.len();
        if n == 0 {
            return None;
        }
        if self.wave.is_some() {
            // the wave plays in order and loads more by itself
            return (i + 1 < n).then_some(i + 1);
        }
        if self.shuffle && n > 1 {
            let mut j = rand_below(n);
            if j == i {
                j = (j + 1) % n;
            }
            return Some(j);
        }
        if i + 1 < n {
            Some(i + 1)
        } else if self.repeat == Repeat::All {
            Some(0)
        } else {
            None
        }
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Search {
                    generation,
                    append,
                    result,
                } => {
                    if generation != self.generation {
                        continue;
                    }
                    self.searching = false;
                    match result {
                        Ok(page) => {
                            if !append {
                                self.results.clear();
                            }
                            self.results.extend(page.tracks);
                            self.next_offset = page.next_offset;
                            self.status = None;
                        }
                        Err(e) => {
                            self.status =
                                Some(crate::i18n::tf("Ошибка поиска: {e}", &[("{e}", format!("{e}"))]))
                        }
                    }
                }
                Msg::Suggest { query, result } => {
                    if query == self.query.trim() {
                        self.suggestions = result;
                    }
                }
                Msg::Genre { key, result } => {
                    if key != self.genre {
                        continue;
                    }
                    self.genre_loading = false;
                    match result {
                        Ok(t) => self.genre_tracks = t,
                        Err(e) => {
                            self.status = Some(crate::i18n::tf(
                                "Не удалось загрузить тренды: {e}",
                                &[("{e}", format!("{e}"))],
                            ))
                        }
                    }
                }
                Msg::Status(text) => self.status = Some(text),
                Msg::Lyrics(id, l) => {
                    if self.lyrics.as_ref().map(|x| x.0) == Some(id) {
                        // line timing is approximate: let the AI align it (if installed)
                        let approx = l.as_ref().is_some_and(|l| l.approx);
                        self.lyrics = Some((id, Some(l)));
                        if approx {
                            self.start_align(ctx, id, false);
                        }
                    }
                }
                Msg::WhisperProgress(p) => {
                    if self.whisper_install.is_some() {
                        self.whisper_install = Some(p);
                    }
                }
                Msg::WhisperInstalled(r) => {
                    self.whisper_install = None;
                    match r {
                        Ok(()) => {
                            self.status = Some(crate::i18n::t("Нейросеть установлена — подгоняю текст").into());
                            if let Some(id) = self.current_track().map(|t| t.id) {
                                self.start_align(ctx, id, true);
                            }
                        }
                        Err(e) => {
                            self.status =
                                Some(crate::i18n::tf("Не получилось установить нейросеть: {e}", &[("{e}", e)]));
                        }
                    }
                }
                Msg::AlignProgress(id, p) => {
                    if self.aligning.is_some_and(|a| a.0 == id) {
                        self.aligning = Some((id, p));
                    }
                }
                Msg::Aligned(id, r) => {
                    if self.aligning.is_some_and(|a| a.0 == id) {
                        self.aligning = None;
                    }
                    match r {
                        Ok(lines) => {
                            crate::align::save_cache(id, &lines);
                            if self.lyrics.as_ref().map(|x| x.0) == Some(id) {
                                let plain = lines.iter().map(|l| l.1.as_str()).collect::<Vec<_>>().join("\n");
                                self.lyrics = Some((
                                    id,
                                    Some(Some(crate::lyrics::Lyrics {
                                        synced: lines,
                                        plain: Some(plain),
                                        source: "AI",
                                        approx: false,
                                    })),
                                ));
                                self.lyrics_offset = 0.0;
                                crate::lyrics::save_offset(id, 0.0);
                            }
                            self.status = Some(crate::i18n::t("Нейросеть подогнала текст под трек").into());
                        }
                        Err(e) => {
                            self.status = Some(crate::i18n::tf("Не получилось подогнать текст: {e}", &[("{e}", e)]));
                        }
                    }
                }
                Msg::Waveform(id, samples) => self.waveform = Some((id, samples)),
                Msg::StatsProgress(n) => self.stats_progress = n,
                Msg::StatsDone(r) => {
                    self.stats_loading = false;
                    match r {
                        Ok(st) => self.stats = Some(st),
                        Err(e) => {
                            self.status = Some(crate::i18n::tf("Статистика: {e}", &[("{e}", format!("{e}"))]))
                        }
                    }
                }
                Msg::Wave { kind, tracks, append } => {
                    if self.wave != Some(kind) {
                        continue;
                    }
                    self.wave_loading = false;
                    if tracks.is_empty() {
                        if !append {
                            self.wave = None;
                        }
                        self.status =
                            Some(crate::i18n::t("Не получилось собрать волну — попробуй ещё раз").into());
                        continue;
                    }
                    if append {
                        self.queue.extend(tracks);
                    } else {
                        self.status = None;
                        self.fail_streak = 0;
                        self.queue = tracks;
                        self.play_queue_index(ctx, 0);
                    }
                }
                Msg::ScToken(r) => match r {
                    Ok(tok) => {
                        save_sc_token(Some(&tok));
                        self.api.set_token(Some(tok));
                        self.fetch_me(ctx);
                    }
                    Err(e) => {
                        self.sc_busy = false;
                        self.status = Some(crate::i18n::tf("Вход: {e}", &[("{e}", format!("{e}"))]));
                    }
                },
                Msg::ScMe(r) => match r {
                    Ok(me) => {
                        self.status = Some(crate::i18n::tf(
                            "Вошёл как {} — синхронизирую лайки и историю…",
                            &[("{}", format!("{}", me.username))],
                        ));
                        self.sc_me = Some(me);
                        self.apply(ctx, Action::ScSync);
                    }
                    Err(e) => {
                        self.sc_busy = false;
                        if e.starts_with("401") {
                            // the token is no longer valid
                            save_sc_token(None);
                            self.api.set_token(None);
                        }
                        self.status = Some(crate::i18n::tf(
                            "Аккаунт SoundCloud: {e}",
                            &[("{e}", format!("{e}"))],
                        ));
                    }
                },
                Msg::ScSync { likes, history } => {
                    self.sc_busy = false;
                    let mut errors = Vec::new();
                    match likes {
                        Ok(l) => self.library.likes = l,
                        Err(e) => errors.push(crate::i18n::tf("лайки: {e}", &[("{e}", format!("{e}"))])),
                    }
                    match history {
                        Ok(h) if !h.is_empty() => self.library.history = h,
                        Ok(_) => {}
                        Err(e) => errors.push(crate::i18n::tf("история: {e}", &[("{e}", format!("{e}"))])),
                    }
                    self.library.save();
                    self.status = if errors.is_empty() {
                        Some(crate::i18n::tf(
                            "Синхронизировано с SoundCloud: {} лайков, {} в истории",
                            &[
                                ("{}", format!("{}", self.library.likes.len())),
                                ("{}", format!("{}", self.library.history.len())),
                            ],
                        ))
                    } else {
                        Some(crate::i18n::tf(
                            "Синхронизация частично не удалась ({})",
                            &[("{}", format!("{}", errors.join("; ")))],
                        ))
                    };
                }
                Msg::Download(ev) => match ev {
                    DlEvent::Progress(id, p) => {
                        self.downloads.insert(id, DlState::Running(p));
                    }
                    DlEvent::Done(id, path) => {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let folder = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                        self.status = Some(crate::i18n::tf(
                            "Скачано: {name} → {folder}",
                            &[("{name}", format!("{name}")), ("{folder}", format!("{folder}"))],
                        ));
                        self.downloads.insert(id, DlState::Done(path));
                    }
                    DlEvent::Failed(id, e) => {
                        self.status = Some(crate::i18n::tf(
                            "Не удалось скачать: {e}",
                            &[("{e}", format!("{e}"))],
                        ));
                        self.downloads.insert(id, DlState::Failed(e));
                    }
                },
                Msg::Playlists(r) => {
                    self.playlists_importing = false;
                    match r {
                        Ok(list) if list.is_empty() => {
                            self.status = Some(crate::i18n::t("На SoundCloud у тебя нет плейлистов").into())
                        }
                        Ok(list) => {
                            let n = list.len();
                            for (id, name, tracks) in list {
                                self.library.import_playlist(id, &name, tracks);
                            }
                            self.status = Some(crate::i18n::tf(
                                "Импортировано плейлистов: {n}",
                                &[("{n}", n.to_string())],
                            ));
                        }
                        Err(e) => {
                            self.status = Some(crate::i18n::tf("Не удалось загрузить плейлисты: {e}", &[("{e}", e)]))
                        }
                    }
                }
                Msg::Related { id, result } => {
                    if self.related_for == Some(id) {
                        self.related = result;
                    }
                }
                Msg::Stream { track_id, result } => {
                    let Some(track) = self.current_track().cloned() else {
                        continue;
                    };
                    if track.id != track_id {
                        continue;
                    }
                    self.loading_stream = false;
                    match result {
                        Ok(url) => {
                            self.fail_streak = 0;
                            self.status = None;
                            if let Some(p) = &self.player {
                                p.load(&url);
                                p.set_volume(self.volume);
                                self.loaded_at = Some(Instant::now());
                            }
                            self.library.push_history(&track);
                            self.toast = Some((track.clone(), Instant::now()));
                            if let Some(wurl) = track.waveform_url.clone() {
                                let (api, tx, ctx, id) =
                                    (self.api.clone(), self.tx.clone(), ctx.clone(), track.id);
                                std::thread::spawn(move || {
                                    if let Ok(w) = api.waveform(&wurl) {
                                        let _ = tx.send(Msg::Waveform(id, w));
                                        ctx.request_repaint();
                                    }
                                });
                            }
                            if self.sc_me.is_some() {
                                // to the SoundCloud history in batches of 10, so the background browser doesn't start for every track
                                self.pending_plays.push(track.id);
                                if self.pending_plays.len() >= 10 {
                                    self.flush_plays();
                                }
                            }
                            self.start_related(ctx, track.id);
                        }
                        Err(e) => {
                            self.status = Some(crate::i18n::tf(
                                "«{}» не воспроизводится: {e}",
                                &[("{}", format!("{}", track.title())), ("{e}", format!("{e}"))],
                            ));
                            self.fail_streak += 1;
                            if self.fail_streak < 3 {
                                if let Some(n) = self.next_index() {
                                    self.play_queue_index(ctx, n);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn list(&self, src: Source) -> &Vec<Track> {
        match src {
            Source::Results => &self.results,
            Source::Genre => &self.genre_tracks,
            Source::Related => &self.related,
            Source::Likes => &self.library.likes,
            Source::History => &self.library.history,
            Source::Playlist(id) => {
                static EMPTY: Vec<Track> = Vec::new();
                self.library.playlist(id).map_or(&EMPTY, |p| &p.tracks)
            }
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Search => self.start_search(ctx, false),
            Action::LoadMore => self.start_search(ctx, true),
            Action::StartWave(kind) => {
                if self.wave == Some(kind) {
                    // clicking the playing wave again: pause/resume
                    if let Some(p) = &self.player {
                        p.toggle_pause();
                    }
                    return;
                }
                self.wave = Some(kind);
                self.start_wave(ctx, false);
            }
            Action::PlayNext(t) => {
                if self.current.is_none() || self.queue.is_empty() {
                    self.queue = vec![t];
                    self.wave = None;
                    self.play_queue_index(ctx, 0);
                } else {
                    let at = self.current.map_or(0, |i| i + 1).min(self.queue.len());
                    self.status = Some(crate::i18n::tf("«{}» сыграет следующим", &[("{}", t.title())]));
                    self.queue.insert(at, t);
                }
            }
            Action::Enqueue(t) => {
                self.status = Some(crate::i18n::tf("«{}» добавлен в очередь", &[("{}", t.title())]));
                if self.queue.is_empty() {
                    self.queue.push(t);
                    self.play_queue_index(ctx, 0);
                } else {
                    self.queue.push(t);
                }
            }
            Action::Radio(t) => {
                self.radio_seed = Some(t.clone());
                self.wave = Some(WaveKind::Radio);
                self.queue = vec![t];
                self.play_queue_index(ctx, 0);
                self.start_wave(ctx, true);
            }
            Action::PlayQueue(i) => self.play_queue_index(ctx, i),
            Action::RemoveFromQueue(i) => {
                if i < self.queue.len() && Some(i) != self.current {
                    self.queue.remove(i);
                    if let Some(c) = self.current {
                        if i < c {
                            self.current = Some(c - 1);
                        }
                    }
                }
            }
            Action::Sleep(t) => {
                self.sleep = t;
                self.status = Some(match t {
                    None => crate::i18n::t("Таймер сна выключен").to_string(),
                    Some(SleepTimer::EndOfTrack) => crate::i18n::t("Пауза в конце этого трека").to_string(),
                    Some(SleepTimer::At(at)) => crate::i18n::tf(
                        "Пауза через {} мин",
                        &[("{}", format!("{}", (at - Instant::now()).as_secs().div_ceil(60)))],
                    ),
                });
            }
            Action::ToggleMute => match self.muted_from.take() {
                Some(v) => self.apply(ctx, Action::Volume(v)),
                None => {
                    self.muted_from = Some(self.volume.max(5.0));
                    self.apply(ctx, Action::Volume(0.0));
                }
            },
            Action::SeekBy(d) => {
                if let Some(p) = &self.player {
                    p.seek((p.position() + d).max(0.0));
                }
            }
            Action::VolumeBy(d) => {
                let v = (self.volume + d).clamp(0.0, 100.0);
                self.apply(ctx, Action::Volume(v));
            }
            Action::Relax(on) => {
                self.relax = on;
                self.relax_mouse = Instant::now();
                self.show_queue = false;
                self.show_fx = false;
                if on {
                    self.relax_prev_full = self.fullscreen;
                    self.apply(ctx, Action::Fullscreen(true));
                } else {
                    let back = self.relax_prev_full;
                    self.apply(ctx, Action::Fullscreen(back));
                }
            }
            Action::Fullscreen(on) => {
                self.fullscreen = on;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
                self.save_settings();
            }
            Action::WaveMode(m) => {
                self.wave_mode = m;
                self.radio_seed = None;
                self.wave = Some(m.kind());
                self.start_wave(ctx, false);
            }
            Action::WaveShake => {
                let kind = match self.wave {
                    Some(k) if k != WaveKind::Radio => k,
                    _ => self.wave_mode.kind(),
                };
                self.wave = Some(kind);
                self.start_wave(ctx, false);
            }
            Action::WaveToggle => {
                if self.wave.is_some() && self.current_track().is_some() {
                    if let Some(p) = &self.player {
                        p.toggle_pause();
                    }
                } else {
                    let m = self.wave_mode;
                    self.apply(ctx, Action::WaveMode(m));
                }
            }
            Action::AddToPlaylist(pid, t) => {
                let name = self.library.playlist(pid).map(|p| p.name.clone()).unwrap_or_default();
                self.status = Some(if self.library.add_to_playlist(pid, &t) {
                    crate::i18n::tf("Добавлено в «{name}»: {t}", &[("{name}", name), ("{t}", t.title())])
                } else {
                    crate::i18n::tf("Уже есть в «{name}»", &[("{name}", name)])
                });
            }
            Action::NewPlaylistWith(t) => {
                let pid = self.library.create_playlist("");
                self.apply(ctx, Action::AddToPlaylist(pid, t));
            }
            Action::RemoveFromPlaylist(pid, tid) => {
                self.library.remove_from_playlist(pid, tid);
            }
            Action::ImportPlaylists => {
                let Some(uid) = self.sc_me.as_ref().map(|m| m.id) else { return };
                if self.playlists_importing {
                    return;
                }
                self.playlists_importing = true;
                let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Playlists(api.my_playlists(uid)));
                    ctx.request_repaint();
                });
            }
            Action::Dislike => {
                if let Some(t) = self.current_track().cloned() {
                    self.library.dislike(&t);
                    self.status = Some(crate::i18n::tf(
                        "Больше не покажу «{}»",
                        &[("{}", format!("{}", t.title()))],
                    ));
                }
                if let Some(n) = self.next_index() {
                    self.play_queue_index(ctx, n);
                }
            }
            Action::Play(src, i) => {
                let list = self.list(src).clone();
                let same = self.current_track().map(|t| t.id) == list.get(i).map(|t| t.id);
                if same {
                    // clicking the playing track: pause/resume
                    if let Some(p) = &self.player {
                        p.toggle_pause();
                    }
                    return;
                }
                self.fail_streak = 0;
                self.wave = None;
                self.radio_seed = None;
                self.queue = list;
                self.play_queue_index(ctx, i);
            }
            Action::Prev => {
                if let (Some(i), Some(p)) = (self.current, &self.player) {
                    if p.position() > 3.0 || i == 0 {
                        p.seek(0.0);
                    } else {
                        self.play_queue_index(ctx, i - 1);
                    }
                }
            }
            Action::Next => {
                self.fail_streak = 0;
                // in the wave, a quick skip means "not this"
                if let (Some(_), Some(p), Some(t)) = (self.wave, &self.player, self.current_track().cloned())
                {
                    if self.loaded_at.is_some() && p.position() < 30.0 {
                        self.library.feedback(&t, -0.5);
                    }
                }
                if let Some(n) = self.next_index() {
                    self.play_queue_index(ctx, n);
                }
            }
            Action::TogglePause => {
                if let (Some(p), Some(_)) = (&self.player, self.current) {
                    p.toggle_pause();
                }
            }
            Action::Seek(s) => {
                if let Some(p) = &self.player {
                    p.seek(s);
                }
            }
            Action::Volume(v) => {
                self.volume = v;
                if let Some(p) = &self.player {
                    p.set_volume(v);
                }
                self.save_settings();
            }
            Action::Like(t) => {
                self.library.toggle_like(&t);
                self.flush_plays(); // the browser will start anyway, so send the history too
                if let Some(me) = &self.sc_me {
                    let liked = self.library.is_liked(t.id);
                    let (api, tx, ctx, uid) = (self.api.clone(), self.tx.clone(), ctx.clone(), me.id);
                    std::thread::spawn(move || {
                        if let Err(e) = api.set_like(uid, t.id, liked) {
                            let _ = tx.send(Msg::Status(crate::i18n::tf(
                                "Лайк не ушёл в SoundCloud: {e}",
                                &[("{e}", format!("{e}"))],
                            )));
                            ctx.request_repaint();
                        }
                    });
                }
            }
            Action::ScLogin => {
                if self.sc_busy {
                    return;
                }
                self.sc_busy = true;
                self.status = Some(crate::i18n::t("Открыл окно входа SoundCloud…").into());
                let (tx, ctx) = (self.tx.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let r = login::login_blocking();
                    let _ = tx.send(Msg::ScToken(r));
                    ctx.request_repaint();
                });
            }
            Action::ScLogout => {
                save_sc_token(None);
                self.api.set_token(None);
                login::forget_webview_session();
                self.sc_me = None;
                self.status = Some(
                    crate::i18n::t("Вышел из аккаунта SoundCloud. Лайки и история остались локально").into(),
                );
            }
            Action::ScSync => {
                let Some(me) = &self.sc_me else { return };
                self.sc_busy = true;
                let (api, tx, ctx, uid) = (self.api.clone(), self.tx.clone(), ctx.clone(), me.id);
                std::thread::spawn(move || {
                    let likes = api.my_likes(uid, 2000);
                    let history = api.play_history();
                    let _ = tx.send(Msg::ScSync { likes, history });
                    ctx.request_repaint();
                });
            }
            Action::Genre(k) => self.start_genre(ctx, k),
            Action::Go(p) => {
                self.show_lyrics = false;
                self.page = p;
                if p == Page::Stats && self.stats.is_none() && !self.stats_loading {
                    self.apply(ctx, Action::LoadStats);
                }
            }
            Action::LoadStats => {
                if self.sc_me.is_none() || self.stats_loading {
                    return;
                }
                self.stats_loading = true;
                self.stats_progress = 0;
                let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let progress = |n: usize| {
                        let _ = tx.send(Msg::StatsProgress(n));
                        ctx.request_repaint();
                    };
                    let r = api.play_history_full(300, &progress).map(stats::compute);
                    let _ = tx.send(Msg::StatsDone(r));
                    ctx.request_repaint();
                });
            }
            Action::Shuffle => self.shuffle = !self.shuffle,
            Action::CopyLink(t) => match &t.permalink_url {
                Some(url) => {
                    ctx.copy_text(url.clone());
                    self.status = Some(crate::i18n::tf(
                        "Ссылка скопирована: {url}",
                        &[("{url}", format!("{url}"))],
                    ));
                }
                None => self.status = Some(crate::i18n::t("У этого трека нет ссылки").into()),
            },
            Action::Download(t) => {
                if matches!(self.downloads.get(&t.id), Some(DlState::Running(_))) {
                    return;
                }
                self.downloads.insert(t.id, DlState::Running(0.0));
                let (api, tx, ctx, fx) = (self.api.clone(), self.tx.clone(), ctx.clone(), self.applied_fx);
                let dir = self.download_path();
                std::thread::spawn(move || {
                    downloader::download(&api, &t, fx, dir, &|ev| {
                        let _ = tx.send(Msg::Download(ev));
                        ctx.request_repaint();
                    });
                });
            }
            Action::Reveal(id) => match self.downloads.get(&id) {
                Some(DlState::Done(p)) => downloader::reveal(p),
                _ => {
                    let dir = self.download_path();
                    let _ = std::fs::create_dir_all(&dir);
                    downloader::reveal(&dir);
                }
            },
            Action::Repeat => {
                self.repeat = match self.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                }
            }
        }
    }

    fn apply_fx(&mut self, ctx: &egui::Context) {
        if self.fx == self.applied_fx {
            return;
        }
        let Some(p) = &self.player else { return };
        // speed applies at once, filters when the slider is released (rebuilding filters causes a click)
        if self.fx.speed != self.applied_fx.speed {
            p.set_speed(self.fx.speed as f64);
            self.applied_fx.speed = self.fx.speed;
        }
        if ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        if self.fx.af() != self.applied_fx.af() {
            p.set_af(&self.fx.af());
        }
        self.applied_fx = self.fx;
        self.save_settings();
    }

    fn download_path(&self) -> std::path::PathBuf {
        if self.download_dir.trim().is_empty() {
            downloader::default_download_dir()
        } else {
            std::path::PathBuf::from(self.download_dir.trim())
        }
    }

    fn start_wave(&mut self, ctx: &egui::Context, append: bool) {
        let Some(kind) = self.wave else { return };
        self.wave_loading = true;
        if !append {
            self.status = Some(crate::i18n::tf(
                "Собираю «{}»…",
                &[("{}", format!("{}", kind.title()))],
            ));
        }
        let in_queue: &[Track] = if append { &self.queue } else { &[] };
        let profile = wave::Profile::build(
            kind,
            &self.library,
            in_queue,
            &self.genre_tracks,
            self.radio_seed.as_ref(),
        )
        .with_mode(if kind == WaveKind::Radio { WaveMode::Mix } else { self.wave_mode });
        let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let tracks = wave::generate(&api, &profile, 15);
            let _ = tx.send(Msg::Wave { kind, tracks, append });
            ctx.request_repaint();
        });
    }

    /// While music plays the taskbar icon's equalizer "jumps"; on pause it freezes.
    fn animate_window_icon(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let playing = self.animate_taskbar
            && self.player.as_ref().is_some_and(|p| !p.is_paused())
            && self.loaded_at.is_some();
        // theme-colored icon: re-render when the theme changes
        let target = self.theme.palette();
        let rethemed = crate::logo::retheme(&target.grad, target.angle);
        let frames = crate::logo::frames();
        if rethemed {
            set_icon(ctx, frame, &frames, self.icon_frame.map_or(0, |f| f + 1));
        } else if crate::logo::retheme_pending() {
            ctx.request_repaint_after(Duration::from_millis(450));
        }
        if playing {
            // ~3 frames per second: noticeable, but doesn't strain the taskbar (or TranslucentTB etc.)
            if self.icon_frame.is_none() || self.icon_changed.elapsed() >= Duration::from_millis(320) {
                let next = self.icon_frame.map_or(0, |f| (f + 1) % 8);
                self.icon_frame = Some(next);
                self.icon_changed = Instant::now();
                set_icon(ctx, frame, &frames, next + 1);
            }
            ctx.request_repaint_after(Duration::from_millis(320));
        } else if self.icon_frame.take().is_some() {
            set_icon(ctx, frame, &frames, 0);
        }
    }

    fn flush_plays(&mut self) {
        if self.sc_me.is_none() || self.pending_plays.is_empty() {
            return;
        }
        let plays = std::mem::take(&mut self.pending_plays);
        let api = self.api.clone();
        std::thread::spawn(move || {
            for id in plays {
                let _ = api.record_play(id);
            }
        });
    }

    fn fetch_me(&mut self, ctx: &egui::Context) {
        self.sc_busy = true;
        let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let r = api.me();
            let _ = tx.send(Msg::ScMe(r));
            ctx.request_repaint();
        });
    }

    fn save_settings(&self) {
        save_settings(&serde_json::json!({
            "discord_enabled": self.discord_enabled,
            "discord_app_id": self.discord_app_id,
            "volume": self.volume,
            "fx": self.fx,
            "download_dir": self.download_dir,
            "theme": self.theme,
            "lang": self.lang_code,
            "saved_themes": self.saved_themes,
            "animate_taskbar": self.animate_taskbar,
            "fullscreen": self.fullscreen,
        }));
    }

    // ================= Discord =================

    fn sync_discord(&mut self) {
        let active = self.loaded_at.is_some() || self.loading_stream;
        let desired = match (&self.player, self.current_track(), active) {
            (Some(p), Some(t), true) if self.discord_enabled => Some(Presence {
                title: t.title(),
                artist: t.artist(),
                artwork: t.artwork("t500x500"),
                url: t.permalink_url.clone(),
                position: p.position(),
                duration: p.duration(),
                paused: p.is_paused() || self.loading_stream,
                speed: self.applied_fx.speed as f64,
                fx_label: self.applied_fx.label(),
            }),
            _ => None,
        };
        let changed = match (&desired, &self.last_presence) {
            (None, None) => false,
            (Some(_), None) | (None, Some(_)) => true,
            (Some(d), Some((last, at))) => {
                let expected = if last.paused {
                    last.position
                } else {
                    last.position + at.elapsed().as_secs_f64() * last.speed
                };
                d.title != last.title
                    || d.artist != last.artist
                    || d.paused != last.paused
                    || d.fx_label != last.fx_label
                    || (d.speed - last.speed).abs() > 0.01
                    || (d.duration - last.duration).abs() > 1.0
                    || (!d.paused && (d.position - expected).abs() > 3.0)
            }
        };
        if !changed {
            return;
        }
        match &desired {
            Some(p) => self.discord.set(p.clone()),
            None => self.discord.clear(),
        }
        self.last_presence = desired.map(|p| (p, Instant::now()));
    }

    // ================= top bar =================

    fn ui_topbar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        // there is no Windows title bar, so the window is dragged by our header
        // (widgets added later capture clicks in their own areas)
        let drag = ui.interact(
            ui.max_rect().expand(10.0),
            egui::Id::new("titlebar-drag"),
            Sense::click_and_drag(),
        );
        if drag.double_clicked() {
            // "maximize" = full screen over the taskbar
            actions.push(Action::Fullscreen(!self.fullscreen));
        } else if drag.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            let playing = self.player.as_ref().is_some_and(|p| !p.is_paused()) && self.loaded_at.is_some();
            let logo = crate::logo::header(
                ui,
                c_accent(),
                c_text(),
                theme::pal().grad[0],
                egui::FontId::new(19.0, fam_display()),
                playing,
            );
            if logo.clicked() {
                actions.push(Action::Go(Page::Home));
            }
            if nav_tab(ui, crate::i18n::t("Главная"), self.page == Page::Home) {
                actions.push(Action::Go(Page::Home));
            }
            if nav_tab(ui, crate::i18n::t("Библиотека"), self.page == Page::Library) {
                actions.push(Action::Go(Page::Library));
            }
            if nav_tab(ui, crate::i18n::t("Статистика"), self.page == Page::Stats) {
                actions.push(Action::Go(Page::Stats));
            }
            if !self.last_query.is_empty() && nav_tab(ui, crate::i18n::t("Поиск"), self.page == Page::Search)
            {
                actions.push(Action::Go(Page::Search));
            }

            ui.add_space(10.0);
            let w = (ui.available_width() - 60.0).clamp(200.0, 560.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .id(egui::Id::new("search-field"))
                    .hint_text(crate::i18n::t("Поиск"))
                    .desired_width(w)
                    .margin(Vec2::new(12.0, 7.0)),
            );
            self.search_rect = Some(resp.rect);
            if resp.gained_focus() || resp.clicked() {
                // with an empty field, show recent searches
                self.show_suggestions = true;
            }
            if resp.changed() {
                self.query_edited_at = Some(Instant::now());
                self.show_suggestions = true;
            }
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                actions.push(Action::Search);
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.show_suggestions = false;
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                window_buttons(ui);
                ui.add_space(6.0);
                if gear_btn(ui) {
                    self.show_settings = !self.show_settings;
                }
                ui.add_space(8.0);
                match &self.sc_me {
                    Some(me) => {
                        let r = ui
                            .add(
                                egui::Label::new(RichText::new(&me.username).size(13.0).color(c_text()))
                                    .sense(Sense::click()),
                            )
                            .on_hover_text(crate::i18n::t("Аккаунт SoundCloud — настройки"));
                        if r.clicked() {
                            self.show_settings = true;
                        }
                        avatar_img(ui, me.avatar_url.clone(), &me.username, 28.0);
                    }
                    None => {
                        let text = if self.sc_busy {
                            crate::i18n::t("Вход…")
                        } else {
                            crate::i18n::t("Войти")
                        };
                        let b = egui::Button::new(RichText::new(text).family(fam_semi()).color(c_text()))
                            .fill(c_accent())
                            .rounding(4.0);
                        if ui
                            .add_enabled(!self.sc_busy, b)
                            .on_hover_text(crate::i18n::t("Войти в SoundCloud"))
                            .clicked()
                        {
                            actions.push(Action::ScLogin);
                        }
                    }
                }
            });
        });
    }

    /// Dropdown under the search box: suggestions while typing,
    /// and recent searches when the field is empty.
    fn ui_suggestions(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.show_suggestions {
            return;
        }
        let history_mode = self.query.trim().is_empty();
        if (history_mode && self.library.searches.is_empty())
            || (!history_mode && self.suggestions.is_empty())
        {
            return;
        }
        let Some(r) = self.search_rect else { return };
        let mut chosen = None;
        let mut remove: Option<usize> = None;
        let mut clear = false;
        let area = egui::Area::new(egui::Id::new("suggestions"))
            .order(egui::Order::Foreground)
            .fixed_pos(r.left_bottom() + Vec2::new(0.0, 4.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(Margin::same(8.0))
                    .show(ui, |ui| {
                        ui.set_width(r.width() - 16.0);
                        if history_mode {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(crate::i18n::t("Недавние запросы"))
                                        .size(13.0)
                                        .color(c_muted()),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let c = egui::Label::new(
                                        RichText::new(crate::i18n::t("Очистить"))
                                            .size(13.0)
                                            .color(c_muted()),
                                    )
                                    .sense(Sense::click());
                                    if ui
                                        .add(c)
                                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                                        .clicked()
                                    {
                                        clear = true;
                                    }
                                });
                            });
                            ui.add_space(2.0);
                            for (i, q) in self.library.searches.iter().take(10).enumerate() {
                                ui.horizontal(|ui| {
                                    let w = ui.available_width() - 28.0;
                                    let item = ui.add_sized(
                                        [w, 26.0],
                                        egui::SelectableLabel::new(false, format!("🕘  {q}")),
                                    );
                                    if item.clicked() {
                                        chosen = Some(q.clone());
                                    }
                                    if ui
                                        .add(
                                            egui::Button::new(RichText::new("✕").size(12.0).color(c_muted()))
                                                .frame(false),
                                        )
                                        .on_hover_text(crate::i18n::t("Удалить из истории"))
                                        .clicked()
                                    {
                                        remove = Some(i);
                                    }
                                });
                            }
                        } else {
                            for s in &self.suggestions {
                                if ui.selectable_label(false, format!("🔍  {s}")).clicked() {
                                    chosen = Some(s.clone());
                                }
                            }
                        }
                    });
            });
        if let Some(i) = remove {
            self.library.searches.remove(i);
            self.library.save();
        } else if clear {
            self.library.searches.clear();
            self.library.save();
            self.show_suggestions = false;
        } else if let Some(s) = chosen {
            self.query = s;
            actions.push(Action::Search);
        } else if ctx.input(|i| i.pointer.any_click()) {
            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                if !area.response.rect.contains(p) && !r.contains(p) {
                    self.show_suggestions = false;
                }
            }
        }
    }

    // ================= pages =================

    /// Relax mode: full screen; in the center the artwork, title, lyrics (karaoke) and player.
    /// Background: a blurred "glow" from the artwork and color blobs; controls hide while the mouse is idle.
    fn ui_relax(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let rect = ui.max_rect();
        let ctx = ui.ctx().clone();
        let time = ui.input(|i| i.time) as f32;
        ctx.request_repaint_after(Duration::from_millis(33));
        if ui.input(|i| i.pointer.delta() != Vec2::ZERO || i.pointer.any_pressed()) {
            self.relax_mouse = Instant::now();
        }
        let idle = self.relax_mouse.elapsed().as_secs_f32();
        let show_ui = ctx.animate_bool_with_time(ui.id().with("relax-ui"), idle < 2.5, 0.5);
        if idle > 3.0 {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        let track = self.current_track().cloned();
        let playing = self.player.as_ref().is_some_and(|p| !p.is_paused()) && self.loaded_at.is_some();
        let live = ctx.animate_bool_with_time(ui.id().with("relax-live"), playing, 0.8);
        let (pos, dur) = self.player.as_ref().map_or((0.0, 0.0), |p| (p.position(), p.duration()));
        let p = ui.painter().with_clip_rect(rect);

        // ---- loudness of the track "right now" from the SoundCloud waveform: the background breathes with the beat ----
        let samples: Option<&[f32]> = self
            .waveform
            .as_ref()
            .filter(|w| track.as_ref().is_some_and(|t| t.id == w.0))
            .map(|w| w.1.as_slice());
        let level = |at: f64| -> f32 {
            match samples {
                Some(s) if !s.is_empty() && dur > 0.0 => {
                    let i = ((at / dur).clamp(0.0, 1.0) * (s.len() - 1) as f64) as usize;
                    s[i]
                }
                _ => 0.5,
            }
        };
        let now_lvl = level(pos);
        let prev_lvl = level((pos - 0.25).max(0.0));
        // "kick": a sharp rise in loudness + a steady pulse of ~2 beats/s
        let kick = ((now_lvl - prev_lvl).max(0.0) * 4.0).min(1.0);
        let pulse = ((time * std::f32::consts::PI * 2.0).sin().max(0.0)).powi(6);
        let beat_raw = (kick * 0.7 + pulse * 0.3 * now_lvl) * live;
        let beat = ctx.animate_value_with_time(ui.id().with("relax-beat"), beat_raw, 0.12);
        let energy = ctx.animate_value_with_time(ui.id().with("relax-energy"), now_lvl * live, 0.4);

        // ---- background like "My Wave": dark with bright living blobs ----
        p.rect_filled(rect, 0.0, Color32::from_rgb(10, 8, 16));
        let unit = rect.height();
        let pal = theme::pal();
        let blobs = [
            (c_accent(), Vec2::new(-0.30, -0.10), 0.48, 0.0),
            (c_wave_col(), Vec2::new(0.32, 0.12), 0.42, 1.9),
            (mix(pal.grad[2], Color32::WHITE, 0.2), Vec2::new(-0.05, 0.30), 0.40, 3.7),
            (mix(c_accent(), Color32::WHITE, 0.4), Vec2::new(0.05, -0.05), 0.22, 5.3),
        ];
        let speed = 0.25 + 0.5 * live + 0.6 * energy;
        for (i, (col, off, r, seed)) in blobs.iter().enumerate() {
            let t = time * speed + seed;
            let drift = Vec2::new((t * 0.37 + i as f32).sin() * 0.10, (t * 0.29 + i as f32 * 1.3).cos() * 0.08);
            let c = rect.center() + (*off + drift) * Vec2::new(rect.width() * 0.8, unit);
            let rad = r * unit * (0.9 + 0.25 * energy + 0.18 * beat);
            let alpha = (0.55 + 0.35 * energy + 0.2 * beat).min(1.0) * if i == 3 { 0.6 } else { 1.0 };
            blob(&p, c, rad, col.gamma_multiply(alpha), t, *seed);
        }
        // darkening in the center under the text and a soft vignette
        let mut m = egui::Mesh::default();
        let center = rect.center();
        m.colored_vertex(center, Color32::from_black_alpha(90));
        let n = 48u32;
        for k in 0..n {
            let a = k as f32 / n as f32 * std::f32::consts::TAU;
            let v = Vec2::angled(a);
            m.colored_vertex(center + Vec2::new(v.x * rect.width(), v.y * rect.height()) * 0.8, Color32::from_black_alpha(170));
        }
        for k in 0..n {
            m.add_triangle(0, 1 + k, 1 + (k + 1) % n);
        }
        p.add(m);

        // ---- centered layout: artwork → title and artist → waveform → buttons → lyrics ----
        let h = rect.height();
        let cover_s = (h * 0.28).min(rect.width() * 0.25).clamp(150.0, 380.0);
        let wave_w = (rect.width() * 0.55).clamp(320.0, 900.0);
        let mut y = rect.top() + (h * 0.06).max(30.0);
        let cx = rect.center().x;
        let art_url = track.as_ref().and_then(|t| t.artwork("t500x500"));

        // artwork: sways and gently "bumps" to the beat
        let bob = (time * 1.1).sin() * 4.0 * live;
        let cover = Rect::from_center_size(
            Pos2::new(cx, y + cover_s / 2.0 + bob),
            Vec2::splat(cover_s * (1.0 + 0.035 * beat)),
        );
        for k in 1..=5 {
            let e = k as f32 * 6.0;
            p.rect_filled(
                cover.translate(Vec2::new(0.0, 14.0)).expand(e),
                20.0 + e,
                Color32::from_black_alpha((70 / k) as u8),
            );
        }
        match &art_url {
            Some(u) => {
                egui::Image::new(u.as_str()).rounding(20.0).paint_at(ui, cover);
            }
            None => {
                let g = theme::pal().grad;
                gradient(&p, cover, g[1], g[theme::N - 2]);
                crate::logo::paint(&p, cover.shrink(cover_s * 0.25), Color32::WHITE, Color32::TRANSPARENT, playing.then_some(time));
            }
        }
        let cover_click = ui.interact(cover, ui.id().with("relax-cover"), Sense::click());
        if cover_click.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            actions.push(Action::TogglePause);
        }
        y += cover_s + 26.0;

        // title and artist
        if let Some(t) = &track {
            let mut job = egui::text::LayoutJob::single_section(
                t.title(),
                egui::TextFormat { font_id: egui::FontId::new(28.0, fam_display()), color: Color32::WHITE, ..Default::default() },
            );
            job.wrap.max_width = rect.width() * 0.7;
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let g = ctx.fonts(|f| f.layout_job(job));
            p.galley(Pos2::new(cx - g.size().x / 2.0, y), g, Color32::WHITE);
            p.text(
                Pos2::new(cx, y + 40.0),
                egui::Align2::CENTER_TOP,
                t.artist(),
                egui::FontId::new(16.0, fam_semi()),
                Color32::from_white_alpha(175),
            );
        }
        y += 72.0;

        // waveform: the same live waveform as in the player
        let wave_rect = Rect::from_min_size(Pos2::new(cx - wave_w / 2.0, y), Vec2::new(wave_w, 44.0));
        let mut wave_ui = ui.new_child(egui::UiBuilder::new().max_rect(wave_rect));
        let key = track.as_ref().map_or(0, |t| t.id);
        if let Some(target) = waveform_seek(&mut wave_ui, key, playing, samples, pos, dur, wave_w, 44.0, &mut self.seek_drag) {
            actions.push(Action::Seek(target));
        }
        let small = egui::FontId::new(12.0, fam_semi());
        let shown = self.seek_drag.unwrap_or(pos);
        p.text(Pos2::new(wave_rect.left(), wave_rect.bottom() + 8.0), egui::Align2::LEFT_TOP, fmt_time(shown), small.clone(), Color32::from_white_alpha(170));
        p.text(Pos2::new(wave_rect.right(), wave_rect.bottom() + 8.0), egui::Align2::RIGHT_TOP, fmt_time(dur), small, Color32::from_white_alpha(170));
        y += 44.0 + 14.0;

        // buttons (compact, under the waveform)
        let row_y = y + 30.0;
        let pr = Rect::from_center_size(Pos2::new(cx, row_y), Vec2::splat(56.0));
        let presp = ui.interact(pr, ui.id().with("relax-play"), Sense::click());
        let ph = anim(ui, presp.id, presp.hovered());
        let rad = 26.0 * (1.0 + 0.08 * ease_out_back(ph) + 0.05 * beat) - 3.0 * press_t(ui, &presp);
        p.circle_filled(pr.center(), rad, Color32::WHITE);
        let c = pr.center();
        if playing {
            for dx in [-5.5, 5.5] {
                p.rect_filled(Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(5.0, 18.0)), 2.0, INK);
            }
        } else {
            p.add(egui::Shape::convex_polygon(
                vec![c + Vec2::new(-6.0, -10.0), c + Vec2::new(-6.0, 10.0), c + Vec2::new(11.0, 0.0)],
                INK,
                Stroke::NONE,
            ));
        }
        if presp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            actions.push(Action::TogglePause);
        }
        let side = |dx: f32| Rect::from_center_size(Pos2::new(cx + dx, row_y), Vec2::splat(42.0));
        if hero_icon(ui, &p, side(-76.0), "relax-prev", HeroIcon::Prev, false) {
            actions.push(Action::Prev);
        }
        if hero_icon(ui, &p, side(76.0), "relax-next", HeroIcon::Next, false) {
            actions.push(Action::Next);
        }
        if let Some(t) = &track {
            let liked = self.library.is_liked(t.id);
            if hero_icon(ui, &p, side(-146.0), "relax-like", HeroIcon::Heart, liked) {
                actions.push(Action::Like(t.clone()));
            }
            if self.wave.is_some() && hero_icon(ui, &p, side(146.0), "relax-dislike", HeroIcon::Dislike, false) {
                actions.push(Action::Dislike);
            }
        }
        y += 72.0;

        // lyrics: all the remaining space, lines centered
        let lyr_rect = Rect::from_min_max(
            Pos2::new(rect.left() + rect.width() * 0.1, y),
            Pos2::new(rect.right() - rect.width() * 0.1, rect.bottom() - 24.0),
        );
        if lyr_rect.height() > 60.0 {
            let lyr = self
                .lyrics
                .as_ref()
                .filter(|x| track.as_ref().is_some_and(|t| t.id == x.0))
                .and_then(|x| x.1.clone());
            match lyr {
                Some(Some(l)) if !l.synced.is_empty() => {
                    if let Some(start) = relax_lyrics(ui, lyr_rect, &l.synced, pos - self.lyrics_offset) {
                        actions.push(Action::Seek((start + self.lyrics_offset).max(0.0)));
                    }
                }
                Some(None) | Some(Some(_)) => {
                    p.text(lyr_rect.center(), egui::Align2::CENTER_CENTER, "♪", egui::FontId::proportional(40.0), Color32::from_white_alpha(90));
                }
                None => {
                    p.text(lyr_rect.center(), egui::Align2::CENTER_CENTER, "• • •", egui::FontId::proportional(24.0), Color32::from_white_alpha(90));
                }
            }
        }

        // exit (top-right corner), visible while the mouse moves
        if show_ui > 0.01 {
            let ex = Rect::from_center_size(rect.right_top() + Vec2::new(-40.0, 40.0), Vec2::splat(44.0));
            let er = ui.interact(ex, ui.id().with("relax-exit"), Sense::click());
            let eh = anim(ui, er.id, er.hovered());
            p.circle_filled(ex.center(), 20.0, Color32::from_white_alpha((20.0 + 30.0 * eh) as u8).gamma_multiply(show_ui));
            let col = Color32::from_white_alpha(220).gamma_multiply(show_ui);
            let c = ex.center();
            p.line_segment([c + Vec2::new(-6.0, -6.0), c + Vec2::new(6.0, 6.0)], Stroke::new(2.0_f32, col));
            p.line_segment([c + Vec2::new(6.0, -6.0), c + Vec2::new(-6.0, 6.0)], Stroke::new(2.0_f32, col));
            p.text(
                ex.left_center() - Vec2::new(10.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                crate::i18n::t("Esc — выйти"),
                egui::FontId::proportional(12.0),
                Color32::from_white_alpha(140).gamma_multiply(show_ui),
            );
            if er.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                actions.push(Action::Relax(false));
            }
        }
    }

    /// "My Wave": a big living block with shimmering color blobs, a large heading,
    /// artwork, a track pill and buttons. On the left: wave modes and waves for favorite artists.
    fn ui_wave_hero(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let w = ui.available_width();
        let h = 460.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
        let time = ui.input(|i| i.time) as f32;
        let playing_wave = self.wave.is_some()
            && self.current_track().is_some()
            && self.player.as_ref().is_some_and(|p| !p.is_paused());
        let live = ui.ctx().animate_bool_with_time(ui.id().with("wave-live"), playing_wave, 0.8);
        ui.ctx().request_repaint_after(Duration::from_millis(33));

        // ---- background and blobs ----
        let p = ui.painter().with_clip_rect(rect);
        p.rect_filled(rect, 18.0, mix(theme::pal().window, Color32::BLACK, 0.55));
        let list_w = (w * 0.3).clamp(230.0, 320.0);
        let stage = Rect::from_min_max(Pos2::new(rect.left() + list_w, rect.top()), rect.max);
        // "beat": short bumps while playing
        let beat = ((time * std::f32::consts::PI * 1.9).sin().max(0.0)).powi(8) * live;
        let speed = 0.35 + 0.65 * live;
        let pal = theme::pal();
        let blobs = [
            (c_accent(), Vec2::new(0.0, 0.05), 0.42, 0.0),
            (c_wave_col(), Vec2::new(0.12, 0.12), 0.30, 1.9),
            (mix(pal.grad[2], Color32::WHITE, 0.25), Vec2::new(-0.16, -0.08), 0.27, 3.7),
            (mix(c_accent(), Color32::WHITE, 0.45), Vec2::new(0.04, -0.02), 0.16, 5.3),
        ];
        let sc = stage.center();
        let unit = stage.height();
        for (i, (col, off, r, seed)) in blobs.iter().enumerate() {
            let t = time * speed + seed;
            let drift = Vec2::new((t * 0.37 + i as f32).sin() * 0.09, (t * 0.29 + i as f32 * 1.3).cos() * 0.07);
            let c = sc + (*off + drift) * unit;
            let rad = r * unit * (1.0 + 0.05 * (t * 0.8).sin() + 0.08 * beat);
            let alpha = if i == 3 { 0.55 } else { 0.85 };
            blob(&p, c, rad, col.gamma_multiply(alpha), t, *seed);
        }
        // darkening under the left list for readability
        let mut shade = egui::Mesh::default();
        let lr = Rect::from_min_max(rect.min, Pos2::new(rect.left() + list_w + 80.0, rect.bottom()));
        let dark = Color32::from_black_alpha(170);
        shade.colored_vertex(lr.left_top(), dark);
        shade.colored_vertex(lr.right_top(), Color32::TRANSPARENT);
        shade.colored_vertex(lr.right_bottom(), Color32::TRANSPARENT);
        shade.colored_vertex(lr.left_bottom(), dark);
        shade.add_triangle(0, 1, 2);
        shade.add_triangle(0, 2, 3);
        p.add(shade);

        // ---- center: heading, artwork, pill, buttons ----
        let title = crate::i18n::t("Моя волна");
        let size = (stage.width() * 0.11).clamp(40.0, 76.0);
        let tg = p.layout_no_wrap(title.to_string(), egui::FontId::new(size, fam_display()), Color32::WHITE);
        let ty = stage.top() + stage.height() * 0.13;
        let tpos = Pos2::new(sc.x - tg.size().x / 2.0, ty);
        p.galley(tpos + Vec2::new(0.0, 3.0), tg.clone(), Color32::from_black_alpha(90));
        p.galley(tpos, tg.clone(), Color32::WHITE);
        let sub = if self.wave_loading {
            crate::i18n::t("Собираю треки…").to_string()
        } else {
            self.wave_mode.title().to_string()
        };
        p.text(
            Pos2::new(sc.x, ty + tg.size().y + 6.0),
            egui::Align2::CENTER_TOP,
            sub,
            egui::FontId::new(14.0, fam_semi()),
            Color32::from_white_alpha(200),
        );

        let track = self.wave.and(self.current_track().cloned());
        let cover_y = ty + tg.size().y + 44.0;
        let cover_s = 96.0;
        let bob = (time * 1.3).sin() * 3.0 * live;
        let cover = Rect::from_center_size(
            Pos2::new(sc.x, cover_y + cover_s / 2.0 + bob),
            Vec2::splat(cover_s * (1.0 + 0.04 * beat)),
        );
        let cover_resp = ui.interact(cover, ui.id().with("wave-cover"), Sense::click());
        match track.as_ref().and_then(|t| t.artwork("t300x300")) {
            Some(u) => {
                p.rect_filled(cover.translate(Vec2::new(0.0, 6.0)), 12.0, Color32::from_black_alpha(80));
                egui::Image::new(u).rounding(12.0).paint_at(ui, cover);
            }
            None => {
                p.rect_filled(cover, 14.0, Color32::from_white_alpha(28));
                crate::logo::paint(&p, cover.shrink(22.0), Color32::from_white_alpha(230), Color32::TRANSPARENT, playing_wave.then_some(time));
            }
        }
        if cover_resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            if track.is_some() {
                self.show_lyrics = true;
            } else {
                actions.push(Action::WaveToggle);
            }
        }

        // track pill
        let pill_w = (stage.width() * 0.72).clamp(300.0, 560.0);
        let pill = Rect::from_center_size(
            Pos2::new(sc.x, cover.bottom() + 22.0 + 26.0),
            Vec2::new(pill_w, 52.0),
        );
        p.rect_filled(pill, 26.0, Color32::from_black_alpha(110));
        p.rect_stroke(pill, 26.0, Stroke::new(1.0_f32, Color32::from_white_alpha(22)));
        let text = match &track {
            Some(t) => format!("{} — {}", t.artist(), t.title()),
            None => crate::i18n::t("Нажми ▶ — волна подстроится под тебя").to_string(),
        };
        let mut job = egui::text::LayoutJob::single_section(
            text,
            egui::TextFormat {
                font_id: egui::FontId::new(15.0, fam_semi()),
                color: Color32::from_white_alpha(235),
                ..Default::default()
            },
        );
        job.wrap.max_width = pill_w - 130.0;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let g = ui.ctx().fonts(|f| f.layout_job(job));
        p.galley(pill.center() - g.size() / 2.0, g, Color32::WHITE);
        if let Some(t) = &track {
            // "dislike" on the left, like on the right
            let dl = Rect::from_center_size(pill.left_center() + Vec2::new(30.0, 0.0), Vec2::splat(36.0));
            if hero_icon(ui, &p, dl, "wave-dislike", HeroIcon::Dislike, false) {
                actions.push(Action::Dislike);
            }
            let lk = Rect::from_center_size(pill.right_center() - Vec2::new(30.0, 0.0), Vec2::splat(36.0));
            if hero_icon(ui, &p, lk, "wave-like", HeroIcon::Heart, self.library.is_liked(t.id)) {
                actions.push(Action::Like(t.clone()));
            }
        }

        // buttons
        let cy = pill.bottom() + 46.0;
        let play = Rect::from_center_size(Pos2::new(sc.x, cy), Vec2::splat(64.0));
        let pr = ui.interact(play, ui.id().with("wave-play"), Sense::click());
        let ph = anim(ui, pr.id, pr.hovered());
        let pr_r = 30.0 * (1.0 + 0.08 * ease_out_back(ph) + 0.06 * beat) - 3.0 * press_t(ui, &pr);
        p.circle_filled(play.center() + Vec2::new(0.0, 4.0), pr_r, Color32::from_black_alpha(70));
        p.circle_filled(play.center(), pr_r, c_accent());
        let c = play.center();
        if playing_wave {
            for dx in [-6.0, 6.0] {
                p.rect_filled(Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(6.0, 20.0)), 2.0, INK);
            }
        } else if self.wave_loading {
            let a = time * 4.0;
            for k in 0..3 {
                let ang = a + k as f32 * 2.09;
                p.circle_filled(c + Vec2::angled(ang) * 9.0, 3.0, INK);
            }
        } else {
            p.add(egui::Shape::convex_polygon(
                vec![c + Vec2::new(-7.0, -12.0), c + Vec2::new(-7.0, 12.0), c + Vec2::new(13.0, 0.0)],
                INK,
                Stroke::NONE,
            ));
        }
        if pr.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            actions.push(Action::WaveToggle);
        }
        let has = track.is_some();
        let prev = Rect::from_center_size(Pos2::new(sc.x - 78.0, cy), Vec2::splat(40.0));
        if hero_icon(ui, &p, prev, "wave-prev", HeroIcon::Prev, false) && has {
            actions.push(Action::Prev);
        }
        let next = Rect::from_center_size(Pos2::new(sc.x + 78.0, cy), Vec2::splat(40.0));
        if hero_icon(ui, &p, next, "wave-next", HeroIcon::Next, false) && has {
            actions.push(Action::Next);
        }

        // ---- left: modes and artists ----
        let mut y = rect.top() + 20.0;
        let row_h = 46.0;
        let left = rect.left() + 18.0;
        let mut row = |ui: &mut Ui, id: &str, label: &str, small: Option<&str>, icon: &dyn Fn(&egui::Painter, Pos2, f32), active: bool| -> bool {
            let r = Rect::from_min_size(Pos2::new(left, y), Vec2::new(list_w - 30.0, row_h));
            y += row_h + 4.0;
            if r.bottom() > rect.bottom() - 8.0 {
                return false;
            }
            let resp = ui.interact(r, ui.id().with(("wave-row", id)), Sense::click());
            let hv = anim(ui, resp.id, resp.hovered());
            let sp = ease_out_back(hv);
            let pt = ui.painter().with_clip_rect(rect);
            if active || hv > 0.01 {
                pt.rect_filled(r, 12.0, Color32::from_white_alpha((14.0 + 14.0 * hv) as u8));
            }
            let ic = Pos2::new(r.left() + 26.0 + 4.0 * sp, r.center().y);
            icon(&pt, ic, 1.0 + 0.12 * sp);
            let tx = r.left() + 60.0 + 4.0 * sp;
            let col = if active { c_accent() } else { Color32::from_white_alpha(235) };
            match small {
                Some(sm) => {
                    pt.text(Pos2::new(tx, r.center().y - 1.0), egui::Align2::LEFT_BOTTOM, sm, egui::FontId::proportional(11.0), Color32::from_white_alpha(140));
                    pt.text(Pos2::new(tx, r.center().y), egui::Align2::LEFT_TOP, label, egui::FontId::new(14.0, fam_semi()), col);
                }
                None => {
                    pt.text(Pos2::new(tx, r.center().y), egui::Align2::LEFT_CENTER, label, egui::FontId::new(14.0, fam_semi()), col);
                }
            }
            resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
        };

        let shake_t = time;
        if row(
            ui,
            "shake",
            crate::i18n::t("Встряхнуть волну"),
            None,
            &|pt: &egui::Painter, c: Pos2, s: f32| {
                // wavy "lightning"
                let pts: Vec<Pos2> = (0..=20)
                    .map(|k| {
                        let x = (k as f32 / 20.0 - 0.5) * 30.0 * s;
                        c + Vec2::new(x, (x * 0.35 + shake_t * 3.0).sin() * 7.0 * s)
                    })
                    .collect();
                pt.add(egui::Shape::line(pts, Stroke::new(3.0 * s, Color32::from_rgb(210, 220, 60))));
            },
            false,
        ) {
            actions.push(Action::WaveShake);
        }
        let mode_col = |m: WaveMode| match m {
            WaveMode::Mix => c_accent(),
            WaveMode::Discover => Color32::from_rgb(255, 70, 80),
            WaveMode::Favorite => Color32::from_rgb(255, 90, 170),
            WaveMode::Popular => Color32::from_rgb(255, 170, 40),
            WaveMode::Calm => Color32::from_rgb(80, 170, 255),
            WaveMode::Energetic => Color32::from_rgb(40, 220, 150),
            WaveMode::NoWords => Color32::from_rgb(130, 100, 255),
        };
        for (k, m) in WaveMode::ALL.iter().enumerate() {
            let col = mode_col(*m);
            let active = self.wave.is_some_and(|w| w != WaveKind::Radio) && self.wave_mode == *m;
            let seed = k as f32 * 1.7 + 0.5;
            if row(
                ui,
                &format!("mode{k}"),
                m.title(),
                None,
                &|pt: &egui::Painter, c: Pos2, s: f32| blob(pt, c, 15.0 * s, col, time * 0.8 + seed, seed),
                active,
            ) {
                actions.push(Action::WaveMode(*m));
            }
        }
        // waves for favorite artists: from history, by play count
        let mut counts: Vec<(u64, usize, Track)> = Vec::new();
        for t in self.library.history.iter().take(150) {
            let uid = t.user.as_ref().and_then(|u| u.id).unwrap_or(0);
            match counts.iter_mut().find(|c| c.0 == uid) {
                Some(c) => c.1 += 1,
                None => counts.push((uid, 1, t.clone())),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        for (uid, _, t) in counts.into_iter().take(3) {
            let avatar = t.user.as_ref().and_then(|u| u.avatar_url.clone());
            let name = t.artist();
            if row(
                ui,
                &format!("artist{uid}"),
                &name,
                Some(crate::i18n::t("Артист")),
                &|pt: &egui::Painter, c: Pos2, s: f32| {
                    let r = Rect::from_center_size(c, Vec2::splat(36.0 * s));
                    pt.circle_filled(c, 18.0 * s + 1.5, Color32::from_white_alpha(60));
                    if let Some(u) = &avatar {
                        let img = egui::Image::new(u.as_str()).rounding(18.0 * s);
                        let ctx = pt.ctx().clone();
                        if let Ok(egui::load::TexturePoll::Ready { texture }) =
                            img.load_for_size(&ctx, r.size())
                        {
                            // round avatar: a fully rounded rectangle with a texture
                            let mut shape = egui::epaint::RectShape::filled(r, 18.0 * s, Color32::WHITE);
                            shape.fill_texture_id = texture.id;
                            shape.uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                            pt.add(shape);
                        }
                    }
                },
                false,
            ) {
                actions.push(Action::Radio(t.clone()));
            }
        }
    }

    fn ui_home(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let playing = self.current_track().map(|t| t.id);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(16.0);
            self.ui_wave_hero(ui, actions);
            ui.add_space(14.0);
            if likes_banner(ui, self.library.likes.len()) {
                actions.push(Action::Go(Page::Library));
            }

            if !self.library.history.is_empty() {
                section_title(ui, crate::i18n::t("Недавно слушал"));
                card_row(
                    ui,
                    "recent",
                    &self.library.history,
                    Source::History,
                    playing,
                    actions,
                );
            }
            if !self.related.is_empty() {
                section_title(ui, crate::i18n::t("Ещё в твоём вкусе"));
                card_row(ui, "related", &self.related, Source::Related, playing, actions);
            }

            section_title(ui, crate::i18n::t("Тренды по жанрам"));
            ui.horizontal_wrapped(|ui| {
                for (key, label, _) in GENRES {
                    if chip(ui, crate::i18n::t(label), self.genre == *key) {
                        actions.push(Action::Genre(*key));
                    }
                }
            });
            ui.add_space(10.0);
            if self.genre_loading && self.genre_tracks.is_empty() {
                ui.spinner();
            } else {
                let tracks = &self.genre_tracks;
                let shown = tracks.len().min(15);
                ui.columns(3, |cols| {
                    for i in 0..shown {
                        let ui = &mut cols[i % 3];
                        if small_row(ui, &tracks[i], playing == Some(tracks[i].id)) {
                            actions.push(Action::Play(Source::Genre, i));
                        }
                    }
                });
            }
            ui.add_space(24.0);
        });
    }

    fn ui_search(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let playing = self.current_track().map(|t| t.id);
        ui.add_space(14.0);
        ui.label(
            RichText::new(crate::i18n::tf(
                "Результаты по запросу «{}»",
                &[("{}", format!("{}", self.last_query))],
            ))
            .size(20.0)
            .family(fam_display())
            .color(c_text()),
        );
        ui.add_space(8.0);
        if self.results.is_empty() {
            if self.searching {
                ui.spinner();
            } else {
                ui.label(RichText::new(crate::i18n::t("Ничего не найдено")).color(c_muted()));
            }
            return;
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (i, t) in self.results.iter().enumerate() {
                let dl = self.downloads.get(&t.id);
                let r = track_row(ui, t, playing == Some(t.id), self.library.is_liked(t.id), dl);
                r.push(actions, Source::Results, i, t);
            }
            ui.add_space(10.0);
            if self.searching {
                ui.vertical_centered(|ui| ui.spinner());
            } else if self.next_offset.is_some() {
                ui.vertical_centered(|ui| {
                    if ui.button(crate::i18n::t("Загрузить ещё")).clicked() {
                        actions.push(Action::LoadMore);
                    }
                });
            }
            ui.add_space(20.0);
        });
    }

    fn ui_library(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let playing = self.current_track().map(|t| t.id);
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            for (tab, label, n) in [
                (LibTab::Likes, "Лайки  {}", self.library.likes.len()),
                (LibTab::History, "История  {}", self.library.history.len()),
                (LibTab::Playlists, "Плейлисты  {}", self.library.playlists.len()),
            ] {
                if chip(ui, &crate::i18n::tf(label, &[("{}", n.to_string())]), self.lib_tab == tab) {
                    self.lib_tab = tab;
                    if tab == LibTab::Playlists {
                        self.open_playlist = None;
                    }
                }
            }
        });
        ui.add_space(8.0);
        if self.lib_tab == LibTab::Playlists {
            match self.open_playlist {
                Some(pid) if self.library.playlist(pid).is_some() => self.ui_playlist(ui, pid, actions),
                _ => self.ui_playlists_grid(ui, actions),
            }
            return;
        }
        let (src, list) = match self.lib_tab {
            LibTab::History => (Source::History, &self.library.history),
            _ => (Source::Likes, &self.library.likes),
        };
        if list.is_empty() {
            ui.label(
                RichText::new(match self.lib_tab {
                    LibTab::History => crate::i18n::t("Ты ещё ничего не слушал"),
                    _ => crate::i18n::t("Пока пусто — жми ♥ у трека, и он появится здесь"),
                })
                .color(c_muted()),
            );
            return;
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (i, t) in list.iter().enumerate() {
                let dl = self.downloads.get(&t.id);
                let r = track_row(ui, t, playing == Some(t.id), self.library.is_liked(t.id), dl);
                r.push(actions, src, i, t);
            }
            ui.add_space(20.0);
        });
    }

    /// Playlist grid: a "Create" tile, then playlists (artwork is a mosaic of 4 tracks).
    fn ui_playlists_grid(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::t("Правый клик по любому треку → «Добавить в плейлист»"))
                    .size(12.0)
                    .color(c_muted()),
            );
            if self.sc_me.is_some() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.playlists_importing {
                        ui.spinner();
                    } else if chip(ui, crate::i18n::t("Импорт из SoundCloud"), false) {
                        actions.push(Action::ImportPlaylists);
                    }
                });
            }
        });
        ui.add_space(12.0);
        let size = 170.0;
        let mut open = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(20.0, 20.0);
                // "Create playlist"
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(size, size + 44.0), Sense::click());
                let h = anim(ui, resp.id, resp.hovered());
                let tile = Rect::from_min_size(rect.min, Vec2::splat(size)).shrink(4.0 * (1.0 - h));
                let p = ui.painter();
                p.rect_filled(tile, 12.0, mix(c_surface(), c_hover(), h));
                p.rect_stroke(tile, 12.0, Stroke::new(1.5_f32, c_accent().gamma_multiply(0.35 + 0.5 * h)));
                let c = tile.center();
                let r = 18.0 + 3.0 * ease_out_back(h);
                p.circle_filled(c, r, c_accent().gamma_multiply(0.85));
                p.line_segment([c - Vec2::new(7.0, 0.0), c + Vec2::new(7.0, 0.0)], Stroke::new(2.5_f32, INK));
                p.line_segment([c - Vec2::new(0.0, 7.0), c + Vec2::new(0.0, 7.0)], Stroke::new(2.5_f32, INK));
                p.text(
                    Pos2::new(rect.left(), tile.bottom() + 10.0),
                    egui::Align2::LEFT_TOP,
                    crate::i18n::t("Новый плейлист"),
                    egui::FontId::new(13.0, fam_semi()),
                    c_text(),
                );
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    let id = self.library.create_playlist("");
                    let name = self.library.playlist(id).map(|p| p.name.clone()).unwrap_or_default();
                    self.open_playlist = Some(id);
                    self.playlist_rename = Some((id, name));
                }

                for pl in &self.library.playlists {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(size, size + 44.0), Sense::click());
                    let h = anim(ui, resp.id, resp.hovered());
                    let lift = 4.0 * ease_out_back(h);
                    let tile = Rect::from_min_size(rect.min - Vec2::new(0.0, lift), Vec2::splat(size));
                    playlist_cover(ui, tile, &pl.tracks);
                    shine(ui, tile, resp.id, resp.hovered());
                    let p = ui.painter();
                    p.text(
                        Pos2::new(rect.left(), tile.bottom() + 8.0),
                        egui::Align2::LEFT_TOP,
                        &pl.name,
                        egui::FontId::new(13.0, fam_semi()),
                        c_text(),
                    );
                    let sub = if pl.sc_id.is_some() {
                        format!("SoundCloud · {}", crate::i18n::tf("{count} треков", &[("{count}", pl.tracks.len().to_string())]))
                    } else {
                        crate::i18n::tf("{count} треков", &[("{count}", pl.tracks.len().to_string())])
                    };
                    p.text(
                        Pos2::new(rect.left(), tile.bottom() + 26.0),
                        egui::Align2::LEFT_TOP,
                        sub,
                        egui::FontId::proportional(12.0),
                        c_muted(),
                    );
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        open = Some(pl.id);
                    }
                }
            });
            ui.add_space(30.0);
        });
        if open.is_some() {
            self.open_playlist = open;
            self.playlist_confirm_delete = None;
        }
    }

    /// Open playlist: a header with artwork and actions, tracks below.
    fn ui_playlist(&mut self, ui: &mut Ui, pid: u64, actions: &mut Vec<Action>) {
        let playing = self.current_track().map(|t| t.id);
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("menu-playlist"), Some(pid)));
        let Some(pl) = self.library.playlist(pid).cloned() else { return };
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(150.0), Sense::hover());
            playlist_cover(ui, rect, &pl.tracks);
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                if chip(ui, crate::i18n::t("← Все плейлисты"), false) {
                    self.open_playlist = None;
                }
                ui.add_space(6.0);
                let mut commit = None;
                if let Some((id, name)) = &mut self.playlist_rename {
                    if *id == pid {
                        let r = ui.add(
                            egui::TextEdit::singleline(name)
                                .font(egui::FontId::new(22.0, fam_display()))
                                .desired_width(360.0),
                        );
                        if !r.has_focus() && !r.lost_focus() {
                            r.request_focus();
                        }
                        if r.lost_focus() {
                            commit = Some(name.clone());
                        }
                    }
                }
                if let Some(name) = commit {
                    self.library.rename_playlist(pid, &name);
                    self.playlist_rename = None;
                } else if !self.playlist_rename.as_ref().is_some_and(|r| r.0 == pid) {
                    ui.label(RichText::new(&pl.name).size(26.0).family(fam_display()).color(c_text()));
                }
                ui.label(
                    RichText::new(crate::i18n::tf("{count} треков", &[("{count}", pl.tracks.len().to_string())]))
                        .color(c_muted()),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if !pl.tracks.is_empty() && chip(ui, crate::i18n::t("▶ Слушать"), true) {
                        actions.push(Action::Play(Source::Playlist(pid), 0));
                    }
                    if chip(ui, crate::i18n::t("Переименовать"), false) {
                        self.playlist_rename = Some((pid, pl.name.clone()));
                    }
                    if self.playlist_confirm_delete == Some(pid) {
                        if chip(ui, crate::i18n::t("Точно удалить?"), true) {
                            self.library.delete_playlist(pid);
                            self.open_playlist = None;
                            self.playlist_confirm_delete = None;
                        }
                        if chip(ui, crate::i18n::t("Отмена"), false) {
                            self.playlist_confirm_delete = None;
                        }
                    } else if chip(ui, crate::i18n::t("Удалить"), false) {
                        self.playlist_confirm_delete = Some(pid);
                    }
                });
            });
        });
        ui.add_space(14.0);
        if pl.tracks.is_empty() {
            ui.label(
                RichText::new(crate::i18n::t(
                    "Плейлист пуст. Правый клик по треку → «Добавить в плейлист».",
                ))
                .color(c_muted()),
            );
            return;
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (i, t) in pl.tracks.iter().enumerate() {
                let dl = self.downloads.get(&t.id);
                let r = track_row(ui, t, playing == Some(t.id), self.library.is_liked(t.id), dl);
                r.push(actions, Source::Playlist(pid), i, t);
            }
            ui.add_space(20.0);
        });
    }

    fn ui_stats(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.add_space(16.0);
        if self.sc_me.is_none() {
            ui.vertical_centered(|ui| {
                ui.add_space(90.0);
                ui.label(
                    RichText::new(crate::i18n::t("Сколько ты слушаешь?"))
                        .size(28.0)
                        .family(fam_semi())
                        .color(c_text()),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(crate::i18n::t(
                        "Статистика считается по истории твоего аккаунта SoundCloud.",
                    ))
                    .color(c_muted()),
                );
                ui.add_space(16.0);
                let b = egui::Button::new(
                    RichText::new(crate::i18n::t("Войти в SoundCloud"))
                        .size(16.0)
                        .family(fam_semi())
                        .color(c_text()),
                )
                .fill(c_accent())
                .rounding(6.0)
                .min_size(Vec2::new(260.0, 44.0));
                if ui.add_enabled(!self.sc_busy, b).clicked() {
                    actions.push(Action::ScLogin);
                }
            });
            return;
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::t("Статистика"))
                    .size(24.0)
                    .family(fam_display())
                    .color(c_text()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.stats_loading,
                        egui::Button::new(crate::i18n::t("🔄 Обновить")),
                    )
                    .clicked()
                {
                    actions.push(Action::LoadStats);
                }
                if self.stats_loading {
                    ui.label(
                        RichText::new(crate::i18n::tf(
                            "загружаю историю… {} прослушиваний",
                            &[("{}", format!("{}", self.stats_progress))],
                        ))
                        .color(c_muted()),
                    );
                    ui.spinner();
                }
            });
        });
        let Some(st) = &self.stats else {
            return;
        };
        if let Some(oldest) = st.oldest {
            let full_year = oldest <= chrono::NaiveDate::from_ymd_opt(st.year_num, 1, 1).unwrap_or(oldest);
            let mut note = crate::i18n::tf(
                "История с {date} · {plays} прослушиваний · {uniq} разных треков",
                &[
                    ("{date}", oldest.format("%d.%m.%Y").to_string()),
                    ("{plays}", st.plays.to_string()),
                    ("{uniq}", st.unique_tracks.to_string()),
                ],
            );
            if !full_year {
                note.push_str(crate::i18n::t(" · SoundCloud хранит не всю историю, за год может быть больше"));
            }
            ui.label(RichText::new(note).size(12.0).color(c_muted()));
        } else {
            ui.label(RichText::new(crate::i18n::t("В истории SoundCloud пока пусто")).color(c_muted()));
            return;
        }
        ui.add_space(12.0);

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            stat_tiles(
                ui,
                &[
                    (crate::i18n::tf("За {} год", &[("{}", format!("{}", st.year_num))]), fmt_hours(st.year)),
                    (crate::i18n::t("За месяц").to_string(), fmt_hours(st.month)),
                    (crate::i18n::t("За 7 дней").to_string(), fmt_hours(st.week)),
                    (crate::i18n::t("Сегодня").to_string(), fmt_hours(st.today)),
                    (
                        crate::i18n::t("Серия").to_string(),
                        crate::i18n::tf("{} дн.", &[("{}", format!("{}", st.streak))]),
                    ),
                ],
            );

            section_title(
                ui,
                &crate::i18n::tf("{} по месяцам", &[("{}", format!("{}", st.year_num))]),
            );
            let months: Vec<(String, f64)> = st
                .by_month
                .iter()
                .map(|(m, v)| (crate::i18n::t(m).to_string(), *v))
                .collect();
            bar_chart(ui, "months", &months, true);

            section_title(ui, crate::i18n::t("Последние 30 дней"));
            let days: Vec<(String, f64)> = st
                .last30
                .iter()
                .map(|(d, v)| (d.format("%d.%m").to_string(), *v))
                .collect();
            bar_chart(ui, "days", &days, false);

            ui.add_space(8.0);
            ui.columns(2, |cols| {
                section_title(&mut cols[0], crate::i18n::t("Топ артистов"));
                let max = st.top_artists.first().map(|a| a.1).unwrap_or(1.0).max(1.0);
                for (i, (name, secs)) in st.top_artists.iter().enumerate() {
                    let ui = &mut cols[0];
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [22.0, 20.0],
                            egui::Label::new(
                                RichText::new(format!("{}", i + 1))
                                    .family(fam_semi())
                                    .color(c_muted()),
                            ),
                        );
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(name).family(fam_semi()).color(c_text()));
                                ui.label(RichText::new(fmt_hours(*secs)).size(12.0).color(c_muted()));
                            });
                            let w = (ui.available_width() - 8.0).max(40.0);
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 4.0), Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, c_text().gamma_multiply(0.08));
                            let grow = ui.ctx().animate_bool_with_time(ui.id().with(("artist-bar", i)), true, 0.7);
                            let mut bar = rect;
                            bar.set_width(rect.width() * (*secs / max) as f32 * grow);
                            ui.painter().rect_filled(bar, 2.0, c_accent());
                        });
                    });
                    ui.add_space(6.0);
                }

                section_title(&mut cols[1], crate::i18n::t("Топ треков"));
                for (t, secs, n) in &st.top_tracks {
                    let ui = &mut cols[1];
                    ui.horizontal(|ui| {
                        let _ = art(ui, t.artwork("t67x67"), 44.0);
                        ui.vertical(|ui| {
                            ui.set_max_width((ui.available_width() - 110.0).max(80.0));
                            ui.add(
                                egui::Label::new(
                                    RichText::new(t.title())
                                        .size(13.0)
                                        .family(fam_semi())
                                        .color(c_text()),
                                )
                                .truncate(),
                            );
                            ui.add(
                                egui::Label::new(RichText::new(t.artist()).size(12.0).color(c_muted()))
                                    .truncate(),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("{} · {n}×", fmt_hours(*secs)))
                                    .size(12.0)
                                    .color(c_muted()),
                            );
                        });
                    });
                    ui.add_space(4.0);
                }
            });
            ui.add_space(10.0);
            ui.label(
                RichText::new(crate::i18n::t(
                    "Время оценивается по истории: длина трека или время до следующего запуска — что меньше.",
                ))
                .size(11.0)
                .color(c_muted()),
            );
            ui.add_space(24.0);
        });
    }

    fn ui_sidebar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let playing = self.current_track().map(|t| t.id);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(16.0);
            sidebar_header(
                ui,
                &crate::i18n::tf("Лайки  {}", &[("{}", format!("{}", self.library.likes.len()))]),
                || actions.push(Action::Go(Page::Library)),
            );
            if self.library.likes.is_empty() {
                ui.label(
                    RichText::new(crate::i18n::t("Жми ♥ у треков"))
                        .small()
                        .color(c_muted()),
                );
            }
            for (i, t) in self.library.likes.iter().take(3).enumerate() {
                if side_row(ui, t, playing == Some(t.id)) {
                    actions.push(Action::Play(Source::Likes, i));
                }
            }
            ui.add_space(18.0);
            sidebar_header(ui, crate::i18n::t("Недавно играло"), || {
                actions.push(Action::Go(Page::Library))
            });
            for (i, t) in self.library.history.iter().take(5).enumerate() {
                if side_row(ui, t, playing == Some(t.id)) {
                    actions.push(Action::Play(Source::History, i));
                }
            }
        });
    }

    // ================= player =================

    fn ui_player(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let track = self.current_track().cloned();
        let liked = track.as_ref().is_some_and(|t| self.library.is_liked(t.id));
        let (pos, dur, paused) = match (&self.player, track.is_some()) {
            (Some(p), true) => (p.position(), p.duration(), p.is_paused()),
            _ => (0.0, 0.0, true),
        };

        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            if icon_btn(ui, "⏮", 20.0, c_text()) {
                actions.push(Action::Prev);
            }
            if play_button(ui, paused, self.loading_stream) {
                actions.push(Action::TogglePause);
            }
            if icon_btn(ui, "⏭", 20.0, c_text()) {
                actions.push(Action::Next);
            }
            if icon_btn(ui, "🔀", 16.0, if self.shuffle { c_accent() } else { c_muted() }) {
                actions.push(Action::Shuffle);
            }
            let (rep_icon, rep_col) = match self.repeat {
                Repeat::Off => ("🔁", c_muted()),
                Repeat::All => ("🔁", c_accent()),
                Repeat::One => ("🔂", c_accent()),
            };
            if icon_btn(ui, rep_icon, 16.0, rep_col) {
                actions.push(Action::Repeat);
            }

            // The right side is laid out from the right edge (FX is always visible),
            // and the progress bar takes exactly the remaining space.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let fx_on = self.fx.is_active();
                if fx_button(ui, fx_on) {
                    self.show_fx = !self.show_fx;
                }

                if let Some(kind) = self.wave {
                    ui.label(RichText::new("🌊").size(16.0).color(c_wave_col()))
                        .on_hover_text(kind.title());
                    if ui
                        .add(egui::Button::new(RichText::new("👎").size(16.0).color(c_muted())).frame(false))
                        .on_hover_text(crate::i18n::t("Не нравится — больше не показывать"))
                        .clicked()
                    {
                        actions.push(Action::Dislike);
                    }
                }

                // relax mode (F10)
                if relax_button(ui).on_hover_text(crate::i18n::t("Режим отдыха (F10)")).clicked() {
                    actions.push(Action::Relax(true));
                }
                // queue and sleep timer
                if icon_btn(
                    ui,
                    "☰",
                    16.0,
                    if self.show_queue { c_accent() } else { c_muted() },
                ) {
                    self.show_queue = !self.show_queue;
                }
                let moon_col = if self.sleep.is_some() {
                    c_accent()
                } else {
                    c_muted()
                };
                let moon_resp = moon_button(ui, 15.0, moon_col, self.sleep.is_some());
                let moon_popup = ui.make_persistent_id("sleep-popup");
                if moon_resp.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(moon_popup));
                }
                egui::popup_above_or_below_widget(
                    ui,
                    moon_popup,
                    &moon_resp,
                    egui::AboveOrBelow::Above,
                    egui::PopupCloseBehavior::CloseOnClickOutside,
                    |ui| {
                        ui.set_min_width(170.0);
                        ui.label(RichText::new(crate::i18n::t("Таймер сна")).family(fam_semi()));
                        let mut close = false;
                        for m in [15u64, 30, 45, 60, 90] {
                            if ui
                                .button(crate::i18n::tf("Через {} мин", &[("{}", m.to_string())]))
                                .clicked()
                            {
                                actions.push(Action::Sleep(Some(SleepTimer::At(
                                    Instant::now() + Duration::from_secs(m * 60),
                                ))));
                                close = true;
                            }
                        }
                        if ui.button(crate::i18n::t("В конце трека")).clicked() {
                            actions.push(Action::Sleep(Some(SleepTimer::EndOfTrack)));
                            close = true;
                        }
                        if self.sleep.is_some() && ui.button(crate::i18n::t("Выключить")).clicked() {
                            actions.push(Action::Sleep(None));
                            close = true;
                        }
                        if close {
                            ui.memory_mut(|m| m.close_popup());
                        }
                    },
                );
                let moon = egui::InnerResponse::new((), moon_resp);
                if let Some(SleepTimer::At(at)) = self.sleep {
                    let left = at.saturating_duration_since(Instant::now()).as_secs();
                    moon.response.on_hover_text(crate::i18n::tf(
                        "Пауза через {}:{}",
                        &[
                            ("{}", (left / 60).to_string()),
                            ("{}", format!("{:02}", left % 60)),
                        ],
                    ));
                }

                if let Some(t) = &track {
                    if link_button(ui) {
                        actions.push(Action::CopyLink(t.clone()));
                    }
                    let mut r = RowResp::default();
                    download_button(ui, self.downloads.get(&t.id), &mut r);
                    if r.download {
                        actions.push(Action::Download(t.clone()));
                    }
                    if r.reveal {
                        actions.push(Action::Reveal(t.id));
                    }
                    if heart_btn(ui, liked) {
                        actions.push(Action::Like(t.clone()));
                    }
                    ui.allocate_ui_with_layout(Vec2::new(190.0, 40.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(190.0);
                        ui.add(
                            egui::Label::new(RichText::new(t.artist()).size(11.0).color(c_muted()))
                                .truncate(),
                        );
                        let mut title = t.title();
                        if let Some(l) = self.applied_fx.label() {
                            title = format!("{title} ({l})");
                        }
                        let tl = egui::Label::new(
                            RichText::new(title).size(12.0).family(fam_semi()).color(c_text()),
                        )
                        .truncate()
                        .sense(Sense::click());
                        let r = ui
                            .add(tl)
                            .on_hover_text(crate::i18n::t("Открыть на soundcloud.com"));
                        if r.clicked() {
                            if let Some(url) = &t.permalink_url {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                            }
                        }
                    });
                    let cover = art(ui, t.artwork("t67x67"), 38.0);
                    if cover.on_hover_text(crate::i18n::t("Текст песни")).clicked() {
                        self.show_lyrics = !self.show_lyrics;
                        self.page_changed = Instant::now();
                    }
                }

                // volume
                if let Some(vol) = volume_bar(ui, self.volume) {
                    actions.push(Action::Volume(vol));
                }
                ui.label(RichText::new("🔊").color(c_muted()));

                // progress: total time | bar | current time (right to left)
                let shown = self.seek_drag.unwrap_or(pos);
                ui.label(RichText::new(fmt_time(dur)).size(12.0).color(c_text()));
                let samples = self
                    .waveform
                    .as_ref()
                    .filter(|w| track.as_ref().map(|t| t.id) == Some(w.0))
                    .map(|w| w.1.as_slice());
                let width = (ui.available_width() - 60.0).max(80.0);
                let key = track.as_ref().map(|t| t.id).unwrap_or(0);
                if let Some(target) = waveform_seek(
                    ui,
                    key,
                    self.player.as_ref().is_some_and(|p| !p.is_paused()) && self.loaded_at.is_some(),
                    samples,
                    pos,
                    dur,
                    width,
                    36.0,
                    &mut self.seek_drag,
                ) {
                    actions.push(Action::Seek(target));
                }
                ui.label(RichText::new(fmt_time(shown)).size(12.0).color(c_accent()));
            });
        });
    }

    /// Hotkeys (when not typing in a text field).
    fn hotkeys(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use egui::Key;
        let ctrl_f = ctx.input(|i| i.modifiers.command && i.key_pressed(Key::F));
        if ctrl_f {
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("search-field")));
            self.show_suggestions = true;
            return;
        }
        if ctx.wants_keyboard_input() || self.lyrics_edit.as_ref().is_some_and(|e| e.sync.is_some()) {
            return;
        }
        ctx.input(|i| {
            let shift = i.modifiers.shift;
            for (key, act) in [
                (Key::Space, Action::TogglePause),
                (Key::ArrowRight, Action::SeekBy(if shift { 30.0 } else { 5.0 })),
                (Key::ArrowLeft, Action::SeekBy(if shift { -30.0 } else { -5.0 })),
                (Key::ArrowUp, Action::VolumeBy(5.0)),
                (Key::ArrowDown, Action::VolumeBy(-5.0)),
                (Key::M, Action::ToggleMute),
                (Key::N, Action::Next),
                (Key::P, Action::Prev),
            ] {
                if i.key_pressed(key) {
                    actions.push(act);
                }
            }
            if i.key_pressed(Key::F1) {
                self.show_help = !self.show_help;
            }
            if i.key_pressed(Key::F11) {
                actions.push(Action::Fullscreen(!self.fullscreen));
            }
            if i.key_pressed(Key::F10) {
                actions.push(Action::Relax(!self.relax));
            }
            if i.key_pressed(Key::Escape) {
                if self.relax {
                    actions.push(Action::Relax(false));
                }
                self.show_help = false;
                self.show_queue = false;
                self.show_fx = false;
                self.show_lyrics = false;
            }
        });
        if ctx.input(|i| i.key_pressed(Key::L)) {
            if let Some(t) = self.current_track().cloned() {
                actions.push(Action::Like(t));
            }
        }
    }

    /// Windows system media overlay: title, artwork, state; updated on track change/pause.
    fn sync_media(&mut self, _ctx: &egui::Context) {
        let paused = self.player.as_ref().map_or(true, |p| p.is_paused()) || self.loaded_at.is_none();
        let pos = self.player.as_ref().map_or(0.0, |p| p.position());
        let dur = self.player.as_ref().map_or(0.0, |p| p.duration());
        match self.current_track() {
            Some(t) => {
                let (title, artist, art) = (t.title(), t.artist(), t.artwork("t500x500"));
                self.media
                    .update(t.id, &title, &artist, art.as_deref(), dur, pos, paused);
            }
            None => self.media.update(0, "", "", None, 0.0, 0.0, true),
        }
    }

    /// "Queue" window: what plays next.
    fn ui_queue(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.show_queue {
            return;
        }
        let mut open = true;
        egui::Window::new(crate::i18n::t("Очередь"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -76.0))
            .default_width(360.0)
            .vscroll(true)
            .max_height(520.0)
            .show(ctx, |ui| {
                let cur = self.current.unwrap_or(0);
                if let Some(t) = self.current_track() {
                    ui.label(
                        RichText::new(crate::i18n::t("Сейчас играет"))
                            .size(12.0)
                            .color(c_muted()),
                    );
                    ui.label(RichText::new(t.title()).family(fam_semi()).color(c_accent()));
                    ui.add_space(8.0);
                }
                ui.label(RichText::new(crate::i18n::t("Далее")).size(12.0).color(c_muted()));
                let upcoming: Vec<(usize, Track)> = self
                    .queue
                    .iter()
                    .enumerate()
                    .skip(cur + 1)
                    .take(50)
                    .map(|(i, t)| (i, t.clone()))
                    .collect();
                if upcoming.is_empty() {
                    ui.label(
                        RichText::new(crate::i18n::t(
                            "Пусто — правый клик по треку → «Добавить в очередь»",
                        ))
                        .size(12.0)
                        .color(c_muted()),
                    );
                }
                for (i, t) in upcoming {
                    ui.horizontal(|ui| {
                        let r = art(ui, t.artwork("t67x67"), 36.0);
                        if r.clicked() {
                            actions.push(Action::PlayQueue(i));
                        }
                        ui.vertical(|ui| {
                            ui.set_max_width(220.0);
                            ui.add(
                                egui::Label::new(RichText::new(t.title()).size(13.0).family(fam_semi()))
                                    .truncate(),
                            );
                            ui.add(
                                egui::Label::new(RichText::new(t.artist()).size(11.0).color(c_muted()))
                                    .truncate(),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(AnimIcon("✕", 13.0, c_muted()))
                                .on_hover_text(crate::i18n::t("Убрать из очереди"))
                                .clicked()
                            {
                                actions.push(Action::RemoveFromQueue(i));
                            }
                        });
                    });
                }
            });
        if !open {
            self.show_queue = false;
        }
    }

    /// Lyrics screen: artwork on the left, lines on the right that scroll with the music.
    /// Start AI lyrics alignment for the current track.
    fn start_align(&mut self, ctx: &egui::Context, id: u64, force: bool) {
        if self.aligning.is_some() || !crate::align::available() {
            return;
        }
        if !force && !self.align_tried.insert(id) {
            return;
        }
        let Some(t) = self.current_track().cloned().filter(|t| t.id == id) else { return };
        let lines: Vec<String> = match self.lyrics.as_ref().and_then(|x| x.1.clone()).flatten() {
            Some(l) if !l.synced.is_empty() => l.synced.iter().map(|x| x.1.clone()).collect(),
            Some(l) => l
                .plain
                .unwrap_or_default()
                .lines()
                .map(str::trim)
                .filter(|x| !x.is_empty() && !(x.starts_with('[') && x.ends_with(']')))
                .map(str::to_string)
                .collect(),
            None => return,
        };
        if lines.len() < 2 {
            return;
        }
        self.aligning = Some((id, 0.0));
        let dur = t.full_duration.unwrap_or(t.duration) as f64 / 1000.0;
        let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let r = api.stream_url_fresh(&t).and_then(|url| {
                let tx2 = tx.clone();
                let ctx2 = ctx.clone();
                crate::align::align(&url, &lines, dur, &move |p| {
                    let _ = tx2.send(Msg::AlignProgress(id, p));
                    ctx2.request_repaint();
                })
            });
            let _ = tx.send(Msg::Aligned(id, r));
            ctx.request_repaint();
        });
    }

    fn ui_lyrics(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let Some(t) = self.current_track().cloned() else {
            self.show_lyrics = false;
            return;
        };
        let pos = self.player.as_ref().map_or(0.0, |p| p.position());
        let state = self
            .lyrics
            .as_ref()
            .filter(|x| x.0 == t.id)
            .and_then(|x| x.1.clone());
        let found = state.clone().flatten();
        ui.add_space(20.0);
        let full = ui.available_rect_before_wrap();
        let left_w = (full.width() * 0.26).clamp(200.0, 280.0);
        let left = Rect::from_min_size(full.min, Vec2::new(left_w, full.height()));
        let right = Rect::from_min_max(Pos2::new(left.right() + 40.0, full.top()), full.max);
        ui.allocate_rect(full, Sense::hover());

        // ---- left: artwork, title, actions ----
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(left), |ui| {
            let cover = art(ui, t.artwork("t500x500"), left_w);
            if cover.clicked() {
                self.show_lyrics = false;
            }
            ui.add_space(14.0);
            ui.add(
                egui::Label::new(RichText::new(t.title()).size(19.0).family(fam_semi()).color(c_text())).wrap(),
            );
            ui.add(egui::Label::new(RichText::new(t.artist()).size(14.0).color(c_muted())).truncate());
            ui.add_space(14.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                if chip(ui, crate::i18n::t("Закрыть текст"), false) {
                    self.show_lyrics = false;
                }
                let edit_label = if found.as_ref().is_some_and(|l| l.source == "own") {
                    crate::i18n::t("Изменить свой текст")
                } else {
                    crate::i18n::t("Свой текст")
                };
                if chip(ui, edit_label, false) {
                    let text = match &found {
                        Some(l) if !l.synced.is_empty() && l.source == "own" && !l.approx => {
                            crate::lyrics::to_lrc(&l.synced)
                        }
                        Some(l) => l.plain.clone().unwrap_or_default(),
                        None => String::new(),
                    };
                    self.lyrics_edit = Some(LyricsEdit { track_id: t.id, text, sync: None });
                }
            });
            if let Some(l) = &found {
                ui.add_space(12.0);
                if !l.synced.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(crate::i18n::t("Сдвиг")).size(12.0).color(c_muted()));
                        if chip(ui, "−0.5", false) {
                            self.lyrics_offset -= 0.5;
                            crate::lyrics::save_offset(t.id, self.lyrics_offset);
                        }
                        ui.label(
                            RichText::new(format!("{:+.1} s", self.lyrics_offset))
                                .size(12.0)
                                .family(fam_semi())
                                .color(c_text()),
                        );
                        if chip(ui, "+0.5", false) {
                            self.lyrics_offset += 0.5;
                            crate::lyrics::save_offset(t.id, self.lyrics_offset);
                        }
                        if self.lyrics_offset != 0.0 && chip(ui, "↺", false) {
                            self.lyrics_offset = 0.0;
                            crate::lyrics::save_offset(t.id, 0.0);
                        }
                    });
                    ui.add_space(6.0);
                }
                let src = match l.source {
                    "own" => crate::i18n::t("свой текст").to_string(),
                    "AI" => crate::i18n::t("подогнан нейросетью").to_string(),
                    s => s.to_string(),
                };
                let hint = if l.approx {
                    crate::i18n::tf(
                        "Текст: {src} · время строк примерное. ПКМ по строке, которая звучит сейчас, — подгоню весь текст",
                        &[("{src}", src)],
                    )
                } else if l.synced.is_empty() {
                    crate::i18n::tf("Текст: {src} · без таймкодов", &[("{src}", src)])
                } else {
                    crate::i18n::tf(
                        "Текст: {src} · клик по строке — перемотка, ПКМ — «звучит сейчас» (подогнать время)",
                        &[("{src}", src)],
                    )
                };
                ui.label(RichText::new(hint).size(11.0).color(c_muted()));
                ui.add_space(10.0);
                match self.aligning {
                    Some((id, p)) if id == t.id => {
                        ui.label(
                            RichText::new(crate::i18n::tf(
                                "Нейросеть слушает трек… {p}%",
                                &[("{p}", format!("{}", (p * 100.0) as u32))],
                            ))
                            .size(12.0)
                            .family(fam_semi())
                            .color(c_text()),
                        );
                        let (r, _) = ui.allocate_exact_size(Vec2::new(left_w, 4.0), Sense::hover());
                        ui.painter().rect_filled(r, 2.0, c_text().gamma_multiply(0.1));
                        let mut f = r;
                        f.set_width(r.width() * p.clamp(0.02, 1.0));
                        ui.painter().rect_filled(f, 2.0, c_accent());
                        ui.ctx().request_repaint_after(Duration::from_millis(200));
                    }
                    _ if crate::align::available() => {
                        if l.source != "own" && chip(ui, crate::i18n::t("Подогнать нейросетью"), l.approx) {
                            self.align_after_click = true;
                        }
                    }
                    _ if self.whisper_install.is_some() => {
                        let p = self.whisper_install.unwrap_or(0.0);
                        ui.label(
                            RichText::new(crate::i18n::tf(
                                "Скачиваю нейросеть… {p}%",
                                &[("{p}", format!("{}", (p * 100.0) as u32))],
                            ))
                            .size(12.0)
                            .family(fam_semi())
                            .color(c_text()),
                        );
                        let (r, _) = ui.allocate_exact_size(Vec2::new(left_w, 4.0), Sense::hover());
                        ui.painter().rect_filled(r, 2.0, c_text().gamma_multiply(0.1));
                        let mut f = r;
                        f.set_width(r.width() * p.clamp(0.02, 1.0));
                        ui.painter().rect_filled(f, 2.0, c_accent());
                    }
                    _ if l.approx => {
                        ui.label(
                            RichText::new(crate::i18n::t(
                                "Нейросеть подгонит время строк точно. Работает на твоём компе, бесплатно.",
                            ))
                            .size(11.0)
                            .color(c_muted()),
                        );
                        ui.add_space(4.0);
                        if chip(ui, crate::i18n::t("Установить нейросеть (~150 МБ)"), true) {
                            self.install_after_click = true;
                        }
                    }
                    _ => {}
                }
            }
        });
        if std::mem::take(&mut self.install_after_click) && self.whisper_install.is_none() {
            self.whisper_install = Some(0.0);
            let (tx, ctx) = (self.tx.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let tx2 = tx.clone();
                let ctx2 = ctx.clone();
                let r = crate::align::install(&move |p| {
                    let _ = tx2.send(Msg::WhisperProgress(p));
                    ctx2.request_repaint();
                });
                let _ = tx.send(Msg::WhisperInstalled(r));
                ctx.request_repaint();
            });
        }
        if std::mem::take(&mut self.align_after_click) {
            let ctx = ui.ctx().clone();
            self.start_align(&ctx, t.id, true);
        }

        // ---- right: the lyrics ----
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(right), |ui| match state {
            None => {
                ui.add_space(right.height() * 0.25);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(crate::i18n::t("Ищу текст…")).size(16.0).color(c_muted()));
                });
            }
            Some(None) => {
                ui.add_space(right.height() * 0.22);
                ui.label(
                    RichText::new(crate::i18n::t("Текст не найден"))
                        .size(26.0)
                        .family(fam_display())
                        .color(c_text()),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(crate::i18n::t(
                        "Искал в LRCLIB и Genius. Можно вставить текст самому — обычный или с таймкодами.",
                    ))
                    .color(c_muted()),
                );
                ui.add_space(14.0);
                if chip(ui, crate::i18n::t("Добавить свой текст"), true) {
                    self.lyrics_edit = Some(LyricsEdit { track_id: t.id, text: String::new(), sync: None });
                }
            }
            Some(Some(l)) if !l.synced.is_empty() => {
                match synced_lyrics(ui, &l.synced, pos - self.lyrics_offset) {
                    Some(LyricsClick::Seek(start)) => {
                        actions.push(Action::Seek((start + self.lyrics_offset).max(0.0)));
                    }
                    Some(LyricsClick::Align(start)) => {
                        // "this line is playing now": shift all lyrics to match it
                        self.lyrics_offset = ((pos - start) * 10.0).round() / 10.0;
                        crate::lyrics::save_offset(t.id, self.lyrics_offset);
                        self.status = Some(crate::i18n::tf(
                            "Текст подогнан: сдвиг {s} с",
                            &[("{s}", format!("{:+.1}", self.lyrics_offset))],
                        ));
                    }
                    None => {}
                }
            }
            Some(Some(l)) => {
                plain_lyrics(ui, l.plain.as_deref().unwrap_or_default());
            }
        });
    }

    /// "Own lyrics" window: type/paste lyrics or sync them while the track plays.
    fn ui_lyrics_editor(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(mut ed) = self.lyrics_edit.take() else { return };
        let pos = self.player.as_ref().map_or(0.0, |p| p.position());
        let is_current = self.current_track().is_some_and(|t| t.id == ed.track_id);
        let mut open = true;
        let mut keep = true;
        let mut saved: Option<Option<crate::lyrics::Lyrics>> = None;
        egui::Window::new(crate::i18n::t("Свой текст"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(560.0);
                let mut start_sync: Option<Vec<String>> = None;
                let mut stop_sync = false;
                match &mut ed.sync {
                    None => {
                        ui.label(
                            RichText::new(crate::i18n::t(
                                "Вставь текст песни. Строки с таймкодами [01:23.45] станут синхронными — иначе текст будет обычным.",
                            ))
                            .size(12.0)
                            .color(c_muted()),
                        );
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut ed.text)
                                    .desired_rows(16)
                                    .desired_width(f32::INFINITY)
                                    .hint_text(crate::i18n::t("Текст песни…")),
                            );
                        });
                        ui.add_space(10.0);
                        ui.horizontal_wrapped(|ui| {
                            let has = !ed.text.trim().is_empty();
                            if ui.add_enabled(has, egui::Button::new(crate::i18n::t("Сохранить"))).clicked() {
                                saved = Some(crate::lyrics::save_custom(ed.track_id, ed.text.trim()));
                                keep = false;
                            }
                            let sync_btn = ui
                                .add_enabled(has && is_current, egui::Button::new(crate::i18n::t("Синхронизировать под трек")))
                                .on_hover_text(crate::i18n::t(
                                    "Трек начнётся сначала — нажимай Пробел в начале каждой строки",
                                ));
                            if sync_btn.clicked() {
                                // strip timestamps from the text, skip empty lines
                                let lines: Vec<String> = ed
                                    .text
                                    .lines()
                                    .map(|l| {
                                        let p = crate::lyrics::parse_lrc(l);
                                        p.first().map_or(l.trim().to_string(), |x| x.1.clone())
                                    })
                                    .filter(|l| !l.is_empty())
                                    .collect();
                                start_sync = Some(lines);
                                actions.push(Action::Seek(0.0));
                                if self.player.as_ref().is_some_and(|p| p.is_paused()) {
                                    actions.push(Action::TogglePause);
                                }
                            }
                            if crate::lyrics::load_custom(ed.track_id).is_some()
                                && ui.button(crate::i18n::t("Удалить свой текст")).clicked()
                            {
                                crate::lyrics::delete_custom(ed.track_id);
                                saved = Some(None);
                                keep = false;
                            }
                            if ui.button(crate::i18n::t("Отмена")).clicked() {
                                keep = false;
                            }
                        });
                    }
                    Some((lines, times)) => {
                        let i = times.len();
                        ui.label(
                            RichText::new(crate::i18n::t(
                                "Нажимай Пробел или Enter, когда начинается строка. Backspace — шаг назад.",
                            ))
                            .size(12.0)
                            .color(c_muted()),
                        );
                        ui.add_space(14.0);
                        for k in i.saturating_sub(2)..(i + 3).min(lines.len()) {
                            let (size, col) = if k == i {
                                (22.0, c_accent())
                            } else if k < i {
                                (15.0, c_muted())
                            } else {
                                (15.0, c_text().gamma_multiply(0.7))
                            };
                            let t = times.get(k).map_or(String::new(), |t| format!("{:>5.1}  ", t));
                            ui.label(RichText::new(format!("{t}{}", lines[k])).size(size).family(fam_semi()).color(col));
                        }
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(crate::i18n::tf(
                                "Строка {i} из {n} · {pos}",
                                &[
                                    ("{i}", format!("{}", (i + 1).min(lines.len()))),
                                    ("{n}", format!("{}", lines.len())),
                                    ("{pos}", fmt_time(pos)),
                                ],
                            ))
                            .size(12.0)
                            .color(c_muted()),
                        );
                        ui.add_space(8.0);
                        let (mark, back) = ui.input_mut(|inp| {
                            (
                                inp.consume_key(egui::Modifiers::NONE, egui::Key::Space)
                                    || inp.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                                inp.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
                            )
                        });
                        let mut mark = mark;
                        let mut back = back;
                        let mut restart = false;
                        let mut cancel = false;
                        ui.horizontal(|ui| {
                            mark |= ui.button(crate::i18n::t("Отметить строку")).clicked();
                            back |= ui.add_enabled(i > 0, egui::Button::new(crate::i18n::t("Назад"))).clicked();
                            restart = ui.button(crate::i18n::t("Заново")).clicked();
                            cancel = ui.button(crate::i18n::t("Отмена")).clicked();
                        });
                        if mark && i < lines.len() {
                            times.push(pos);
                        }
                        if back && i > 0 {
                            times.pop();
                            actions.push(Action::Seek((times.last().copied().unwrap_or(0.0)).max(0.0)));
                        }
                        if restart {
                            times.clear();
                            actions.push(Action::Seek(0.0));
                        }
                        if cancel {
                            stop_sync = true;
                        } else if times.len() == lines.len() && !lines.is_empty() {
                            let synced: Vec<(f64, String)> =
                                times.iter().copied().zip(lines.iter().cloned()).collect();
                            let lrc = crate::lyrics::to_lrc(&synced);
                            saved = Some(crate::lyrics::save_custom(ed.track_id, &lrc));
                            keep = false;
                        }
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                    }
                }
                if let Some(lines) = start_sync {
                    ed.sync = Some((lines, Vec::new()));
                }
                if stop_sync {
                    ed.sync = None;
                }
            });
        if let Some(l) = saved {
            if self.lyrics.as_ref().is_some_and(|x| x.0 == ed.track_id) || l.is_some() {
                if l.is_none() {
                    // own lyrics deleted: search the databases again
                    self.lyrics = None;
                } else {
                    self.lyrics = Some((ed.track_id, Some(l)));
                    self.status = Some(crate::i18n::t("Текст сохранён").into());
                }
            }
        }
        if open && keep {
            self.lyrics_edit = Some(ed);
        }
    }

    /// F1: hotkey help.
    fn ui_help(&mut self, ctx: &egui::Context) {
        if !self.show_help {
            return;
        }
        let mut open = true;
        egui::Window::new(crate::i18n::t("Горячие клавиши"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Grid::new("hotkeys")
                    .num_columns(2)
                    .spacing([24.0, 8.0])
                    .show(ui, |ui| {
                        for (k, what) in [
                            ("Space", "Пауза / продолжить"),
                            ("← / →", "Перемотка на 5 с (с Shift — 30 с)"),
                            ("↑ / ↓", "Громкость"),
                            ("N / P", "Следующий / предыдущий трек"),
                            ("L", "Лайк"),
                            ("M", "Без звука"),
                            ("Ctrl+F", "Поиск"),
                            ("F10", "Режим отдыха"),
                            ("F11", "Полный экран"),
                            ("F1", "Эта подсказка"),
                            ("Esc", "Закрыть окна"),
                        ] {
                            ui.label(RichText::new(k).family(fam_semi()).color(c_accent()));
                            ui.label(crate::i18n::t(what));
                            ui.end_row();
                        }
                    });
            });
        if !open {
            self.show_help = false;
        }
    }

    /// "Now playing" card: slides in from the right on track change and leaves after 3.5 s.
    fn ui_toast(&mut self, ctx: &egui::Context) {
        let Some((t, at)) = self.toast.clone() else { return };
        let el = at.elapsed().as_secs_f32();
        let total = 4.2;
        if el > total {
            self.toast = None;
            return;
        }
        ctx.request_repaint();
        let appear = (el / 0.45).min(1.0);
        let vanish = ((total - el) / 0.5).min(1.0);
        let k = ease_out_back(appear).min(1.2) * vanish.clamp(0.0, 1.0);
        let alpha = appear.min(vanish).clamp(0.0, 1.0);
        let screen = ctx.screen_rect();
        let w = 300.0;
        let pos = Pos2::new(
            screen.right() - w - 20.0 + 40.0 * (1.0 - k),
            screen.bottom() - 80.0 - 86.0,
        );
        egui::Area::new(egui::Id::new("now-playing-toast"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_opacity(alpha);
                let p = theme::pal();
                egui::Frame::none()
                    .fill(p.window)
                    .rounding(12.0)
                    .stroke(Stroke::new(1.0_f32, c_accent().gamma_multiply(0.6)))
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.set_width(w - 20.0);
                        ui.horizontal(|ui| {
                            let _ = art(ui, t.artwork("t67x67"), 52.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new(crate::i18n::t("Сейчас играет"))
                                        .size(11.0)
                                        .color(c_accent()),
                                );
                                ui.add(
                                    egui::Label::new(RichText::new(t.title()).size(14.0).family(fam_semi()))
                                        .truncate(),
                                );
                                ui.add(
                                    egui::Label::new(RichText::new(t.artist()).size(12.0).color(c_muted()))
                                        .truncate(),
                                );
                            });
                        });
                    });
            });
    }

    /// Notifications at the top right, in the same style as the "Now playing" card.
    fn ui_notes(&mut self, ctx: &egui::Context) {
        if let Some(text) = self.status.take() {
            let dup = self.notes.last().is_some_and(|(t, at)| *t == text && at.elapsed().as_secs_f32() < 1.0);
            if !dup {
                self.notes.push((text, Instant::now()));
            }
            if self.notes.len() > 4 {
                self.notes.remove(0);
            }
        }
        let total = 3.8;
        self.notes.retain(|(_, at)| at.elapsed().as_secs_f32() < total);
        if self.notes.is_empty() {
            return;
        }
        ctx.request_repaint();
        let screen = ctx.screen_rect();
        let w = 340.0;
        let mut y = screen.top() + 62.0;
        for (i, (text, at)) in self.notes.iter().enumerate().rev() {
            let el = at.elapsed().as_secs_f32();
            let appear = (el / 0.4).min(1.0);
            let vanish = ((total - el) / 0.45).clamp(0.0, 1.0);
            let k = ease_out_back(appear).min(1.15) * vanish;
            let alpha = appear.min(vanish);
            let low = text.to_lowercase();
            let kind = if ["не удал", "ошибк", "помилк", "не вдал", "fail", "error", "не найд", "нет ссылки"]
                .iter()
                .any(|w| low.contains(w))
            {
                2
            } else if ["скач", "завантаж", "download", "mp3"].iter().any(|w| low.contains(w)) {
                1
            } else {
                0
            };
            // "Title: details" → two lines
            let (title, detail) = match text.split_once(": ") {
                Some((a, b)) if a.chars().count() < 40 => (a.to_string(), Some(b.to_string())),
                _ => (text.clone(), None),
            };
            let pos = Pos2::new(screen.right() - w - 20.0 + 50.0 * (1.0 - k), y);
            let resp = egui::Area::new(egui::Id::new(("note", i)))
                .order(egui::Order::Foreground)
                .fixed_pos(pos)
                .interactable(false)
                .show(ctx, |ui| {
                    ui.set_opacity(alpha);
                    let p = theme::pal();
                    let accent = if kind == 2 { Color32::from_rgb(235, 90, 90) } else { c_accent() };
                    egui::Frame::none()
                        .fill(p.window)
                        .rounding(12.0)
                        .stroke(Stroke::new(1.0_f32, accent.gamma_multiply(0.55)))
                        .inner_margin(Margin::symmetric(12.0, 10.0))
                        .show(ui, |ui| {
                            ui.set_width(w - 24.0);
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
                                let c = r.center();
                                let pt = ui.painter();
                                pt.circle_filled(c, 16.0, accent.gamma_multiply(0.2));
                                let st = Stroke::new(2.2_f32, accent);
                                match kind {
                                    1 => {
                                        pt.line_segment([c + Vec2::new(0.0, -7.0), c + Vec2::new(0.0, 4.0)], st);
                                        pt.line_segment([c + Vec2::new(-5.0, -1.0), c + Vec2::new(0.0, 4.5)], st);
                                        pt.line_segment([c + Vec2::new(5.0, -1.0), c + Vec2::new(0.0, 4.5)], st);
                                        pt.line_segment([c + Vec2::new(-6.0, 8.0), c + Vec2::new(6.0, 8.0)], st);
                                    }
                                    2 => {
                                        pt.line_segment([c + Vec2::new(0.0, -7.0), c + Vec2::new(0.0, 2.0)], st);
                                        pt.circle_filled(c + Vec2::new(0.0, 7.0), 1.6, accent);
                                    }
                                    _ => {
                                        // the check mark draws itself as it appears
                                        let d = (el / 0.5).clamp(0.0, 1.0);
                                        let a = c + Vec2::new(-6.0, 0.5);
                                        let b = c + Vec2::new(-1.5, 5.0);
                                        let e = c + Vec2::new(7.0, -5.0);
                                        let t1 = (d * 2.0).min(1.0);
                                        pt.line_segment([a, a + (b - a) * t1], st);
                                        if d > 0.5 {
                                            let t2 = (d - 0.5) * 2.0;
                                            pt.line_segment([b, b + (e - b) * t2], st);
                                        }
                                    }
                                }
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&title).size(13.5).family(fam_semi()).color(c_text()),
                                        )
                                        .truncate(),
                                    );
                                    if let Some(d) = &detail {
                                        ui.add(
                                            egui::Label::new(RichText::new(d).size(12.0).color(c_muted()))
                                                .truncate(),
                                        );
                                    }
                                });
                            });
                        });
                });
            y += (resp.response.rect.height() + 8.0) * alpha.max(0.0);
        }
    }

    fn ui_fx(&mut self, ctx: &egui::Context) {
        if !self.show_fx {
            return;
        }
        let mut open = true;
        egui::Window::new(crate::i18n::t("Эффекты"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-16.0, -76.0))
            .default_width(400.0)
            .vscroll(true)
            .max_height(560.0)
            .show(ctx, |ui| {
                let fx = &mut self.fx;
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    for (name, preset) in Fx::PRESETS {
                        if chip(ui, crate::i18n::t(name), *fx == preset) {
                            *fx = preset;
                        }
                    }
                });
                ui.spacing_mut().slider_width = 230.0;

                let group = |ui: &mut Ui, title: &str| {
                    ui.add_space(12.0);
                    ui.label(RichText::new(title).size(15.0).family(fam_display()).color(c_text()));
                    ui.add_space(2.0);
                };
                let row = |ui: &mut Ui, name: &str, slider: egui::Slider| {
                    ui.label(RichText::new(name).size(13.0).color(c_muted()));
                    ui.add(slider.trailing_fill(true));
                };

                group(ui, crate::i18n::t("Темп и тон"));
                row(
                    ui,
                    crate::i18n::t("Скорость (slowed ↔ nightcore)"),
                    egui::Slider::new(&mut fx.speed, 0.6..=1.4).step_by(0.01).custom_formatter(|v, _| format!("×{v:.2}")),
                );
                row(
                    ui,
                    crate::i18n::t("Тон без изменения скорости"),
                    egui::Slider::new(&mut fx.pitch, -6.0..=6.0)
                        .step_by(0.5)
                        .custom_formatter(|v, _| crate::i18n::tf("{v:+.1} полут.", &[("{v:+.1}", format!("{v:+.1}"))])),
                );

                group(ui, crate::i18n::t("Звук"));
                row(
                    ui,
                    "Reverb",
                    egui::Slider::new(&mut fx.reverb, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
                row(
                    ui,
                    "Bass boost",
                    egui::Slider::new(&mut fx.bass, 0.0..=15.0).custom_formatter(|v, _| crate::i18n::tf("+{v:.0} дБ", &[("{v:.0}", format!("{v:.0}"))])),
                );
                row(
                    ui,
                    crate::i18n::t("Высокие"),
                    egui::Slider::new(&mut fx.treble, -10.0..=10.0).custom_formatter(|v, _| crate::i18n::tf("{v:+.0} дБ", &[("{v:+.0}", format!("{v:+.0}"))])),
                );
                row(
                    ui,
                    crate::i18n::t("Приглушить (под водой)"),
                    egui::Slider::new(&mut fx.muffle, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );

                group(ui, crate::i18n::t("Эквалайзер"));
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = 110.0;
                    for (b, freq) in crate::fx::EQ_BANDS.iter().enumerate() {
                        ui.vertical(|ui| {
                            ui.add(
                                egui::Slider::new(&mut fx.eq[b], -12.0..=12.0)
                                    .vertical()
                                    .show_value(false)
                                    .trailing_fill(true),
                            )
                            .on_hover_text(format!("{:+.1} дБ", fx.eq[b]));
                            let label = if *freq >= 1000 { format!("{}k", freq / 1000) } else { freq.to_string() };
                            ui.label(RichText::new(label).size(10.0).color(c_muted()));
                        });
                    }
                });
                ui.horizontal(|ui| {
                    if chip(ui, crate::i18n::t("Выровнять громкость"), fx.normalize) {
                        fx.normalize = !fx.normalize;
                    }
                    if fx.eq.iter().any(|g| g.abs() > 0.2) && chip(ui, crate::i18n::t("Сбросить EQ"), false) {
                        fx.eq = [0.0; 10];
                    }
                });

                group(ui, crate::i18n::t("Эффекты"));
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                    for (name, flag) in [
                        ("8D", &mut fx.eight_d),
                        ("Lo-fi", &mut fx.lofi),
                        (crate::i18n::t("Телефон"), &mut fx.phone),
                        (crate::i18n::t("Хор"), &mut fx.chorus),
                        (crate::i18n::t("Эхо"), &mut fx.echo),
                        (crate::i18n::t("Минусовка"), &mut fx.karaoke),
                    ] {
                        if chip(ui, name, *flag) {
                            *flag = !*flag;
                        }
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new(crate::i18n::t("8D лучше в наушниках. Минусовка убирает вокал из центра — работает не на всех треках.")).size(11.0).color(c_muted()));

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if fx.is_active() && chip(ui, crate::i18n::t("Сбросить"), false) {
                        *fx = Fx::ORIGINAL;
                    }
                    if let Some(l) = fx.label() {
                        ui.label(RichText::new(l).size(12.0).color(c_accent()));
                    }
                });
            });
        if !open {
            self.show_fx = false;
        }
    }

    fn ui_settings(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut save = false;
        let mut sc_action: Option<Action> = None;
        let mut lang_changed = false;
        let mut theme_changed = false; // theme change: smooth transition
        let mut theme_live = false; // slider/color edits: immediately
        egui::Window::new(crate::i18n::t("Настройки"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .vscroll(true)
            .max_height(640.0)
            .show(ctx, |ui| {
                ui.heading("Discord");
                if ui
                    .checkbox(
                        &mut self.discord_enabled,
                        crate::i18n::t("Показывать «Слушает …» в Discord"),
                    )
                    .changed()
                {
                    save = true;
                }
                ui.add_space(4.0);
                ui.label("Application ID:");
                let r =
                    ui.add(egui::TextEdit::singleline(&mut self.discord_app_id).desired_width(f32::INFINITY));
                if r.lost_focus() {
                    save = true;
                }
                ui.label(
                    RichText::new(crate::i18n::t(
                        "Имя приложения в Discord Developer Portal = текст после «Слушает».",
                    ))
                    .small()
                    .color(c_muted()),
                );
                ui.hyperlink_to(
                    "Discord Developer Portal",
                    "https://discord.com/developers/applications",
                );
                ui.add_space(12.0);
                ui.heading(crate::i18n::t("Загрузки"));
                ui.label(crate::i18n::t("Куда сохранять MP3:"));
                let dir = self.download_path();
                ui.label(RichText::new(dir.display().to_string()).color(c_text()));
                ui.horizontal(|ui| {
                    if ui.button(crate::i18n::t("📁 Выбрать папку…")).clicked() {
                        if let Some(p) = rfd::FileDialog::new().set_directory(&dir).pick_folder() {
                            self.download_dir = p.display().to_string();
                            save = true;
                        }
                    }
                    if ui.button(crate::i18n::t("Открыть")).clicked() {
                        let _ = std::fs::create_dir_all(&dir);
                        downloader::reveal(&dir);
                    }
                    if !self.download_dir.is_empty() && ui.button(crate::i18n::t("По умолчанию")).clicked()
                    {
                        self.download_dir.clear();
                        save = true;
                    }
                });
                ui.add_space(12.0);
                ui.heading("Язык · Мова · Language");
                ui.horizontal_wrapped(|ui| {
                    for (code, name) in [
                        ("auto", "Авто / Auto"),
                        ("ru", "Русский"),
                        ("uk", "Українська"),
                        ("en", "English"),
                    ] {
                        if chip(ui, name, self.lang_code == code) && self.lang_code != code {
                            self.lang_code = code.to_string();
                            crate::i18n::apply_setting(code);
                            lang_changed = true;
                        }
                    }
                });
                ui.add_space(12.0);
                if ui
                    .checkbox(
                        &mut self.animate_taskbar,
                        crate::i18n::t("Анимировать иконку на панели задач, пока играет музыка"),
                    )
                    .changed()
                {
                    lang_changed = true; // just save the settings
                }
                ui.add_space(12.0);
                ui.heading(crate::i18n::t("Оформление"));
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);
                    for (key, name) in theme::PRESETS {
                        if theme_swatch(ui, key, crate::i18n::t(name), &self.theme)
                            && self.theme.preset != key
                        {
                            if key == "custom" && self.theme.preset != "custom" {
                                // "Custom" starts from the current theme, which is handy for tweaking
                                self.theme.stops = ThemeSettings::stops_of(&self.theme.preset);
                                self.theme_sel = 0;
                            }
                            self.theme.preset = key.to_string();
                            theme_changed = true;
                        }
                    }
                });
                // ---- My themes ----
                if !self.saved_themes.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(crate::i18n::t("Мои темы"))
                            .family(fam_semi())
                            .color(c_text()),
                    );
                    ui.label(
                        RichText::new(crate::i18n::t("Клик — применить, правый клик — удалить"))
                            .size(11.0)
                            .color(c_muted()),
                    );
                    let current_code = self.theme.to_code();
                    let mut remove = None;
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);
                        for (i, (name, code)) in self.saved_themes.iter().enumerate() {
                            let Ok(th) = ThemeSettings::from_code(code) else {
                                continue;
                            };
                            let r = swatch(ui, &th.palette(), name, *code == current_code);
                            if r.clicked() {
                                self.theme = th;
                                self.theme_sel = 0;
                                theme_changed = true;
                            }
                            r.context_menu(|ui| {
                                if ui.button(crate::i18n::t("Удалить")).clicked() {
                                    remove = Some(i);
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                    if let Some(i) = remove {
                        self.saved_themes.remove(i);
                        lang_changed = true; // save settings
                    }
                }

                if self.theme.preset == "custom" {
                    ui.add_space(8.0);
                    if theme_editor(ui, &mut self.theme, &mut self.theme_sel) {
                        theme_live = true;
                    }
                }

                // ---- save / share / import ----
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.theme_name_input)
                            .hint_text(crate::i18n::t("Название темы"))
                            .desired_width(160.0),
                    );
                    if chip(ui, crate::i18n::t("💾 Сохранить тему"), false) {
                        let name = if self.theme_name_input.trim().is_empty() {
                            crate::i18n::tf("Тема {}", &[("{}", format!("{}", self.saved_themes.len() + 1))])
                        } else {
                            self.theme_name_input.trim().to_string()
                        };
                        let code = self.theme.to_code();
                        self.saved_themes.retain(|(_, c)| *c != code);
                        self.saved_themes.push((name.clone(), code));
                        self.theme_name_input.clear();
                        self.status = Some(crate::i18n::tf("Тема «{name}» сохранена", &[("{name}", name)]));
                        lang_changed = true;
                    }
                    if chip(ui, crate::i18n::t("📋 Скопировать код"), false) {
                        let code = self.theme.to_code();
                        ui.ctx().copy_text(code.clone());
                        self.status = Some(crate::i18n::tf(
                            "Код темы скопирован: {code}",
                            &[("{code}", code)],
                        ));
                    }
                });
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.theme_code_input)
                            .hint_text(crate::i18n::t("Код темы, например 04G20-81040-…"))
                            .desired_width(260.0),
                    );
                    if chip(ui, crate::i18n::t("Применить код"), false) {
                        match ThemeSettings::from_code(&self.theme_code_input) {
                            Ok(th) => {
                                let code = th.to_code();
                                if !self.saved_themes.iter().any(|(_, c)| *c == code) {
                                    let short: String = code.chars().take(5).collect();
                                    self.saved_themes
                                        .push((crate::i18n::tf("Импорт {}", &[("{}", short)]), code));
                                }
                                self.theme = th;
                                self.theme_sel = 0;
                                self.theme_code_input.clear();
                                self.status = Some(
                                    crate::i18n::t("Тема из кода применена и сохранена в «Мои темы»")
                                        .to_string(),
                                );
                                theme_changed = true;
                                lang_changed = true;
                            }
                            Err(()) => {
                                self.status = Some(
                                    crate::i18n::t("Неверный код темы — проверь, что скопирован целиком")
                                        .to_string(),
                                );
                            }
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::t("Прозрачность окна"));
                    let mut pct = (1.0 - self.theme.opacity) * 100.0;
                    let r = ui.add(
                        egui::Slider::new(&mut pct, 0.0..=70.0)
                            .custom_formatter(|v, _| format!("{v:.0}%"))
                            .trailing_fill(true),
                    );
                    if r.changed() {
                        self.theme.opacity = 1.0 - pct / 100.0;
                        theme_live = true;
                    }
                });
                ui.add_space(12.0);
                ui.heading(crate::i18n::t("Аккаунт SoundCloud"));
                match &self.sc_me {
                    Some(me) => {
                        ui.horizontal(|ui| {
                            avatar_img(ui, me.avatar_url.clone(), &me.username, 32.0);
                            ui.label(RichText::new(&me.username).family(fam_semi()).color(c_text()));
                        });
                        ui.label(
                            RichText::new(crate::i18n::t(
                                "Лайки и история синхронизируются с твоим аккаунтом SoundCloud.",
                            ))
                            .small()
                            .color(c_muted()),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    !self.sc_busy,
                                    egui::Button::new(crate::i18n::t("🔄 Синхронизировать")),
                                )
                                .clicked()
                            {
                                sc_action = Some(Action::ScSync);
                            }
                            if ui.button(crate::i18n::t("Выйти")).clicked() {
                                sc_action = Some(Action::ScLogout);
                            }
                        });
                    }
                    None => {
                        ui.label(
                            RichText::new(crate::i18n::t(
                                "Войди — и лайки, история будут те же, что на сайте и в телефоне.",
                            ))
                            .small()
                            .color(c_muted()),
                        );
                        if ui
                            .add_enabled(
                                !self.sc_busy,
                                egui::Button::new(crate::i18n::t("Войти в SoundCloud")),
                            )
                            .clicked()
                        {
                            sc_action = Some(Action::ScLogin);
                        }
                    }
                }
                ui.add_space(12.0);
                ui.heading(crate::i18n::t("Лайки и история"));
                ui.label(
                    RichText::new(crate::i18n::t(
                        "Копия хранится в «Документы\\SCloud» и не пропадёт при удалении программы. \
                         Чтобы перенести на другой ПК — скопируй туда эту папку.",
                    ))
                    .small()
                    .color(c_muted()),
                );
                if let Some(p) = crate::library::backup_path() {
                    if ui.button(crate::i18n::t("Открыть папку с копией")).clicked() {
                        downloader::reveal(if p.exists() { &p } else { p.parent().unwrap_or(&p) });
                    }
                }
            });
        if let Some(a) = sc_action {
            self.apply(ctx, a);
        }
        if lang_changed {
            self.page_changed = Instant::now(); // gently "redraw" the page in the new language
            self.save_settings();
        }
        if theme_changed || theme_live {
            theme::transition_to(self.theme.palette(), theme_live && !theme_changed);
            apply_visuals(ctx);
            self.save_settings();
        }
        if !open {
            self.show_settings = false;
            save = true;
        }
        if save {
            self.discord_app_id = self.discord_app_id.trim().to_string();
            self.discord.set_app_id(if self.discord_enabled {
                self.discord_app_id.clone()
            } else {
                String::new()
            });
            self.last_presence = None;
            self.save_settings();
        }
    }
}

impl eframe::App for App {
    /// the window is transparent, so we paint the background (solid/gradient) ourselves, honoring theme opacity
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.booted {
            self.booted = true;
            if let Some(tok) = load_sc_token() {
                self.api.set_token(Some(tok));
                self.fetch_me(ctx);
            }
            self.start_genre(ctx, GENRES[0].0);
            if let Some(t) = self.library.history.first() {
                let id = t.id;
                self.start_related(ctx, id);
            }
        }
        self.handle_messages(ctx);
        self.apply_fx(ctx);

        if let Some(t) = self.query_edited_at {
            if t.elapsed() >= Duration::from_millis(300) {
                self.query_edited_at = None;
                self.start_suggest(ctx);
            } else {
                ctx.request_repaint_after(Duration::from_millis(300));
            }
        }

        // the wave loads more by itself when a couple of tracks are left in the queue
        if self.wave.is_some() && !self.wave_loading {
            if let Some(i) = self.current {
                if i + 4 >= self.queue.len() {
                    self.start_wave(ctx, true);
                }
            }
        }

        // time-based sleep timer
        if let (Some(SleepTimer::At(at)), Some(p)) = (self.sleep, &self.player) {
            if Instant::now() >= at {
                if !p.is_paused() {
                    p.toggle_pause();
                }
                self.sleep = None;
                self.status =
                    Some(crate::i18n::t("Таймер сна: музыка на паузе. Спокойной ночи 🌙").to_string());
            } else {
                ctx.request_repaint_after(Duration::from_secs(1));
            }
        }

        // end of track
        if let (Some(p), Some(t)) = (&self.player, self.loaded_at) {
            if t.elapsed() > Duration::from_secs(2) && p.at_end() {
                if self.sleep == Some(SleepTimer::EndOfTrack) {
                    // "end of track" sleep timer: stop playing
                    self.sleep = None;
                    self.loaded_at = None;
                    self.status =
                        Some(crate::i18n::t("Таймер сна: музыка на паузе. Спокойной ночи 🌙").to_string());
                } else if self.repeat == Repeat::One {
                    p.seek(0.0);
                    p.resume();
                    self.loaded_at = Some(Instant::now());
                } else {
                    self.loaded_at = None;
                    if self.wave.is_some() {
                        if let Some(t) = self.current_track().cloned() {
                            self.library.feedback(&t, 0.3); // listened to the end: more like this
                        }
                    }
                    if let Some(n) = self.next_index() {
                        self.play_queue_index(ctx, n);
                    }
                }
            }
        }
        if self.current.is_some() {
            let playing = self.player.as_ref().is_some_and(|p| !p.is_paused()) && self.loaded_at.is_some();
            let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
            // ~30 fps while playing and the window is focused (smooth waveform), otherwise 4 fps
            let ms = if playing && focused { 33 } else { 250 };
            ctx.request_repaint_after(Duration::from_millis(ms));
        }
        self.sync_discord();

        let mut actions = Vec::new();
        self.hotkeys(ctx, &mut actions);
        // playlist list for the right-click menu (the menu is drawn in free functions)
        let names: Vec<(u64, String)> =
            self.library.playlists.iter().map(|p| (p.id, p.name.clone())).collect();
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new("playlist-names"), names);
            d.insert_temp(egui::Id::new("menu-playlist"), None::<u64>);
        });
        let paused_now = self.player.as_ref().map_or(true, |p| p.is_paused());
        for cmd in self.media.poll() {
            actions.push(match cmd {
                crate::media::Cmd::Play if paused_now => Action::TogglePause,
                crate::media::Cmd::Pause if !paused_now => Action::TogglePause,
                crate::media::Cmd::Play | crate::media::Cmd::Pause => continue,
                crate::media::Cmd::Toggle => Action::TogglePause,
                crate::media::Cmd::Next => Action::Next,
                crate::media::Cmd::Prev => Action::Prev,
                crate::media::Cmd::SeekTo(s) => Action::Seek(s),
                crate::media::Cmd::SeekBy(d) => Action::SeekBy(d),
            });
        }
        self.sync_media(ctx);
        if self.show_lyrics || self.relax {
            if let Some(t) = self.current_track().cloned() {
                if self.lyrics.as_ref().map(|x| x.0) != Some(t.id) {
                    self.lyrics = Some((t.id, None));
                    self.lyrics_offset = crate::lyrics::load_offset(t.id);
                    let (tx, ctx2) = (self.tx.clone(), ctx.clone());
                    std::thread::spawn(move || {
                        let l = crate::lyrics::fetch(&t);
                        let _ = tx.send(Msg::Lyrics(t.id, l));
                        ctx2.request_repaint();
                    });
                }
            }
        }

        if theme::tick() {
            apply_visuals(ctx);
            ctx.request_repaint();
        }
        paint_background(ctx);
        self.animate_window_icon(ctx, frame);
        // full screen may have been toggled with the window button: remember it
        if !self.relax && self.started.elapsed().as_secs_f32() > 2.0 {
            let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            if full != self.fullscreen {
                self.fullscreen = full;
                self.save_settings();
            }
        }
        if self.page != self.shown_page {
            self.shown_page = self.page;
            self.page_changed = Instant::now();
            // artwork from the previous page is no longer needed: free the memory
            ctx.forget_all_images();
        }

        if self.relax {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(Color32::TRANSPARENT))
                .show(ctx, |ui| self.ui_relax(ui, &mut actions));
        } else {
            egui::TopBottomPanel::top("topbar")
                .frame(
                    egui::Frame::none()
                        .fill(c_bar())
                        .inner_margin(Margin::symmetric(20.0, 10.0)),
                )
                .show(ctx, |ui| self.ui_topbar(ui, &mut actions));
            egui::TopBottomPanel::bottom("player")
                .exact_height(60.0)
                .frame(
                    egui::Frame::none()
                        .fill(c_bar())
                        .inner_margin(Margin::symmetric(20.0, 6.0)),
                )
                .show(ctx, |ui| self.ui_player(ui, &mut actions));
            if self.page == Page::Home {
                egui::SidePanel::right("sidebar")
                    .exact_width(300.0)
                    .resizable(false)
                    .frame(
                        egui::Frame::none()
                            .fill(Color32::TRANSPARENT)
                            .inner_margin(Margin::symmetric(16.0, 0.0)),
                    )
                    .show(ctx, |ui| self.ui_sidebar(ui, &mut actions));
            }
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(Color32::TRANSPARENT)
                        .inner_margin(Margin::symmetric(32.0, 0.0)),
                )
                .show(ctx, |ui| {
                    // the new page fades in and floats up slightly
                    let t = (self.page_changed.elapsed().as_secs_f32() / 0.28).min(1.0);
                    let e = 1.0 - (1.0 - t).powi(3);
                    if t < 1.0 {
                        ui.ctx().request_repaint();
                    }
                    ui.set_opacity(e);
                    ui.add_space(18.0 * (1.0 - e));
                    if self.show_lyrics {
                        self.ui_lyrics(ui, &mut actions);
                        return;
                    }
                    match self.page {
                        Page::Home => self.ui_home(ui, &mut actions),
                        Page::Search => self.ui_search(ui, &mut actions),
                        Page::Library => self.ui_library(ui, &mut actions),
                        Page::Stats => self.ui_stats(ui, &mut actions),
                    }
                });
        }

        self.ui_suggestions(ctx, &mut actions);
        self.ui_fx(ctx);
        self.ui_queue(ctx, &mut actions);
        self.ui_help(ctx);
        self.ui_lyrics_editor(ctx, &mut actions);
        self.ui_toast(ctx);
        self.ui_notes(ctx);
        // a pick in a track's right-click menu
        let menu_id = egui::Id::new("track-menu-act");
        if let Some((t, act)) = ctx.data(|d| d.get_temp::<(Track, MenuAct)>(menu_id)) {
            ctx.data_mut(|d| d.remove::<(Track, MenuAct)>(menu_id));
            actions.push(match act {
                MenuAct::PlayNext => Action::PlayNext(t),
                MenuAct::Enqueue => Action::Enqueue(t),
                MenuAct::Radio => Action::Radio(t),
                MenuAct::Like => Action::Like(t),
                MenuAct::Download => Action::Download(t),
                MenuAct::Copy => Action::CopyLink(t),
                MenuAct::AddTo(pid) => Action::AddToPlaylist(pid, t),
                MenuAct::NewPlaylist => Action::NewPlaylistWith(t),
                MenuAct::RemoveFromPlaylist(pid) => Action::RemoveFromPlaylist(pid, t.id),
                MenuAct::Open => {
                    if let Some(u) = &t.permalink_url {
                        ctx.open_url(egui::OpenUrl::new_tab(u));
                    }
                    Action::Go(self.page)
                }
            });
        }
        self.ui_settings(ctx);
        resize_edges(ctx);

        for a in actions {
            self.apply(ctx, a);
        }
        if self.status.is_some() {
            ctx.request_repaint(); // the notification will show on the next frame
        }
    }
}

// ================= widgets =================

/// Fonts live next to the app (app\\fonts) or in the setup cache.
/// If the files are missing, egui's built-in fonts are used and everything keeps working.
fn load_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.join("fonts"));
        }
    }
    if let Some(d) = dirs::data_local_dir() {
        dirs.push(d.join("SCloud").join("deps").join("fonts"));
    }
    let read = |name: &str| dirs.iter().find_map(|d| std::fs::read(d.join(name)).ok());

    let mut fonts = FontDefinitions::default();
    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut add = |keys: &[&str]| -> Vec<String> {
        let mut out = Vec::new();
        for k in keys {
            if let Some(bytes) = read(&format!("{k}.ttf")) {
                fonts.font_data.insert(k.to_string(), FontData::from_owned(bytes));
                out.push(k.to_string());
            }
        }
        out
    };
    let body = add(&["onest-latin-400", "onest-cyrillic-400"]);
    let semi = add(&["onest-latin-600", "onest-cyrillic-600"]);
    let display = add(&["unbounded-latin-700", "unbounded-cyrillic-700"]);

    let chain = |first: &[Vec<String>]| -> Vec<String> {
        let mut v: Vec<String> = first.iter().flatten().cloned().collect();
        v.extend(defaults.iter().cloned()); // emoji and fallback glyphs
        v
    };
    fonts
        .families
        .insert(FontFamily::Proportional, chain(&[body.clone()]));
    fonts
        .families
        .insert(fam_semi(), chain(&[semi.clone(), body.clone()]));
    fonts
        .families
        .insert(fam_display(), chain(&[display, semi, body]));
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    load_fonts(ctx);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        // scrollbar: thin, translucent, visible only on hover/scroll
        let mut sc = egui::style::ScrollStyle::floating();
        sc.bar_width = 8.0;
        sc.floating_width = 3.0;
        sc.foreground_color = true;
        sc.dormant_background_opacity = 0.0;
        sc.active_background_opacity = 0.0;
        sc.interact_background_opacity = 0.15;
        sc.dormant_handle_opacity = 0.0;
        sc.active_handle_opacity = 0.35;
        sc.interact_handle_opacity = 0.6;
        s.spacing.scroll = sc;
        s.text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        s.text_styles
            .insert(egui::TextStyle::Button, egui::FontId::new(14.0, fam_semi()));
        s.text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::new(18.0, fam_display()));
    });
}

/// egui widget colors for the current theme.
fn apply_visuals(ctx: &egui::Context) {
    let p = theme::pal();
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = p.window;
    v.extreme_bg_color = p.field;
    v.faint_bg_color = p.hover;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0_f32, p.text);
    v.hyperlink_color = p.accent;
    v.override_text_color = None;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.inactive.weak_bg_fill = p.hover;
    v.widgets.inactive.bg_fill = p.text.gamma_multiply(0.3);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.hovered.weak_bg_fill = p.text.gamma_multiply(0.14);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.active.weak_bg_fill = p.text.gamma_multiply(0.2);
    v.window_rounding = 10.0.into();
    v.window_stroke = Stroke::new(1.0_f32, p.text.gamma_multiply(0.12));
    ctx.set_visuals(v);
}

/// Window background: solid or gradient, honoring transparency.
fn paint_background(ctx: &egui::Context) {
    let p = theme::pal();
    let r = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    let a = p.opacity;
    if p.grad.iter().all(|c| *c == p.grad[0]) {
        painter.rect_filled(r, 0.0, p.grad[0].gamma_multiply(a));
        return;
    }
    // angled gradient (like CSS: 180° = top to bottom), 16×16 grid
    let ang = p.angle.to_radians();
    let dir = Vec2::new(ang.sin(), -ang.cos());
    let half = (r.width() * dir.x.abs() + r.height() * dir.y.abs()) / 2.0;
    let c = r.center();
    let n = 16u32;
    let mut mesh = egui::Mesh::default();
    for iy in 0..=n {
        for ix in 0..=n {
            let pt = Pos2::new(
                r.left() + r.width() * ix as f32 / n as f32,
                r.top() + r.height() * iy as f32 / n as f32,
            );
            let t = 0.5 + (pt - c).dot(dir) / (2.0 * half.max(1.0));
            mesh.colored_vertex(pt, theme::grad_at(&p.grad, t).gamma_multiply(a));
        }
    }
    let w = n + 1;
    for iy in 0..n {
        for ix in 0..n {
            let i = iy * w + ix;
            mesh.add_triangle(i, i + 1, i + w + 1);
            mesh.add_triangle(i, i + w + 1, i + w);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn nav_tab(ui: &mut Ui, text: &str, selected: bool) -> bool {
    let color = if selected { c_text() } else { c_muted() };
    let r = ui.add(
        egui::Label::new(RichText::new(text).size(15.0).family(fam_semi()).color(color))
            .sense(Sense::click()),
    );
    // the underline under a tab grows from the center; a subtle hint on hover
    let t = anim(ui, r.id.with("sel"), selected);
    let h = anim(ui, r.id.with("hov"), r.hovered() && !selected);
    let k = t.max(h * 0.35);
    if k > 0.01 {
        let y = r.rect.bottom() + 10.0;
        let half = r.rect.width() / 2.0 * k;
        let cx = r.rect.center().x;
        ui.painter().line_segment(
            [Pos2::new(cx - half, y), Pos2::new(cx + half, y)],
            Stroke::new(2.0_f32, c_text().gamma_multiply(0.4 + 0.6 * t)),
        );
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

fn chip(ui: &mut Ui, text: &str, selected: bool) -> bool {
    let font = |k: f32| egui::FontId::new(13.0 * k, fam_semi());
    let base = ui.painter().layout_no_wrap(text.to_string(), font(1.0), c_text());
    let size = Vec2::new(base.size().x + 28.0, 30.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let h = anim(ui, resp.id, resp.hovered());
    let sel = anim(ui, resp.id.with("sel"), selected);
    let k = 1.0 + 0.08 * ease_out_back(h) - 0.06 * press_t(ui, &resp);
    let r = Rect::from_center_size(rect.center(), rect.size() * k);
    // selected = filled with the text color, others = outline; a light fill on hover
    let fill = mix(c_hover().gamma_multiply(h), c_text(), sel);
    let stroke = Stroke::new(1.0_f32, c_text().gamma_multiply(0.25 * (1.0 - sel) + 0.15 * h));
    let color = mix(c_text(), INK, sel);
    let p = ui.painter();
    p.rect(r, r.height() / 2.0, fill, stroke);
    let galley = p.layout_no_wrap(text.to_string(), font(k), color);
    p.galley(r.center() - galley.size() / 2.0, galley, color);
    shine(ui, r, resp.id, resp.hovered());
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// Change the icon frame: on Windows straight to the taskbar, on other OSes through egui.
fn set_icon(
    ctx: &egui::Context,
    frame: &eframe::Frame,
    frames: &[std::sync::Arc<egui::IconData>],
    idx: usize,
) {
    #[cfg(windows)]
    {
        let _ = (ctx, frames);
        crate::logo::set_taskbar_icon(frame, idx);
    }
    #[cfg(not(windows))]
    {
        let _ = frame;
        if let Some(f) = frames.get(idx) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(f.clone())));
        }
    }
}

/// Window buttons in the header: minimize, maximize, close (there is no system title bar).
fn window_buttons(ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    // "maximize" opens full screen, over the taskbar
    let max = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    ui.spacing_mut().item_spacing.x = 2.0;
    // right to left: close, maximize, minimize
    for kind in 0..3 {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(40.0, 30.0), Sense::click());
        let h = anim(ui, resp.id, resp.hovered());
        let bg = if kind == 0 {
            Color32::from_rgb(232, 17, 35)
        } else {
            c_text().gamma_multiply(0.12)
        };
        let p = ui.painter();
        p.rect_filled(rect, 6.0, bg.gamma_multiply(h));
        let c = rect.center();
        let col = if kind == 0 {
            mix(c_muted(), Color32::WHITE, h)
        } else {
            mix(c_muted(), c_text(), h)
        };
        let stroke = Stroke::new(1.3_f32, col);
        let s = 5.0 * (1.0 + 0.1 * ease_out_back(h));
        match kind {
            0 => {
                p.line_segment([c + Vec2::new(-s, -s), c + Vec2::new(s, s)], stroke);
                p.line_segment([c + Vec2::new(-s, s), c + Vec2::new(s, -s)], stroke);
            }
            1 if max => {
                p.rect_stroke(
                    Rect::from_center_size(c + Vec2::new(-1.5, 1.5), Vec2::splat(s * 1.6)),
                    1.5,
                    stroke,
                );
                p.line_segment([c + Vec2::new(-s * 0.3, -s), c + Vec2::new(s, -s)], stroke);
                p.line_segment([c + Vec2::new(s, -s), c + Vec2::new(s, s * 0.3)], stroke);
            }
            1 => {
                p.rect_stroke(Rect::from_center_size(c, Vec2::splat(s * 2.0)), 1.5, stroke);
            }
            _ => {
                p.line_segment([c + Vec2::new(-s, 0.0), c + Vec2::new(s, 0.0)], stroke);
            }
        }
        if resp.clicked() {
            ctx.send_viewport_cmd(match kind {
                0 => egui::ViewportCommand::Close,
                1 => egui::ViewportCommand::Fullscreen(!max),
                _ => egui::ViewportCommand::Minimized(true),
            });
        }
    }
}

/// Resizing by the window edges (Windows doesn't provide it for a window without a system frame).
fn resize_edges(ctx: &egui::Context) {
    use egui::viewport::ResizeDirection as D;
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)) {
        return;
    }
    let r = ctx.screen_rect();
    let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let m = 6.0;
    let (l, rt, t, b) = (
        p.x <= r.left() + m,
        p.x >= r.right() - m,
        p.y <= r.top() + m,
        p.y >= r.bottom() - m,
    );
    let dir = match (l, rt, t, b) {
        (true, _, true, _) => D::NorthWest,
        (_, true, true, _) => D::NorthEast,
        (true, _, _, true) => D::SouthWest,
        (_, true, _, true) => D::SouthEast,
        (true, _, _, _) => D::West,
        (_, true, _, _) => D::East,
        (_, _, true, _) => D::North,
        (_, _, _, true) => D::South,
        _ => return,
    };
    ctx.set_cursor_icon(match dir {
        D::NorthWest => egui::CursorIcon::ResizeNorthWest,
        D::NorthEast => egui::CursorIcon::ResizeNorthEast,
        D::SouthWest => egui::CursorIcon::ResizeSouthWest,
        D::SouthEast => egui::CursorIcon::ResizeSouthEast,
        D::West => egui::CursorIcon::ResizeWest,
        D::East => egui::CursorIcon::ResizeEast,
        D::North => egui::CursorIcon::ResizeNorth,
        D::South => egui::CursorIcon::ResizeSouth,
    });
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
    }
}

/// Synced lyrics: the current line is bigger and "fills" with color as it is sung,
/// the lyrics scroll smoothly with the song. Returns a time if a line was clicked.
/// Plain lyrics without timestamps: large, wrapped, scrollable.
fn plain_lyrics(ui: &mut Ui, text: &str) {
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - 24.0).min(760.0));
            ui.add_space(8.0);
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    ui.add_space(14.0);
                    continue;
                }
                // [Chorus], [Verse 1]: subheadings
                let header = line.starts_with('[') && line.ends_with(']');
                let rt = if header {
                    RichText::new(line.trim_matches(['[', ']'])).size(13.0).family(fam_semi()).color(c_accent())
                } else {
                    RichText::new(line).size(24.0).family(fam_semi()).color(c_text().gamma_multiply(0.9))
                };
                ui.add(egui::Label::new(rt).wrap());
                ui.add_space(if header { 2.0 } else { 6.0 });
            }
            ui.add_space(80.0);
        });
    });
}

/// Synced lyrics: the current line sits in the upper third, letters fill with the accent along the line,
/// long lines wrap, the mouse wheel scrolls (after 3 s it returns to the current line).
enum LyricsClick {
    /// LMB: seek to the line
    Seek(f64),
    /// RMB: this line is playing now, so align the timing
    Align(f64),
}

fn synced_lyrics(ui: &mut Ui, lines: &[(f64, String)], pos: f64) -> Option<LyricsClick> {
    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
    let ctx = ui.ctx().clone();
    ctx.request_repaint_after(std::time::Duration::from_millis(33));
    let now = pos + 0.15;
    let cur = lines.iter().rposition(|(t, _)| *t <= now);
    let wrap_w = (rect.width() - 40.0).clamp(200.0, 900.0);
    let size = 28.0;
    let gap = 22.0;
    let font = egui::FontId::new(size, fam_semi());

    // layout of all lines (one color, just to get the heights)
    let display: Vec<String> = lines
        .iter()
        .map(|(_, t)| if t.trim().is_empty() { "• • •".to_string() } else { t.clone() })
        .collect();
    let heights: Vec<f32> = display
        .iter()
        .map(|t| {
            ctx.fonts(|f| f.layout(t.clone(), font.clone(), Color32::WHITE, wrap_w))
                .size()
                .y
        })
        .collect();
    let mut tops = Vec::with_capacity(lines.len());
    let mut y = 0.0;
    for h in &heights {
        tops.push(y);
        y += h + gap;
    }
    let total = y;

    // wheel scrolling: a manual offset that fades out after 3 s
    let mid = resp.id.with("manual");
    let (mut manual, mut last): (f32, f64) = ctx.data(|d| d.get_temp(mid)).unwrap_or((0.0, 0.0));
    let t_now = ui.input(|i| i.time);
    if resp.hovered() {
        let dy = ui.input(|i| i.smooth_scroll_delta.y);
        if dy != 0.0 {
            manual -= dy;
            last = t_now;
        }
    }
    if t_now - last > 3.0 {
        manual *= 0.85;
        if manual.abs() < 0.5 {
            manual = 0.0;
        }
    }
    let target = cur.map_or(-60.0, |c| tops[c] + heights[c] / 2.0);
    manual = manual.clamp(-target - 200.0, (total - target).max(0.0));
    ctx.data_mut(|d| d.insert_temp(mid, (manual, last)));
    let offset = ctx.animate_value_with_time(resp.id.with("scroll"), target, 0.6) + manual;
    let anchor = rect.top() + rect.height() * 0.30;

    let painter = ui.painter().with_clip_rect(rect);
    let hover = resp.hover_pos();
    let mut clicked = None;
    for (i, (start, _)) in lines.iter().enumerate() {
        let top = anchor + tops[i] - offset;
        let h = heights[i];
        if top + h < rect.top() - 10.0 || top > rect.bottom() + 10.0 {
            continue;
        }
        let is_cur = Some(i) == cur;
        let k = ctx.animate_bool_with_time(resp.id.with(("line", i)), is_cur, 0.35);
        let past = cur.is_some_and(|c| i < c);
        let dist = cur.map_or(3.0, |c| (i as f32 - c as f32).abs());
        // smooth fade towards the edges of the area
        let edge = ((top + h / 2.0 - rect.top()) / 70.0)
            .min((rect.bottom() - top - h / 2.0) / 70.0)
            .clamp(0.0, 1.0);
        let fade = (1.0 - dist * 0.12).clamp(0.35, 1.0) * edge;

        let row = Rect::from_min_max(
            Pos2::new(rect.left(), top - gap / 2.0),
            Pos2::new(rect.right(), top + h + gap / 2.0),
        );
        if hover.is_some_and(|p| row.contains(p)) {
            painter.rect_filled(row.shrink2(Vec2::new(0.0, 3.0)), 12.0, c_text().gamma_multiply(0.05));
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                clicked = Some(LyricsClick::Seek(*start));
                ctx.data_mut(|d| d.insert_temp(mid, (0.0_f32, 0.0_f64)));
            } else if resp.secondary_clicked() {
                clicked = Some(LyricsClick::Align(*start));
                ctx.data_mut(|d| d.insert_temp(mid, (0.0_f32, 0.0_f64)));
            }
        }

        let next = lines.get(i + 1).map_or(start + 5.0, |l| l.0);
        let progress = if is_cur {
            ((now - start) / (next - start).max(0.3)).clamp(0.0, 1.0) as f32
        } else if past {
            1.0
        } else {
            0.0
        };
        let base = if past { c_text().gamma_multiply(0.42) } else { c_text().gamma_multiply(0.3) };
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = wrap_w;
        if k > 0.01 {
            // per-character fill of the current line
            let chars: Vec<char> = display[i].chars().collect();
            let n = chars.len().max(1) as f32;
            for (ci, ch) in chars.iter().enumerate() {
                let fill = ((progress * n) - ci as f32).clamp(0.0, 1.0);
                let unsung = mix(base, c_text().gamma_multiply(0.55), k);
                let color = mix(unsung, mix(c_text(), c_accent(), 0.65), fill * k);
                job.append(
                    &ch.to_string(),
                    0.0,
                    egui::TextFormat { font_id: font.clone(), color: color.gamma_multiply(fade), ..Default::default() },
                );
            }
        } else {
            job.append(
                &display[i],
                0.0,
                egui::TextFormat { font_id: font.clone(), color: base.gamma_multiply(fade), ..Default::default() },
            );
        }
        let galley = ctx.fonts(|f| f.layout_job(job));
        let x = rect.left() + 18.0 + 6.0 * k;
        if k > 0.01 {
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(rect.left() + 2.0, top + 4.0), Vec2::new(4.0, h - 8.0)),
                2.0,
                c_accent().gamma_multiply(k * fade),
            );
        }
        painter.galley(Pos2::new(x, top), galley, c_text());
    }
    clicked
}

/// Right-click menu on a track (on the artwork or title).
fn track_menu(r: &Response, t: &Track) {
    r.context_menu(|ui| {
        ui.set_min_width(220.0);
        let pick = |ui: &mut Ui, act: MenuAct| {
            ui.ctx()
                .data_mut(|d| d.insert_temp(egui::Id::new("track-menu-act"), (t.clone(), act)));
            ui.close_menu();
        };
        let items = [
            ("▶  ", "Играть следующим", MenuAct::PlayNext),
            ("➕  ", "Добавить в очередь", MenuAct::Enqueue),
            ("📻  ", "Радио по треку", MenuAct::Radio),
            ("♥  ", "Лайк / убрать лайк", MenuAct::Like),
        ];
        for (icon, label, act) in items {
            if ui.button(format!("{icon}{}", crate::i18n::t(label))).clicked() {
                pick(ui, act);
            }
        }
        let names: Vec<(u64, String)> = ui
            .ctx()
            .data(|d| d.get_temp(egui::Id::new("playlist-names")))
            .unwrap_or_default();
        ui.menu_button(format!("☰  {}", crate::i18n::t("Добавить в плейлист")), |ui| {
            ui.set_min_width(200.0);
            if ui.button(format!("＋  {}", crate::i18n::t("Новый плейлист"))).clicked() {
                pick(ui, MenuAct::NewPlaylist);
            }
            if !names.is_empty() {
                ui.separator();
            }
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                for (id, name) in &names {
                    if ui.button(name).clicked() {
                        pick(ui, MenuAct::AddTo(*id));
                    }
                }
            });
        });
        let open_pl: Option<u64> = ui.ctx().data(|d| d.get_temp(egui::Id::new("menu-playlist"))).flatten();
        if let Some(pid) = open_pl {
            if ui.button(format!("✕  {}", crate::i18n::t("Убрать из плейлиста"))).clicked() {
                pick(ui, MenuAct::RemoveFromPlaylist(pid));
            }
        }
        ui.separator();
        for (icon, label, act) in [
            ("⬇  ", "Скачать MP3", MenuAct::Download),
            ("🔗  ", "Скопировать ссылку", MenuAct::Copy),
            ("↗  ", "Открыть на soundcloud.com", MenuAct::Open),
        ] {
            if ui.button(format!("{icon}{}", crate::i18n::t(label))).clicked() {
                pick(ui, act);
            }
        }
    });
}

/// "Spring": 0..1 with a slight overshoot at the end (like the buttons in the reference video).
fn ease_out_back(t: f32) -> f32 {
    let (c1, c3) = (1.70158, 2.70158);
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

/// Shine: on hover a diagonal light streak sweeps across the element once.
fn shine(ui: &Ui, rect: Rect, id: egui::Id, hovered: bool) {
    let ctx = ui.ctx();
    let key = id.with("shine");
    let now = ctx.input(|i| i.time);
    let start: Option<f64> = ctx.data(|d| d.get_temp(key));
    match (hovered, start) {
        (true, None) => ctx.data_mut(|d| d.insert_temp(key, now)),
        (false, Some(_)) => ctx.data_mut(|d| d.remove::<f64>(key)),
        _ => {}
    }
    let Some(st) = start else { return };
    let p = ((now - st) / 0.6) as f32;
    if !(0.0..1.0).contains(&p) {
        return;
    }
    ctx.request_repaint();
    let e = 1.0 - (1.0 - p).powi(3);
    let w = (rect.height() * 0.35).clamp(10.0, 46.0);
    let skew = rect.height() * 0.6;
    let x = rect.left() - w - skew + (rect.width() + 2.0 * w + skew) * e;
    let pts = vec![
        Pos2::new(x, rect.bottom()),
        Pos2::new(x + skew, rect.top()),
        Pos2::new(x + skew + w, rect.top()),
        Pos2::new(x + w, rect.bottom()),
    ];
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let alpha = (1.0 - (p - 0.5).abs() * 2.0).max(0.0) * 0.9 + 0.1;
    painter.add(egui::Shape::convex_polygon(
        pts,
        Color32::from_white_alpha((44.0 * alpha) as u8),
        Stroke::NONE,
    ));
}

/// Press: a short "push-in".
fn press_t(ui: &Ui, resp: &Response) -> f32 {
    ui.ctx()
        .animate_bool_with_time(resp.id.with("press"), resp.is_pointer_button_down_on(), 0.08)
}

/// Smooth 0..1 value for hover/selection (egui animates it over time).
fn anim(ui: &Ui, id: egui::Id, on: bool) -> f32 {
    ui.ctx().animate_bool_with_time(id, on, 0.14)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// Icon button: brightens smoothly and grows a bit on hover.
fn icon_resp(ui: &mut Ui, icon: &str, size: f32, color: Color32) -> Response {
    let box_size = Vec2::splat(size * 1.35);
    let (rect, resp) = ui.allocate_exact_size(box_size, Sense::click());
    let t = anim(ui, resp.id, resp.hovered());
    let spring = ease_out_back(t);
    let c = mix(color, c_text(), t * 0.6);
    let scale = 1.0 + 0.18 * spring - 0.12 * press_t(ui, &resp);
    // the icon tilts slightly, like in the reference
    let angle = -0.18 * spring;
    let galley = ui
        .painter()
        .layout_no_wrap(icon.to_string(), egui::FontId::proportional(size * scale), c);
    let half = galley.size() / 2.0;
    let (sin, cos) = angle.sin_cos();
    let rotated_half = Vec2::new(half.x * cos - half.y * sin, half.x * sin + half.y * cos);
    let pos = rect.center() - rotated_half;
    ui.painter()
        .add(egui::epaint::TextShape::new(pos, galley, c).with_angle(angle));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn icon_btn(ui: &mut Ui, icon: &str, size: f32, color: Color32) -> bool {
    icon_resp(ui, icon, size, color).clicked()
}

/// Sleep timer button: a drawn crescent (the 🌙 emoji looks off in our fonts).
/// On hover it springs and tilts; while the timer is on, little stars twinkle next to it.
fn moon_button(ui: &mut Ui, size: f32, color: Color32, active: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size * 1.35), Sense::click());
    let t = anim(ui, resp.id, resp.hovered());
    let spring = ease_out_back(t);
    let c = mix(color, c_text(), t * 0.6);
    let scale = 1.0 + 0.18 * spring - 0.12 * press_t(ui, &resp);
    let angle = -0.35 * spring;
    let r1 = size * 0.46 * scale;
    let center = rect.center();
    // outer circle minus a shifted inner one = crescent
    let c1 = Vec2::ZERO;
    let c2 = Vec2::new(0.42, -0.30) * r1;
    let r2 = r1 * 0.80;
    let rot = |v: Vec2| {
        let (sn, cs) = angle.sin_cos();
        center + Vec2::new(v.x * cs - v.y * sn, v.x * sn + v.y * cs)
    };
    let d = (c2 - c1).length();
    let a = (r1 * r1 - r2 * r2 + d * d) / (2.0 * d);
    let h = (r1 * r1 - a * a).max(0.0).sqrt();
    let u = (c2 - c1) / d;
    let perp = Vec2::new(-u.y, u.x);
    let pa = c1 + u * a + perp * h;
    let pb = c1 + u * a - perp * h;
    let ang = |p: Vec2, c: Vec2| (p - c).y.atan2((p - c).x);
    // arc from A to B on the side whose midpoint satisfies the condition
    let arc = |c: Vec2, r: f32, want: &dyn Fn(Vec2) -> bool| -> Vec<Vec2> {
        let a0 = ang(pa, c);
        let mut da = ang(pb, c) - a0;
        while da <= -std::f32::consts::PI {
            da += std::f32::consts::TAU;
        }
        while da > std::f32::consts::PI {
            da -= std::f32::consts::TAU;
        }
        let mid = a0 + da / 2.0;
        if !want(c + Vec2::angled(mid) * r) {
            da -= std::f32::consts::TAU * da.signum();
        }
        (0..=24)
            .map(|i| c + Vec2::angled(a0 + da * i as f32 / 24.0) * r)
            .collect()
    };
    let outer = arc(c1, r1, &|p| (p - c2).length() > r2);
    let inner = arc(c2, r2, &|p| (p - c1).length() < r1);
    let mut mesh = egui::Mesh::default();
    for (o, i) in outer.iter().zip(&inner) {
        mesh.colored_vertex(rot(*o), c);
        mesh.colored_vertex(rot(*i), c);
    }
    for k in 0..outer.len() as u32 - 1 {
        let v = k * 2;
        mesh.add_triangle(v, v + 1, v + 2);
        mesh.add_triangle(v + 1, v + 3, v + 2);
    }
    let painter = ui.painter();
    painter.add(mesh);
    // a thin outline smooths the edges
    let mut outline: Vec<Pos2> = outer.iter().map(|v| rot(*v)).collect();
    outline.extend(inner.iter().rev().map(|v| rot(*v)));
    painter.add(egui::Shape::closed_line(outline, Stroke::new(0.8_f32, c)));
    if active || t > 0.01 {
        let time = ui.input(|i| i.time) as f32;
        let tw = |k: f32| 0.55 + 0.45 * (time * 2.2 + k).sin();
        let amount = if active { 1.0 } else { t };
        for (off, s, k) in [(Vec2::new(0.62, -0.55), 2.2, 0.0), (Vec2::new(0.95, -0.05), 1.5, 1.7)] {
            let p = rot(off * r1);
            let col = c.gamma_multiply(tw(k) * amount);
            let s = s * (0.6 + 0.4 * tw(k)) * scale;
            painter.line_segment([p - Vec2::new(s, 0.0), p + Vec2::new(s, 0.0)], Stroke::new(1.0_f32, col));
            painter.line_segment([p - Vec2::new(0.0, s), p + Vec2::new(0.0, s)], Stroke::new(1.0_f32, col));
        }
        if active {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The same "living" icon as a widget: `ui.add(AnimIcon("⬇", 17.0, color))`.
struct AnimIcon<'a>(&'a str, f32, Color32);

impl egui::Widget for AnimIcon<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        icon_resp(ui, self.0, self.1, self.2)
    }
}

/// Row background that fades in on hover. Drawn "under" content that is already painted.
fn row_background(ui: &mut Ui, slot: egui::layers::ShapeIdx, rect: Rect, id: egui::Id, base: Color32) {
    let t = anim(ui, id, ui.rect_contains_pointer(rect));
    let hover = c_hover();
    let fill = if base == Color32::TRANSPARENT {
        hover.gamma_multiply(t)
    } else {
        mix(base, hover, t * 0.5)
    };
    ui.painter().set(slot, egui::Shape::rect_filled(rect, 6.0, fill));
}

fn section_title(ui: &mut Ui, text: &str) {
    ui.add_space(22.0);
    ui.label(
        RichText::new(text)
            .size(17.0)
            .family(fam_display())
            .color(c_text()),
    );
    ui.add_space(12.0);
}

fn sidebar_header(ui: &mut Ui, text: &str, on_all: impl FnOnce()) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).size(14.0).family(fam_semi()).color(c_text()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let r = ui.add(
                egui::Label::new(RichText::new(crate::i18n::t("Все")).size(12.0).color(c_muted()))
                    .sense(Sense::click()),
            );
            if r.clicked() {
                on_all();
            }
        });
    });
    ui.separator();
}

fn art(ui: &mut Ui, url: Option<String>, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let t = ease_out_back(anim(ui, resp.id.with("pop"), resp.hovered())) - 0.5 * press_t(ui, &resp);
    let draw = rect.expand(size * 0.04 * t);
    match url {
        Some(u) => {
            egui::Image::new(u)
                .rounding(3.0 + 3.0 * t.max(0.0))
                .paint_at(ui, draw);
        }
        None => gradient(
            ui.painter(),
            draw,
            Color32::from_rgb(255, 120, 40),
            Color32::from_rgb(120, 30, 80),
        ),
    }
    shine(ui, draw, resp.id, resp.hovered());
    resp
}

/// ▶/⏸ button over the artwork: fades in together with a light darkening.
fn art_overlay(ui: &Ui, r: &Response, playing: bool) {
    let t = anim(ui, r.id.with("overlay"), r.hovered() || playing);
    if t > 0.01 {
        ui.painter()
            .rect_filled(r.rect, 3.0, Color32::from_black_alpha((70.0 * t) as u8));
        play_overlay(ui, r.rect, playing, t);
    }
}

fn play_overlay(ui: &Ui, rect: Rect, playing: bool, alpha: f32) {
    let p = ui.painter();
    let c = rect.center();
    let rad = (rect.width() * 0.17).clamp(10.0, 30.0) * (0.85 + 0.15 * alpha);
    let white = c_text().gamma_multiply(alpha);
    p.circle_filled(c, rad, c_accent().gamma_multiply(alpha));
    if playing {
        let w = rad * 0.18;
        let h = rad * 0.45;
        for dx in [-rad * 0.22, rad * 0.22] {
            p.rect_filled(
                Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(w * 2.0, h * 2.0)),
                1.0,
                white,
            );
        }
    } else {
        let pts = vec![
            c + Vec2::new(-rad * 0.3, -rad * 0.45),
            c + Vec2::new(-rad * 0.3, rad * 0.45),
            c + Vec2::new(rad * 0.5, 0.0),
        ];
        p.add(egui::Shape::convex_polygon(pts, white, Stroke::NONE));
    }
}

fn gradient(painter: &egui::Painter, rect: Rect, left: Color32, right: Color32) {
    let mut m = egui::Mesh::default();
    m.colored_vertex(rect.left_top(), left);
    m.colored_vertex(rect.right_top(), right);
    m.colored_vertex(rect.right_bottom(), right);
    m.colored_vertex(rect.left_bottom(), left);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(m));
}

/// Big wave card: solid color + a drawn waveform (a SoundCloud motif).
/// Lyrics in relax mode: lines centered, the current one in the middle, bigger and with karaoke fill.
fn relax_lyrics(ui: &mut Ui, rect: Rect, lines: &[(f64, String)], pos: f64) -> Option<f64> {
    let ctx = ui.ctx().clone();
    let resp = ui.interact(rect, ui.id().with("relax-lyrics"), Sense::click());
    let now = pos + 0.15;
    let cur = lines.iter().rposition(|(t, _)| *t <= now);
    let wrap = rect.width().min(980.0);
    let font = |k: f32| egui::FontId::new(24.0 + 6.0 * k, fam_semi());
    let disp = |i: usize| -> String {
        let t = lines[i].1.trim();
        if t.is_empty() { "• • •".into() } else { t.to_string() }
    };
    // line heights (at the normal size)
    let heights: Vec<f32> = (0..lines.len())
        .map(|i| ctx.fonts(|f| f.layout(disp(i), font(0.0), Color32::WHITE, wrap)).size().y)
        .collect();
    let gap = 16.0;
    let mut tops = Vec::with_capacity(lines.len());
    let mut acc = 0.0;
    for h in &heights {
        tops.push(acc);
        acc += h + gap;
    }
    let target = cur.map_or(-40.0, |c| tops[c] + heights[c] / 2.0);
    let offset = ctx.animate_value_with_time(resp.id.with("scroll"), target, 0.6);
    let p = ui.painter().with_clip_rect(rect);
    let hover = resp.hover_pos();
    let mut clicked = None;
    for (i, (start, _)) in lines.iter().enumerate() {
        let top = rect.center().y + tops[i] - offset;
        let h = heights[i];
        if top + h < rect.top() - 20.0 || top > rect.bottom() + 20.0 {
            continue;
        }
        let is_cur = Some(i) == cur;
        let k = ctx.animate_bool_with_time(resp.id.with(("l", i)), is_cur, 0.35);
        let mid = top + h / 2.0;
        let edge = ((mid - rect.top()) / (rect.height() * 0.35))
            .min((rect.bottom() - mid) / (rect.height() * 0.35))
            .clamp(0.0, 1.0);
        let past = cur.is_some_and(|c| i < c);
        let base = Color32::from_white_alpha(if past { 120 } else { 90 });
        let next = lines.get(i + 1).map_or(start + 5.0, |l| l.0);
        let progress = if is_cur { ((now - start) / (next - start).max(0.3)).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let f = font(k);
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = wrap;
        job.halign = egui::Align::Center;
        if k > 0.01 {
            let chars: Vec<char> = disp(i).chars().collect();
            let n = chars.len().max(1) as f32;
            for (ci, ch) in chars.iter().enumerate() {
                let fill = ((progress * n) - ci as f32).clamp(0.0, 1.0);
                let unsung = mix(base, Color32::from_white_alpha(150), k);
                let col = mix(unsung, mix(Color32::WHITE, c_accent(), 0.5), fill * k);
                job.append(&ch.to_string(), 0.0, egui::TextFormat { font_id: f.clone(), color: col.gamma_multiply(edge), ..Default::default() });
            }
        } else {
            job.append(&disp(i), 0.0, egui::TextFormat { font_id: f.clone(), color: base.gamma_multiply(edge), ..Default::default() });
        }
        let g = ctx.fonts(|fo| fo.layout_job(job));
        // centered: with halign=Center the galley is drawn relative to its center
        let row = Rect::from_center_size(Pos2::new(rect.center().x, mid), Vec2::new(wrap, h + gap));
        if hover.is_some_and(|hp| row.contains(hp)) {
            p.rect_filled(row, 14.0, Color32::from_white_alpha(10));
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                clicked = Some(*start);
            }
        }
        p.galley(Pos2::new(rect.center().x, mid - g.size().y / 2.0), g, Color32::WHITE);
    }
    clicked
}

/// Relax mode button in the player: a "full screen" frame.
fn relax_button(ui: &mut Ui) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    let h = anim(ui, resp.id, resp.hovered());
    let s = 1.0 + 0.18 * ease_out_back(h) - 0.12 * press_t(ui, &resp);
    let col = mix(c_muted(), c_text(), h);
    let c = rect.center();
    let d = 6.5 * s + 1.5 * h;
    let l = 3.5 * s;
    let st = Stroke::new(1.8_f32, col);
    let p = ui.painter();
    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        let corner = c + Vec2::new(sx * d, sy * d);
        p.line_segment([corner, corner - Vec2::new(sx * l, 0.0)], st);
        p.line_segment([corner, corner - Vec2::new(0.0, sy * l)], st);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A living color blob: an uneven circle with a soft edge (gradient from the center to transparent).
fn blob(p: &egui::Painter, c: Pos2, r: f32, col: Color32, t: f32, seed: f32) {
    let n = 72;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(c, col);
    let mid = col.gamma_multiply(0.7);
    for i in 0..n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let k = 1.0
            + 0.13 * (3.0 * a + t * 0.7 + seed).sin()
            + 0.08 * (5.0 * a - t * 1.1 + seed * 2.0).sin()
            + 0.05 * (2.0 * a + t * 0.4).sin();
        let d = Vec2::angled(a) * r * k;
        mesh.colored_vertex(c + d * 0.55, mid);
        mesh.colored_vertex(c + d, Color32::TRANSPARENT);
    }
    for i in 0..n as u32 {
        let j = (i + 1) % n as u32;
        let (m0, o0, m1, o1) = (1 + i * 2, 2 + i * 2, 1 + j * 2, 2 + j * 2);
        mesh.add_triangle(0, m0, m1);
        mesh.add_triangle(m0, o0, m1);
        mesh.add_triangle(m1, o0, o1);
    }
    p.add(mesh);
}

#[derive(Clone, Copy)]
enum HeroIcon {
    Prev,
    Next,
    Heart,
    Dislike,
}

/// Round "My Wave" icon button (drawn over the blobs, without font glyphs).
fn hero_icon(ui: &mut Ui, p: &egui::Painter, rect: Rect, id: &str, icon: HeroIcon, on: bool) -> bool {
    let resp = ui.interact(rect, ui.id().with(id), Sense::click());
    let h = anim(ui, resp.id, resp.hovered());
    let s = 1.0 + 0.15 * ease_out_back(h) - 0.12 * press_t(ui, &resp);
    let c = rect.center();
    if h > 0.01 {
        p.circle_filled(c, rect.width() / 2.0, Color32::from_white_alpha((26.0 * h) as u8));
    }
    let col = if on { c_accent() } else { Color32::from_white_alpha((200.0 + 55.0 * h) as u8) };
    let st = Stroke::new(2.0_f32, col);
    match icon {
        HeroIcon::Prev | HeroIcon::Next => {
            let d = if matches!(icon, HeroIcon::Next) { 1.0 } else { -1.0 };
            let tri = vec![
                c + Vec2::new(-6.0 * d, -8.0) * s,
                c + Vec2::new(-6.0 * d, 8.0) * s,
                c + Vec2::new(5.0 * d, 0.0) * s,
            ];
            p.add(egui::Shape::convex_polygon(tri, col, Stroke::NONE));
            p.rect_filled(
                Rect::from_center_size(c + Vec2::new(7.0 * d, 0.0) * s, Vec2::new(2.5, 16.0) * s),
                1.0,
                col,
            );
        }
        HeroIcon::Heart => {
            // a heart made of two circles and a triangle
            let r = 4.6 * s;
            let l = c + Vec2::new(-4.2, -2.5) * s;
            let rr = c + Vec2::new(4.2, -2.5) * s;
            if on {
                p.circle_filled(l, r, col);
                p.circle_filled(rr, r, col);
                p.add(egui::Shape::convex_polygon(
                    vec![c + Vec2::new(-8.6, -1.0) * s, c + Vec2::new(8.6, -1.0) * s, c + Vec2::new(0.0, 9.0) * s],
                    col,
                    Stroke::NONE,
                ));
            } else {
                let mut pts = Vec::new();
                for k in 0..=12 {
                    let a = std::f32::consts::PI * (1.0 + k as f32 / 12.0) - 0.35;
                    pts.push(l + Vec2::angled(a) * r);
                }
                for k in 0..=12 {
                    let a = std::f32::consts::PI * (1.0 + k as f32 / 12.0) + 0.35;
                    pts.push(rr + Vec2::angled(a) * r);
                }
                pts.push(c + Vec2::new(0.0, 9.0) * s);
                p.add(egui::Shape::closed_line(pts, st));
            }
        }
        HeroIcon::Dislike => {
            // crossed-out heart
            let r = 4.6 * s;
            let l = c + Vec2::new(-4.2, -2.5) * s;
            let rr = c + Vec2::new(4.2, -2.5) * s;
            let mut pts = Vec::new();
            for k in 0..=12 {
                let a = std::f32::consts::PI * (1.0 + k as f32 / 12.0) - 0.35;
                pts.push(l + Vec2::angled(a) * r);
            }
            for k in 0..=12 {
                let a = std::f32::consts::PI * (1.0 + k as f32 / 12.0) + 0.35;
                pts.push(rr + Vec2::angled(a) * r);
            }
            pts.push(c + Vec2::new(0.0, 9.0) * s);
            p.add(egui::Shape::closed_line(pts, st));
            p.line_segment([c + Vec2::new(-9.0, 9.0) * s, c + Vec2::new(9.0, -9.0) * s], st);
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

#[allow(dead_code)]
fn wave_card(ui: &mut Ui, kind: WaveKind, active: bool, loading: bool) -> bool {
    let w = ui.available_width();
    let (rect, r) = ui.allocate_exact_size(Vec2::new(w, 128.0), Sense::click());
    let (bg, sub, seed) = match kind {
        WaveKind::Day => (
            c_wave_col(),
            crate::i18n::t("Свежие артисты рядом с тем, что ты слушал сегодня"),
            1.7f32,
        ),
        WaveKind::Taste | WaveKind::Radio => (
            c_accent(),
            crate::i18n::t("Любимые артисты, их фиты и похожее"),
            3.1f32,
        ),
    };
    // the main "living" element: on hover the card lifts and the waveform starts "playing"
    let h = anim(ui, r.id, r.hovered());
    let live = anim(ui, r.id.with("live"), r.hovered() || (active && !loading));
    let time = ui.input(|i| i.time) as f32;
    if live > 0.01 {
        ui.ctx().request_repaint();
    }
    let rect = rect.translate(Vec2::new(0.0, -3.0 * h));
    let p = ui.painter();
    if h > 0.01 {
        p.rect_filled(
            rect.translate(Vec2::new(0.0, 4.0)),
            10.0,
            Color32::from_black_alpha((60.0 * h) as u8),
        );
    }
    p.rect_filled(rect, 10.0, mix(bg, c_text(), 0.06 * h));

    // decorative waveform along the bottom edge
    let base = rect.bottom() - 16.0;
    let n = ((rect.width() - 40.0) / 5.0) as usize;
    for i in 0..n {
        let x = rect.left() + 20.0 + i as f32 * 5.0;
        let t = i as f32 * 0.21 + seed;
        let still = (t.sin() * 0.5 + (t * 2.7).sin() * 0.3 + (t * 0.37).cos() * 0.4).abs() * 0.8 + 0.1;
        let dance =
            ((time * 5.0 + i as f32 * 0.55).sin() * 0.5 + (time * 3.1 + i as f32 * 0.23).sin() * 0.5).abs();
        let amp = (still * (1.0 - 0.5 * live) + dance * 0.6 * live).min(1.0);
        let h = 6.0 + amp * 30.0;
        let a = (60.0 + 60.0 * live) as u8;
        p.rect_filled(
            Rect::from_min_max(Pos2::new(x, base - h), Pos2::new(x + 3.0, base)),
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, a),
        );
    }

    p.text(
        rect.left_top() + Vec2::new(22.0, 20.0),
        egui::Align2::LEFT_TOP,
        kind.title(),
        egui::FontId::new(22.0, fam_display()),
        c_text(),
    );
    let subtitle = if loading {
        crate::i18n::t("Собираю треки…")
    } else if active {
        crate::i18n::t("Играет. Нажми ещё раз, чтобы поставить на паузу")
    } else {
        sub
    };
    p.text(
        rect.left_top() + Vec2::new(22.0, 54.0),
        egui::Align2::LEFT_TOP,
        subtitle,
        egui::FontId::proportional(13.0),
        Color32::from_rgba_unmultiplied(255, 255, 255, 220),
    );

    let c = Pos2::new(rect.right() - 52.0, rect.top() + 46.0);
    p.circle_filled(c, 26.0 + 3.0 * h, c_text());
    if active && !loading {
        for dx in [-6.0, 6.0] {
            p.rect_filled(
                Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(5.0, 18.0)),
                1.0,
                bg,
            );
        }
    } else {
        let pts = vec![
            c + Vec2::new(-7.0, -11.0),
            c + Vec2::new(-7.0, 11.0),
            c + Vec2::new(12.0, 0.0),
        ];
        p.add(egui::Shape::convex_polygon(pts, bg, Stroke::NONE));
    }
    shine(ui, rect, r.id, r.hovered());
    r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

fn likes_banner(ui: &mut Ui, count: usize) -> bool {
    let w = ui.available_width();
    let (rect, r) = ui.allocate_exact_size(Vec2::new(w, 64.0), Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 10.0, if r.hovered() { c_hover() } else { c_surface() });
    p.text(
        rect.left_center() + Vec2::new(22.0, 0.0),
        egui::Align2::LEFT_CENTER,
        "♥",
        egui::FontId::proportional(26.0),
        c_accent(),
    );
    p.text(
        rect.left_center() + Vec2::new(60.0, 0.0),
        egui::Align2::LEFT_CENTER,
        crate::i18n::t("Твои лайки"),
        egui::FontId::new(16.0, fam_display()),
        c_text(),
    );
    p.text(
        rect.right_center() - Vec2::new(22.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        crate::i18n::tf("{count} треков", &[("{count}", format!("{count}"))]),
        egui::FontId::proportional(14.0),
        c_muted(),
    );
    r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// Playlist artwork: a 2×2 mosaic of the first tracks (or a single cover / gradient placeholder).
fn playlist_cover(ui: &mut Ui, rect: Rect, tracks: &[Track]) {
    let p = ui.painter().clone();
    let urls: Vec<String> = tracks.iter().filter_map(|t| t.artwork("t300x300")).take(4).collect();
    let rounding = 12.0;
    if urls.is_empty() {
        let g = theme::pal().grad;
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(rect.left_top(), g[0]);
        mesh.colored_vertex(rect.right_top(), g[3]);
        mesh.colored_vertex(rect.right_bottom(), g[theme::N - 1]);
        mesh.colored_vertex(rect.left_bottom(), g[4]);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        p.add(mesh);
        p.rect_filled(rect, rounding, c_accent().gamma_multiply(0.15));
        crate::logo::paint(&p, rect.shrink(rect.width() * 0.28), Color32::from_white_alpha(200), Color32::TRANSPARENT, None);
        return;
    }
    let cells: Vec<Rect> = if urls.len() < 4 {
        vec![rect]
    } else {
        let h = rect.size() / 2.0;
        vec![
            Rect::from_min_size(rect.min, h),
            Rect::from_min_size(rect.min + Vec2::new(h.x, 0.0), h),
            Rect::from_min_size(rect.min + Vec2::new(0.0, h.y), h),
            Rect::from_min_size(rect.min + h, h),
        ]
    };
    for (i, cell) in cells.iter().enumerate() {
        let r = match i {
            _ if cells.len() == 1 => egui::Rounding::same(rounding),
            0 => egui::Rounding { nw: rounding, ..Default::default() },
            1 => egui::Rounding { ne: rounding, ..Default::default() },
            2 => egui::Rounding { sw: rounding, ..Default::default() },
            _ => egui::Rounding { se: rounding, ..Default::default() },
        };
        egui::Image::new(urls[i].as_str()).rounding(r).paint_at(ui, *cell);
    }
}

/// A horizontal strip of cards, in "pages": cards fill the width exactly
/// (nothing is cut off at the edge), arrows at the edges smoothly flip the page.
fn card_row(
    ui: &mut Ui,
    id: &str,
    tracks: &[Track],
    src: Source,
    playing: Option<u64>,
    actions: &mut Vec<Action>,
) {
    ui.push_id(id, |ui| {
        let tracks = &tracks[..tracks.len().min(30)];
        let sid = ui.id().with("pages");
        let w = ui.available_width();
        let gap = 18.0;
        let per = (((w + gap) / (150.0 + gap)).floor() as usize).max(2);
        let size = (w - gap * (per as f32 - 1.0)) / per as f32;
        let pages = tracks.len().div_ceil(per).max(1);
        let mut page: usize = ui.data(|d| d.get_temp(sid)).unwrap_or(0).min(pages - 1);
        let pagef = ui.ctx().animate_value_with_time(sid.with("x"), page as f32, 0.45);
        let height = size + 54.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, height), Sense::hover());
        let clip = rect.expand2(Vec2::new(0.0, 12.0));
        let step = size + gap;
        for (i, t) in tracks.iter().enumerate() {
            let x = rect.left() + (i as f32 - pagef * per as f32) * step;
            if x + size < rect.left() - 1.0 || x > rect.right() + 1.0 {
                continue;
            }
            let cell = Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(size, height));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(cell));
            child.set_clip_rect(clip.intersect(ui.clip_rect()));
            if card(&mut child, t, size, playing == Some(t.id)) {
                actions.push(Action::Play(src, i));
            }
        }
        // arrows
        let hovered = ui.rect_contains_pointer(rect);
        let cy = rect.top() + size / 2.0;
        for (dir, can) in [(-1_i32, page > 0), (1, page + 1 < pages)] {
            let show = anim(ui, sid.with(("show", dir)), hovered && can);
            if show < 0.01 || !can {
                continue;
            }
            let c = Pos2::new(if dir < 0 { rect.left() + 4.0 } else { rect.right() - 4.0 }, cy);
            let r_hit = Rect::from_center_size(c, Vec2::splat(44.0));
            let resp = ui.interact(r_hit, sid.with(("arrow", dir)), Sense::click());
            let h = anim(ui, resp.id, resp.hovered());
            let r = 19.0 * (0.85 + 0.15 * show) * (1.0 + 0.1 * ease_out_back(h));
            let p = ui.painter();
            p.circle_filled(c + Vec2::new(0.0, 3.0), r + 2.0, Color32::from_black_alpha((70.0 * show) as u8));
            p.circle_filled(c, r, mix(theme::pal().window, c_hover(), h).gamma_multiply(show));
            p.circle_stroke(c, r, Stroke::new(1.0_f32, c_text().gamma_multiply(0.15 * show)));
            let a = 5.0;
            let d = dir as f32;
            let col = c_text().gamma_multiply(show);
            let tip = c + Vec2::new(d * a * 0.55, 0.0);
            p.line_segment([tip - Vec2::new(d * a, a), tip], Stroke::new(2.2_f32, col));
            p.line_segment([tip - Vec2::new(d * a, -a), tip], Stroke::new(2.2_f32, col));
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                page = if dir < 0 { page - 1 } else { page + 1 };
            }
        }
        // page dots above the strip on the right
        if pages > 1 {
            let p = ui.painter();
            for k in 0..pages {
                let on = anim(ui, sid.with(("dot", k)), k == page);
                let wdt = 6.0 + 10.0 * on;
                let x = rect.right() - (pages - k) as f32 * 20.0 + (16.0 - wdt) / 2.0;
                let r = Rect::from_min_size(Pos2::new(x, rect.top() - 22.0), Vec2::new(wdt, 6.0));
                p.rect_filled(r, 3.0, mix(c_text().gamma_multiply(0.25), c_accent(), on));
            }
        }
        ui.data_mut(|d| d.insert_temp(sid, page));
    });
}

/// Track card: rounded artwork that lifts with a soft shadow on hover,
/// a "play" button slides in at the bottom right; the playing one shows an "equalizer" in the corner.
fn card(ui: &mut Ui, t: &Track, size: f32, is_current: bool) -> bool {
    let cover = Rect::from_min_size(ui.max_rect().min, Vec2::splat(size));
    let resp = ui.interact(cover, ui.id().with(("card", t.id)), Sense::click());
    track_menu(&resp, t);
    let h = anim(ui, resp.id, resp.hovered());
    let lift = 5.0 * ease_out_back(h) - 2.0 * press_t(ui, &resp);
    let cr = cover.translate(Vec2::new(0.0, -lift));
    let p = ui.painter().clone();
    // soft shadow: several translucent layers
    for k in 1..=4 {
        let e = k as f32 * 3.0;
        p.rect_filled(
            cr.translate(Vec2::new(0.0, 4.0 + 3.0 * h)).expand(e * (0.5 + 0.5 * h)),
            12.0 + e,
            Color32::from_black_alpha(((18.0 + 14.0 * h) / k as f32) as u8),
        );
    }
    match t.artwork("t300x300") {
        Some(u) => {
            egui::Image::new(u).rounding(12.0).paint_at(ui, cr);
        }
        None => {
            let g = theme::pal().grad;
            gradient(&p, cr, g[1], g[theme::N - 2]);
        }
    }
    if is_current {
        p.rect_stroke(cr.shrink(0.5), 12.0, Stroke::new(2.0_f32, c_accent()));
    }
    // darkening at the bottom on hover so the button stays readable
    if h > 0.01 {
        let mut m = egui::Mesh::default();
        let top = Pos2::new(cr.left(), cr.center().y);
        let dark = Color32::from_black_alpha((110.0 * h) as u8);
        m.colored_vertex(top, Color32::TRANSPARENT);
        m.colored_vertex(Pos2::new(cr.right(), top.y), Color32::TRANSPARENT);
        m.colored_vertex(cr.right_bottom() - Vec2::new(0.0, 6.0), dark);
        m.colored_vertex(cr.left_bottom() - Vec2::new(0.0, 6.0), dark);
        m.add_triangle(0, 1, 2);
        m.add_triangle(0, 2, 3);
        p.add(m);
    }
    // the "play/pause" button slides up from below
    if h > 0.01 {
        let c = Pos2::new(cr.right() - 30.0, cr.bottom() - 30.0 + 10.0 * (1.0 - h));
        play_overlay_at(&p, c, 21.0, is_current, h);
    }
    // playing: bouncing bars in the bottom-left corner
    if is_current {
        let time = ui.input(|i| i.time) as f32;
        ui.ctx().request_repaint_after(Duration::from_millis(40));
        let base = Pos2::new(cr.left() + 12.0, cr.bottom() - 12.0);
        p.rect_filled(
            Rect::from_min_size(base - Vec2::new(5.0, 26.0), Vec2::new(30.0, 30.0)),
            8.0,
            Color32::from_black_alpha(140),
        );
        for k in 0..4 {
            let hh = 5.0 + 13.0 * ((time * 6.0 + k as f32 * 1.3).sin() * 0.5 + 0.5);
            p.rect_filled(
                Rect::from_min_max(base + Vec2::new(k as f32 * 5.5, -hh - 2.0), base + Vec2::new(k as f32 * 5.5 + 3.0, -2.0)),
                1.5,
                c_accent(),
            );
        }
    }
    shine(ui, cr, resp.id, resp.hovered());
    // captions
    let text_w = size - 4.0;
    let title_col = if is_current { c_accent() } else { mix(c_text().gamma_multiply(0.92), c_text(), h) };
    let mut job = egui::text::LayoutJob::single_section(
        t.title(),
        egui::TextFormat { font_id: egui::FontId::new(13.0, fam_semi()), color: title_col, ..Default::default() },
    );
    job.wrap.max_width = text_w;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let tg = ui.ctx().fonts(|f| f.layout_job(job));
    let ty = cover.bottom() + 10.0;
    p.galley(Pos2::new(cover.left() + 2.0, ty), tg.clone(), title_col);
    let mut job = egui::text::LayoutJob::single_section(
        t.artist(),
        egui::TextFormat { font_id: egui::FontId::proportional(12.0), color: c_muted(), ..Default::default() },
    );
    job.wrap.max_width = text_w;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let ag = ui.ctx().fonts(|f| f.layout_job(job));
    p.galley(Pos2::new(cover.left() + 2.0, ty + tg.size().y + 3.0), ag, c_muted());
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

fn play_overlay_at(p: &egui::Painter, c: Pos2, rad: f32, playing: bool, alpha: f32) {
    p.circle_filled(c + Vec2::new(0.0, 3.0), rad, Color32::from_black_alpha((70.0 * alpha) as u8));
    p.circle_filled(c, rad, c_accent().gamma_multiply(alpha));
    let ink = INK.gamma_multiply(alpha);
    if playing {
        for dx in [-rad * 0.22, rad * 0.22] {
            p.rect_filled(Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(rad * 0.24, rad * 0.8)), 1.5, ink);
        }
    } else {
        p.add(egui::Shape::convex_polygon(
            vec![c + Vec2::new(-rad * 0.28, -rad * 0.42), c + Vec2::new(-rad * 0.28, rad * 0.42), c + Vec2::new(rad * 0.48, 0.0)],
            ink,
            Stroke::NONE,
        ));
    }
}

#[allow(dead_code)]
fn card_old(ui: &mut Ui, t: &Track, size: f32, is_current: bool) -> bool {
    let mut clicked = false;
    ui.allocate_ui(Vec2::new(size, size + 48.0), |ui| {
        ui.vertical(|ui| {
            ui.set_width(size);
            let r = art(ui, t.artwork("t300x300"), size);
            track_menu(&r, t);
            art_overlay(ui, &r, is_current);
            clicked |= r.clicked();
            let title_color = if is_current { c_accent() } else { c_text() };
            ui.add(
                egui::Label::new(
                    RichText::new(t.title())
                        .size(13.0)
                        .family(fam_semi())
                        .color(title_color),
                )
                .truncate(),
            );
            ui.add(egui::Label::new(RichText::new(t.artist()).size(12.0).color(c_muted())).truncate());
        });
    });
    clicked
}

/// A row in the "Trending by genre" grid.
fn small_row(ui: &mut Ui, t: &Track, is_current: bool) -> bool {
    let mut clicked = false;
    let slot = ui.painter().add(egui::Shape::Noop);
    let row = ui.horizontal(|ui| {
        let r = art(ui, t.artwork("t67x67"), 48.0);
        track_menu(&r, t);
        art_overlay(ui, &r, is_current);
        clicked |= r.clicked();
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            ui.add_space(4.0);
            let col = if is_current { c_accent() } else { c_text() };
            let tr = ui.add(
                egui::Label::new(RichText::new(t.title()).size(13.0).family(fam_semi()).color(col))
                    .truncate()
                    .sense(Sense::click()),
            );
            track_menu(&tr, t);
            clicked |= tr.clicked();
            ui.add(egui::Label::new(RichText::new(t.artist()).size(12.0).color(c_muted())).truncate());
        });
    });
    let bg_rect = row.response.rect.expand2(Vec2::new(6.0, 4.0));
    let base = if is_current {
        c_accent().gamma_multiply(0.16)
    } else {
        Color32::TRANSPARENT
    };
    row_background(ui, slot, bg_rect, egui::Id::new(("srow", t.id)), base);
    ui.add_space(8.0);
    clicked
}

/// A row in the right column (likes / history).
fn side_row(ui: &mut Ui, t: &Track, is_current: bool) -> bool {
    let mut clicked = false;
    let slot = ui.painter().add(egui::Shape::Noop);
    let row = ui.horizontal(|ui| {
        let r = art(ui, t.artwork("t67x67"), 50.0);
        track_menu(&r, t);
        art_overlay(ui, &r, is_current);
        clicked |= r.clicked();
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(t.artist()).size(12.0).color(c_muted())).truncate());
            let col = if is_current { c_accent() } else { c_text() };
            let tr = ui.add(
                egui::Label::new(RichText::new(t.title()).size(13.0).family(fam_semi()).color(col))
                    .truncate()
                    .sense(Sense::click()),
            );
            track_menu(&tr, t);
            clicked |= tr.clicked();
            stats(ui, t);
        });
    });
    let bg_rect = row.response.rect.expand2(Vec2::new(6.0, 4.0));
    let base = if is_current {
        c_accent().gamma_multiply(0.16)
    } else {
        Color32::TRANSPARENT
    };
    row_background(ui, slot, bg_rect, egui::Id::new(("srow", t.id)), base);
    ui.add_space(8.0);
    clicked
}

#[derive(Default)]
struct RowResp {
    play: bool,
    like: bool,
    download: bool,
    reveal: bool,
    copy: bool,
}

impl RowResp {
    fn push(&self, actions: &mut Vec<Action>, src: Source, i: usize, t: &Track) {
        if self.play {
            actions.push(Action::Play(src, i));
        }
        if self.like {
            actions.push(Action::Like(t.clone()));
        }
        if self.download {
            actions.push(Action::Download(t.clone()));
        }
        if self.reveal {
            actions.push(Action::Reveal(t.id));
        }
        if self.copy {
            actions.push(Action::CopyLink(t.clone()));
        }
    }
}

fn avatar_img(ui: &mut Ui, url: Option<String>, name: &str, size: f32) {
    match url {
        Some(u) => {
            ui.add(
                egui::Image::new(u)
                    .fit_to_exact_size(Vec2::splat(size))
                    .rounding(size / 2.0),
            );
        }
        None => {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
            ui.painter().circle_filled(rect.center(), size / 2.0, c_accent());
            let letter: String = name
                .chars()
                .next()
                .map(|c| c.to_uppercase().collect())
                .unwrap_or_default();
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                letter,
                egui::FontId::proportional(size * 0.5),
                c_text(),
            );
        }
    }
}

fn sc_token_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("scloud").join("sc_token.txt"))
}

fn load_sc_token() -> Option<String> {
    sc_token_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn save_sc_token(tok: Option<&str>) {
    if let Some(p) = sc_token_path() {
        match tok {
            Some(t) => {
                if let Some(d) = p.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                let _ = std::fs::write(p, t);
            }
            None => {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

/// A row of equal-width stat tiles. The first one is the main one, with the accent.
fn stat_tiles(ui: &mut Ui, items: &[(String, String)]) {
    let gap = 12.0;
    let w = ui.available_width();
    // if narrow, 3 per row
    let per_row = if w / items.len() as f32 >= 150.0 { items.len() } else { 3 };
    let tile_w = (w - gap * (per_row as f32 - 1.0)) / per_row as f32;
    let h = 92.0;
    for row in items.chunks(per_row) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
        for (i, (title, value)) in row.iter().enumerate() {
            let main = std::ptr::eq(&row[i], &items[0]);
            let r = Rect::from_min_size(
                rect.min + Vec2::new(i as f32 * (tile_w + gap), 0.0),
                Vec2::new(tile_w, h),
            );
            let id = ui.id().with(("stat", title.as_str()));
            let hov = anim(ui, id, ui.rect_contains_pointer(r));
            let r = r.translate(Vec2::new(0.0, -3.0 * ease_out_back(hov)));
            let p = ui.painter();
            if main {
                // main tile: accent gradient
                let a = c_accent();
                let mut mesh = egui::Mesh::default();
                let c0 = a.gamma_multiply(0.55);
                let c1 = mix(a, theme::pal().grad[0], 0.6).gamma_multiply(0.35);
                mesh.colored_vertex(r.left_top(), c0);
                mesh.colored_vertex(r.right_top(), c1);
                mesh.colored_vertex(r.right_bottom(), c1);
                mesh.colored_vertex(r.left_bottom(), c0);
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                p.rect_filled(r, 14.0, c_surface());
                // keep the rounding: draw the gradient slightly inside
                let mut inner = mesh;
                let shrink = |v: &mut egui::epaint::Vertex| {
                    let c = r.center();
                    v.pos = c + (v.pos - c) * Vec2::new((r.width() - 10.0) / r.width(), (r.height() - 10.0) / r.height());
                };
                inner.vertices.iter_mut().for_each(shrink);
                p.add(inner);
                p.rect_stroke(r, 14.0, Stroke::new(1.0_f32, a.gamma_multiply(0.5)));
            } else {
                p.rect_filled(r, 14.0, mix(c_surface(), c_hover(), hov * 0.6));
                p.rect_stroke(r, 14.0, Stroke::new(1.0_f32, c_text().gamma_multiply(0.06 + 0.08 * hov)));
            }
            p.text(
                r.left_top() + Vec2::new(16.0, 14.0),
                egui::Align2::LEFT_TOP,
                title,
                egui::FontId::proportional(12.5),
                if main { c_text().gamma_multiply(0.85) } else { c_muted() },
            );
            // value: shrink the font until it fits
            let mut size = if main { 26.0 } else { 22.0 };
            let max_w = r.width() - 32.0;
            let col = c_text();
            let mut galley = p.layout_no_wrap(value.clone(), egui::FontId::new(size, fam_display()), col);
            while galley.size().x > max_w && size > 12.0 {
                size -= 1.0;
                galley = p.layout_no_wrap(value.clone(), egui::FontId::new(size, fam_display()), col);
            }
            p.galley(
                Pos2::new(r.left() + 16.0, r.bottom() - 14.0 - galley.size().y),
                galley,
                col,
            );
        }
        ui.add_space(gap - ui.spacing().item_spacing.y);
    }
}

/// Bars: theme-dependent colors, a grid, they grow on show, a "pill" tooltip on hover.
fn bar_chart(ui: &mut Ui, key: &str, items: &[(String, f64)], all_labels: bool) {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 190.0), Sense::hover());
    let grow = ui.ctx().animate_bool_with_time(ui.id().with(("chart-grow", key)), true, 0.8);
    let grow = 1.0 - (1.0 - grow).powi(3);
    let painter = ui.painter();
    painter.rect_filled(rect, 14.0, c_surface());
    painter.rect_stroke(rect, 14.0, Stroke::new(1.0_f32, c_text().gamma_multiply(0.06)));
    if items.is_empty() {
        return;
    }
    let plot = rect.shrink2(Vec2::new(18.0, 0.0));
    let top = plot.top() + 34.0;
    let bottom = plot.bottom() - 28.0;
    let max = items.iter().map(|d| d.1).fold(60.0, f64::max);
    // grid: 3 thin lines
    for k in 1..=3 {
        let y = bottom - (bottom - top) * k as f32 / 3.0;
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(1.0_f32, c_text().gamma_multiply(0.05)),
        );
    }
    painter.line_segment(
        [Pos2::new(plot.left(), bottom), Pos2::new(plot.right(), bottom)],
        Stroke::new(1.0_f32, c_text().gamma_multiply(0.12)),
    );
    let slot = plot.width() / items.len() as f32;
    let hover = resp.hover_pos();
    let font = egui::FontId::proportional(11.0);
    let mut tip: Option<(Pos2, String)> = None;
    let bar_w = (slot * 0.62).min(48.0);
    for (i, (label, secs)) in items.iter().enumerate() {
        let x0 = plot.left() + slot * i as f32;
        let col = Rect::from_min_max(Pos2::new(x0, top), Pos2::new(x0 + slot, bottom));
        let hovered = hover.is_some_and(|p| col.contains(p));
        let hk = ui.ctx().animate_bool_with_time(ui.id().with(("bar", key, i)), hovered, 0.15);
        let h = ((secs / max) as f32 * (bottom - top) * grow).max(if *secs > 0.0 { 3.0 } else { 0.0 });
        let cx = col.center().x;
        if *secs <= 0.0 {
            painter.circle_filled(Pos2::new(cx, bottom - 3.0), 1.5, c_text().gamma_multiply(0.15));
        } else {
            let bar = Rect::from_min_max(
                Pos2::new(cx - bar_w / 2.0, bottom - h),
                Pos2::new(cx + bar_w / 2.0, bottom),
            );
            if hk > 0.01 {
                painter.rect_filled(
                    col.shrink2(Vec2::new(2.0, -6.0)),
                    8.0,
                    c_text().gamma_multiply(0.04 * hk),
                );
            }
            let r = egui::Rounding { nw: (bar_w / 2.0).min(6.0), ne: (bar_w / 2.0).min(6.0), ..Default::default() };
            painter.rect_filled(bar, r, mix(c_accent().gamma_multiply(0.7), c_accent(), hk));
        }
        if all_labels {
            painter.text(
                Pos2::new(cx, rect.bottom() - 9.0),
                egui::Align2::CENTER_BOTTOM,
                label,
                font.clone(),
                if hovered { c_text() } else { c_muted() },
            );
        }
        if hovered {
            tip = Some((Pos2::new(cx, bottom - h - 8.0), format!("{label} · {}", fmt_hours(*secs))));
        }
    }
    if !all_labels {
        painter.text(
            Pos2::new(plot.left(), rect.bottom() - 9.0),
            egui::Align2::LEFT_BOTTOM,
            &items[0].0,
            font.clone(),
            c_muted(),
        );
        painter.text(
            Pos2::new(plot.right(), rect.bottom() - 9.0),
            egui::Align2::RIGHT_BOTTOM,
            crate::i18n::t("сегодня"),
            font.clone(),
            c_muted(),
        );
    }
    painter.text(
        Pos2::new(plot.left(), rect.top() + 12.0),
        egui::Align2::LEFT_TOP,
        crate::i18n::tf("макс. {}", &[("{}", format!("{}", fmt_hours(max)))]),
        font,
        c_muted(),
    );
    if let Some((pos, text)) = tip {
        // the "pill" label goes on top of everything so it isn't clipped
        let layer = egui::LayerId::new(egui::Order::Tooltip, ui.id().with(("chart-tip", key)));
        let tp = ui.ctx().layer_painter(layer);
        let g = tp.layout_no_wrap(text, egui::FontId::new(12.0, fam_semi()), INK);
        let pill = Rect::from_center_size(
            Pos2::new(pos.x, pos.y - g.size().y / 2.0 - 4.0),
            g.size() + Vec2::new(16.0, 8.0),
        );
        let pill = pill.translate(Vec2::new(
            (rect.left() + 4.0 - pill.left()).max(0.0) - (pill.right() - rect.right() + 4.0).max(0.0),
            0.0,
        ));
        tp.rect_filled(pill, 8.0, c_text());
        tp.galley(pill.min + Vec2::new(8.0, 4.0), g, INK);
    }
}

/// Track waveform as the progress bar: click or drag to seek.
/// `key` is the track id: when the track changes the bars "grow".
fn waveform_seek(
    ui: &mut Ui,
    key: u64,
    playing: bool,
    samples: Option<&[f32]>,
    pos: f64,
    dur: f64,
    width: f32,
    height: f32,
    drag: &mut Option<f64>,
) -> Option<f64> {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
    let frac_at = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
    let mut result = None;
    if dur > 0.0 {
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                *drag = Some(frac_at(p.x) * dur);
            }
        }
        if resp.drag_stopped() {
            result = drag.take();
        } else if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                result = Some(frac_at(p.x) * dur);
                *drag = None;
            }
        }
    }
    let ctx = ui.ctx().clone();
    let target = if dur > 0.0 {
        (drag.unwrap_or(pos) / dur).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    // smooth progress (between mpv polls) and a gentle glide when seeking
    let played = if drag.is_some() {
        ctx.animate_value_with_time(resp.id.with("played"), target, 0.0)
    } else {
        ctx.animate_value_with_time(resp.id.with("played"), target, 0.25)
    };
    // bars "grow" for a new track and when the waveform finishes loading
    let grow_id = egui::Id::new(("wave-grow", key, samples.is_some()));
    if ctx.data(|d| d.get_temp::<bool>(grow_id)).is_none() {
        // first frame for this track: start from zero (otherwise egui returns the final value right away)
        ctx.data_mut(|d| d.insert_temp(grow_id, true));
        ctx.animate_value_with_time(grow_id.with("v"), 0.0, 0.0);
    }
    let grow = ctx.animate_value_with_time(grow_id.with("v"), 1.0, 0.6);
    let hover_t = anim(ui, resp.id.with("hover"), resp.hovered() || drag.is_some());
    let hover = resp.hover_pos().map(|p| frac_at(p.x) as f32);

    let (bar, gap) = (2.0, 1.0);
    let n = (rect.width() / (bar + gap)).floor().max(1.0) as usize;
    let painter = ui.painter();
    let head = (played * n as f32) as usize;
    let time = ui.input(|i| i.time) as f32;
    if playing {
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
    for i in 0..n {
        let f = i as f32 / n as f32;
        let amp = match samples {
            Some(s) if !s.is_empty() => {
                let a = ((f * s.len() as f32) as usize).min(s.len() - 1);
                let b = ((((i + 1) as f32 / n as f32) * s.len() as f32) as usize).clamp(a + 1, s.len());
                s[a..b].iter().copied().fold(0.0, f32::max)
            }
            _ => 0.18, // until the waveform loads: a flat bar
        };
        // the wave rolls in from left to right when it appears
        let local = ((grow * 1.6 - f * 0.6).clamp(0.0, 1.0)).powf(0.7);
        // "dock": bars under the cursor get bigger
        let dock = hover.map_or(0.0, |hv| {
            let d = (f - hv) * rect.width() / 34.0;
            (-d * d).exp() * hover_t
        });
        // around the playhead the bars "breathe" while music plays
        let di = i as f32 - head as f32;
        let breathe = if playing && dur > 0.0 {
            (time * 9.0 + i as f32 * 0.7).sin() * 0.22 * (-(di / 7.0).powi(2)).exp()
        } else {
            0.0
        };
        let h = amp.clamp(0.06, 1.0) * rect.height() * local * (1.0 + 0.04 * hover_t + 0.45 * dock + breathe);
        let x = rect.left() + i as f32 * (bar + gap);
        let top = Rect::from_min_max(
            Pos2::new(x, rect.center().y - h * 0.62),
            Pos2::new(x + bar, rect.center().y),
        );
        let low = Rect::from_min_max(
            Pos2::new(x, rect.center().y + 1.0),
            Pos2::new(x + bar, rect.center().y + 1.0 + h * 0.38),
        );
        let (c_top, c_low) = if i == head && dur > 0.0 {
            (c_text(), Color32::from_rgb(170, 170, 170)) // the playhead
        } else if f < played {
            let shimmer = if playing {
                0.5 + 0.5 * (time * 3.0 - i as f32 * 0.12).sin()
            } else {
                0.0
            };
            let top = mix(c_accent(), c_text(), 0.16 * shimmer + 0.25 * dock);
            (top, c_accent().gamma_multiply(0.55))
        } else if hover.is_some_and(|hv| f < hv) {
            (c_accent().gamma_multiply(0.55), c_accent().gamma_multiply(0.3))
        } else {
            (c_text().gamma_multiply(0.36), c_text().gamma_multiply(0.18))
        };
        painter.rect_filled(top, 0.0, c_top);
        painter.rect_filled(low, 0.0, c_low);
    }
    // on hover: a line and the time in a pill
    if let (Some(hv), true) = (hover, dur > 0.0) {
        let x = rect.left() + hv * rect.width();
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0_f32, c_text().gamma_multiply(0.7 * hover_t)),
        );
        let text = fmt_time(hv as f64 * dur);
        let galley = painter.layout_no_wrap(text, egui::FontId::new(11.0, fam_semi()), INK);
        let pill = Rect::from_center_size(
            Pos2::new(x, rect.top() - 4.0 - galley.size().y / 2.0 - 3.0),
            galley.size() + Vec2::new(12.0, 6.0),
        );
        let pill = pill.translate(Vec2::new(
            (rect.left() - pill.left()).max(0.0) - (pill.right() - rect.right()).max(0.0),
            0.0,
        ));
        let top_layer = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("wave-time"),
        ));
        top_layer.rect_filled(
            pill.expand(1.0),
            6.0,
            Color32::from_black_alpha((90.0 * hover_t) as u8),
        );
        top_layer.rect_filled(pill, 5.0, c_text().gamma_multiply(hover_t));
        top_layer.galley(pill.center() - galley.size() / 2.0, galley, INK);
    }
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    result
}

/// Volume: a thin bar, the knob appears on hover. Returns the new value.
fn volume_bar(ui: &mut Ui, volume: f32) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(90.0, 22.0), Sense::click_and_drag());
    let t = anim(ui, resp.id, resp.hovered() || resp.dragged());
    let mut out = None;
    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            out = Some(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * 100.0);
        }
    }
    let v = out.unwrap_or(volume) / 100.0;
    let track_h = 3.0 + 2.0 * t;
    let line = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), track_h));
    let p = ui.painter();
    p.rect_filled(line, track_h / 2.0, c_text().gamma_multiply(0.25));
    let mut fill = line;
    fill.set_width(line.width() * v);
    p.rect_filled(fill, track_h / 2.0, mix(c_text(), c_accent(), t));
    if t > 0.01 {
        p.circle_filled(Pos2::new(fill.right(), line.center().y), 6.0 * t, c_text());
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    out
}

/// Heart: "bounces" on like/unlike.
fn heart_btn(ui: &mut Ui, liked: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    let state = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("liked"), liked, 0.35);
    let bounce = (state * std::f32::consts::PI).sin(); // peak in the middle of the transition
    let h = ease_out_back(anim(ui, resp.id, resp.hovered()));
    let scale = 1.0 + 0.45 * bounce + 0.18 * h - 0.12 * press_t(ui, &resp);
    let color = mix(
        mix(c_muted(), c_text(), h.clamp(0.0, 1.0) * 0.6),
        c_accent(),
        state,
    );
    let angle = -0.18 * h + 0.25 * bounce;
    let galley = ui
        .painter()
        .layout_no_wrap("♥".into(), egui::FontId::proportional(18.0 * scale), color);
    let half = galley.size() / 2.0;
    let (sin, cos) = angle.sin_cos();
    let pos = rect.center() - Vec2::new(half.x * cos - half.y * sin, half.x * sin + half.y * cos);
    ui.painter()
        .add(egui::epaint::TextShape::new(pos, galley, color).with_angle(angle));
    resp.on_hover_text(if liked {
        crate::i18n::t("Убрать из лайков")
    } else {
        crate::i18n::t("Нравится")
    })
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .clicked()
}

/// Big ▶/⏸ button: a white circle that springs on hover and press.
fn play_button(ui: &mut Ui, paused: bool, loading: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
    let h = anim(ui, resp.id, resp.hovered());
    let press = anim(ui, resp.id.with("press"), resp.is_pointer_button_down_on());
    let r = 16.0 * (1.0 + 0.08 * h - 0.08 * press);
    let c = rect.center();
    let p = ui.painter();
    p.circle_filled(c, r, c_text());
    let ink = INK;
    if loading {
        // three dots run around a circle
        let time = ui.input(|i| i.time) as f32;
        for k in 0..3 {
            let a = time * 4.0 + k as f32 * 2.1;
            p.circle_filled(c + Vec2::new(a.cos(), a.sin()) * r * 0.45, 2.2, ink);
        }
        ui.ctx().request_repaint();
    } else if paused {
        let s = r * 0.5;
        let pts = vec![
            c + Vec2::new(-s * 0.55, -s),
            c + Vec2::new(-s * 0.55, s),
            c + Vec2::new(s, 0.0),
        ];
        p.add(egui::Shape::convex_polygon(pts, ink, Stroke::NONE));
    } else {
        for dx in [-r * 0.22, r * 0.22] {
            p.rect_filled(
                Rect::from_center_size(c + Vec2::new(dx, 0.0), Vec2::new(r * 0.2, r * 0.9)),
                1.0,
                ink,
            );
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// Theme card in settings: gradient preview + name.
fn theme_swatch(ui: &mut Ui, key: &str, name: &str, current: &ThemeSettings) -> bool {
    let pal = ThemeSettings::preview(key, current);
    swatch(ui, &pal, name, current.preset == key).clicked()
}

/// Preview card for any palette (built-in themes and "My themes").
fn swatch(ui: &mut Ui, pal: &theme::Palette, name: &str, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(88.0, 76.0), Sense::click());
    let h = ease_out_back(anim(ui, resp.id, resp.hovered())) - 0.6 * press_t(ui, &resp);
    let card = Rect::from_min_size(rect.min, Vec2::new(88.0, 54.0)).expand(2.5 * h);
    paint_grad_rect(ui.painter(), card, &pal.grad, 90.0 + (pal.angle - 90.0));
    // a mini UI inside the preview: a bar and an accent
    let p = ui.painter();
    p.rect_filled(
        Rect::from_min_size(card.left_top() + Vec2::new(8.0, 8.0), Vec2::new(40.0, 7.0)),
        3.0,
        pal.surface.gamma_multiply(1.5),
    );
    p.circle_filled(card.right_bottom() - Vec2::new(14.0, 14.0), 7.0, pal.accent);
    let stroke = if selected {
        Stroke::new(2.0_f32, c_text())
    } else {
        Stroke::new(1.0_f32, c_text().gamma_multiply(0.2))
    };
    p.rect_stroke(card, 8.0, stroke);
    p.text(
        Pos2::new(rect.center().x, rect.bottom() - 2.0),
        egui::Align2::CENTER_BOTTOM,
        name,
        egui::FontId::new(12.0, fam_semi()),
        if selected { c_text() } else { c_muted() },
    );
    shine(ui, card, resp.id, resp.hovered());
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A rectangle with the palette gradient at an angle (like the window background, only small).
fn paint_grad_rect(painter: &egui::Painter, r: Rect, grad: &[Color32; theme::N], angle: f32) {
    let ang = angle.to_radians();
    let dir = Vec2::new(ang.sin(), -ang.cos());
    let half = ((r.width() * dir.x.abs() + r.height() * dir.y.abs()) / 2.0).max(1.0);
    let c = r.center();
    let n = 8u32;
    let mut mesh = egui::Mesh::default();
    for iy in 0..=n {
        for ix in 0..=n {
            let pt = Pos2::new(
                r.left() + r.width() * ix as f32 / n as f32,
                r.top() + r.height() * iy as f32 / n as f32,
            );
            mesh.colored_vertex(pt, theme::grad_at(grad, 0.5 + (pt - c).dot(dir) / (2.0 * half)));
        }
    }
    let w = n + 1;
    for iy in 0..n {
        for ix in 0..n {
            let i = iy * w + ix;
            mesh.add_triangle(i, i + 1, i + w + 1);
            mesh.add_triangle(i, i + w + 1, i + w);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// "Custom" theme editor, like Discord's: gradient bar with keys, color picker, angle, accents.
fn theme_editor(ui: &mut Ui, th: &mut ThemeSettings, sel: &mut usize) -> bool {
    let mut changed = false;
    if th.stops.len() < 2 {
        th.stops = ThemeSettings::stops_of("gta6");
    }
    *sel = (*sel).min(th.stops.len() - 1);

    ui.label(
        RichText::new(crate::i18n::t(
            "Градиент фона — тяни ключи, клик по полосе добавляет новый",
        ))
        .size(12.0)
        .color(c_muted()),
    );
    ui.add_space(4.0);

    // ---- bar with keys ----
    let w = ui.available_width().min(520.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 46.0), Sense::click_and_drag());
    let bar = Rect::from_min_max(
        rect.left_top() + Vec2::new(8.0, 8.0),
        rect.right_bottom() - Vec2::new(8.0, 8.0),
    );
    let pos_at = |x: f32| ((x - bar.left()) / bar.width()).clamp(0.0, 1.0);
    let drag_id = resp.id.with("drag");
    let pointer = resp.interact_pointer_pos();
    if resp.drag_started() || resp.clicked() {
        if let Some(p) = pointer {
            let hit = th
                .stops
                .iter()
                .enumerate()
                .map(|(i, s)| (i, (bar.left() + s.pos * bar.width() - p.x).abs()))
                .filter(|(_, d)| *d < 10.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|x| x.0);
            match hit {
                Some(i) => *sel = i,
                None => {
                    let pos = pos_at(p.x);
                    let c = theme::sample(&th.stops, pos);
                    th.stops.push(theme::Stop {
                        pos,
                        color: [c.r(), c.g(), c.b()],
                    });
                    *sel = th.stops.len() - 1;
                    changed = true;
                }
            }
            ui.ctx().data_mut(|d| d.insert_temp(drag_id, true));
        }
    }
    if resp.dragged() && ui.ctx().data(|d| d.get_temp::<bool>(drag_id)).unwrap_or(false) {
        if let Some(p) = pointer {
            th.stops[*sel].pos = pos_at(p.x);
            changed = true;
        }
    }
    if resp.drag_stopped() {
        ui.ctx().data_mut(|d| d.remove::<bool>(drag_id));
    }
    let mut grad = [Color32::BLACK; theme::N];
    for (i, g) in grad.iter_mut().enumerate() {
        *g = theme::sample(&th.stops, i as f32 / (theme::N - 1) as f32);
    }
    let painter = ui.painter();
    paint_grad_rect(painter, bar, &grad, 90.0);
    painter.rect_stroke(bar, 6.0, Stroke::new(1.0_f32, c_text().gamma_multiply(0.25)));
    for (i, st) in th.stops.iter().enumerate() {
        let x = bar.left() + st.pos * bar.width();
        let is_sel = i == *sel;
        let hr = Rect::from_center_size(Pos2::new(x, bar.center().y), Vec2::new(16.0, bar.height() + 12.0));
        let k = ui
            .ctx()
            .animate_bool_with_time(resp.id.with(("key", i)), is_sel, 0.15);
        let hr = hr.expand(2.0 * k);
        painter.rect_filled(hr, 5.0, Color32::from_rgb(st.color[0], st.color[1], st.color[2]));
        painter.rect_stroke(
            hr,
            5.0,
            Stroke::new(
                2.0 + 1.0 * k,
                if is_sel {
                    c_text()
                } else {
                    c_text().gamma_multiply(0.6)
                },
            ),
        );
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // ---- color of the selected key ----
    ui.add_space(8.0);
    ui.horizontal_top(|ui| {
        let n_stops = th.stops.len();
        let st = &mut th.stops[*sel];
        let mut c = Color32::from_rgb(st.color[0], st.color[1], st.color[2]);
        if egui::widgets::color_picker::color_picker_color32(
            ui,
            &mut c,
            egui::widgets::color_picker::Alpha::Opaque,
        ) {
            st.color = [c.r(), c.g(), c.b()];
            changed = true;
        }
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.label(
                RichText::new(crate::i18n::tf(
                    "Ключ {} из {}",
                    &[("{}", format!("{}", *sel + 1)), ("{}", format!("{}", n_stops))],
                ))
                .family(fam_semi()),
            );
            // #hex field
            let hex_id = egui::Id::new(("theme-hex", *sel));
            let current = format!("{:02x}{:02x}{:02x}", st.color[0], st.color[1], st.color[2]);
            let mut hex: String = ui
                .ctx()
                .data(|d| d.get_temp(hex_id))
                .unwrap_or_else(|| current.clone());
            ui.horizontal(|ui| {
                ui.label("#");
                let r = ui.add(
                    egui::TextEdit::singleline(&mut hex)
                        .desired_width(80.0)
                        .char_limit(6),
                );
                if r.changed() && hex.len() == 6 {
                    if let Ok(v) = u32::from_str_radix(&hex, 16) {
                        st.color = [(v >> 16) as u8, (v >> 8) as u8, v as u8];
                        changed = true;
                    }
                }
                if !r.has_focus() {
                    hex = format!("{:02x}{:02x}{:02x}", st.color[0], st.color[1], st.color[2]);
                }
            });
            ui.ctx().data_mut(|d| d.insert_temp(hex_id, hex));
            // quick colors
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                for q in [
                    [86, 34, 168],
                    [176, 92, 44],
                    [214, 62, 104],
                    [255, 79, 163],
                    [41, 211, 255],
                    [10, 62, 104],
                    [18, 18, 18],
                    [255, 85, 0],
                ] {
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                    let h = anim(ui, resp.id, resp.hovered());
                    ui.painter()
                        .rect_filled(r.expand(2.0 * h), 5.0, Color32::from_rgb(q[0], q[1], q[2]));
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        st.color = q;
                        changed = true;
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(crate::i18n::t("Позиция"));
                changed |= ui
                    .add(
                        egui::Slider::new(&mut st.pos, 0.0..=1.0)
                            .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                    )
                    .changed();
            });
        });
    });
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                th.stops.len() > 2,
                egui::Button::new(crate::i18n::t("Удалить ключ")),
            )
            .clicked()
        {
            th.stops.remove(*sel);
            *sel = sel.saturating_sub(1);
            changed = true;
        }
        if ui.button(crate::i18n::t("Сбросить к GTA VI")).clicked() {
            th.stops = ThemeSettings::stops_of("gta6");
            th.angle = 180.0;
            *sel = 0;
            changed = true;
        }
    });

    // ---- angle and accents ----
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(crate::i18n::t("Угол"));
        for (icon, a) in [("↓", 180.0), ("↘", 135.0), ("→", 90.0), ("↗", 45.0)] {
            if chip(ui, icon, (th.angle - a).abs() < 0.5) {
                th.angle = a;
                changed = true;
            }
        }
        changed |= ui
            .add(egui::Slider::new(&mut th.angle, 0.0..=360.0).custom_formatter(|v, _| format!("{v:.0}°")))
            .changed();
    });
    ui.horizontal(|ui| {
        ui.label(crate::i18n::t("Акцент"));
        changed |= ui.color_edit_button_srgb(&mut th.accent).changed();
        ui.add_space(12.0);
        ui.label(crate::i18n::t("Цвет «Волны»"));
        changed |= ui.color_edit_button_srgb(&mut th.wave).changed();
    });
    ui.horizontal(|ui| {
        ui.label(crate::i18n::t("Затемнение панелей"));
        changed |= ui
            .add(
                egui::Slider::new(&mut th.shade, 0.0..=0.8)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                    .trailing_fill(true),
            )
            .changed();
    });
    changed
}

fn fx_button(ui: &mut Ui, on: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(40.0, 28.0), Sense::click());
    let h = ease_out_back(anim(ui, resp.id, resp.hovered()));
    let k = 1.0 + 0.1 * h - 0.08 * press_t(ui, &resp);
    let r = Rect::from_center_size(rect.center(), rect.size() * k);
    let p = ui.painter();
    p.rect_filled(
        r,
        6.0,
        if on {
            c_accent()
        } else {
            mix(c_hover(), c_text().gamma_multiply(0.2), h.clamp(0.0, 1.0))
        },
    );
    p.text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        "FX",
        egui::FontId::new(13.0 * k, fam_semi()),
        if on { INK } else { c_text() },
    );
    shine(ui, r, resp.id, resp.hovered());
    resp.on_hover_text(crate::i18n::t("Эффекты: slowed, reverb, bass boost…"))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

fn link_button(ui: &mut Ui) -> bool {
    ui.add(AnimIcon("🔗", 15.0, c_muted()))
        .on_hover_text(crate::i18n::t("Скопировать ссылку на SoundCloud"))
        .clicked()
}

/// Gear: rotates 60° with a spring on hover.
fn gear_btn(ui: &mut Ui) -> bool {
    let size = 18.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size * 1.4), Sense::click());
    let t = ease_out_back(anim(ui, resp.id, resp.hovered()));
    let angle = std::f32::consts::FRAC_PI_3 * t;
    let c = mix(c_muted(), c_text(), t.clamp(0.0, 1.0));
    let k = 1.0 + 0.1 * t - 0.12 * press_t(ui, &resp);
    let galley = ui
        .painter()
        .layout_no_wrap("⚙".into(), egui::FontId::proportional(size * k), c);
    let half = galley.size() / 2.0;
    let (sin, cos) = angle.sin_cos();
    let pos = rect.center() - Vec2::new(half.x * cos - half.y * sin, half.x * sin + half.y * cos);
    ui.painter()
        .add(egui::epaint::TextShape::new(pos, galley, c).with_angle(angle));
    resp.on_hover_text(crate::i18n::t("Настройки"))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Download button: ⬇ → percent → ✔ (click shows the file) / ⚠ (click retries).
fn download_button(ui: &mut Ui, dl: Option<&DlState>, out: &mut RowResp) {
    let warn = Color32::from_rgb(240, 160, 90);
    match dl {
        None => {
            let b = AnimIcon("⬇", 17.0, c_muted());
            if ui
                .add(b)
                .on_hover_text(crate::i18n::t("Скачать MP3 (с текущими эффектами FX)"))
                .clicked()
            {
                out.download = true;
            }
        }
        Some(DlState::Running(p)) => {
            ui.label(
                RichText::new(format!("{:.0}%", p * 100.0))
                    .size(12.0)
                    .color(c_accent()),
            );
        }
        Some(DlState::Done(_)) => {
            let b = AnimIcon("✔", 16.0, c_accent());
            if ui
                .add(b)
                .on_hover_text(crate::i18n::t("Скачано — показать файл"))
                .clicked()
            {
                out.reveal = true;
            }
        }
        Some(DlState::Failed(e)) => {
            let b = AnimIcon("⚠", 16.0, warn);
            if ui
                .add(b)
                .on_hover_text(crate::i18n::tf(
                    "{e}\nНажми, чтобы попробовать ещё раз",
                    &[("{e}", format!("{e}"))],
                ))
                .clicked()
            {
                out.download = true;
            }
        }
    }
}

/// A track row in search and the library.
fn track_row(ui: &mut Ui, t: &Track, is_current: bool, liked: bool, dl: Option<&DlState>) -> RowResp {
    let mut out = RowResp::default();
    let fill = if is_current {
        c_accent().gamma_multiply(0.16)
    } else {
        Color32::TRANSPARENT
    };
    let slot = ui.painter().add(egui::Shape::Noop);
    let framed = egui::Frame::none()
        .inner_margin(Margin::same(6.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let r = art(ui, t.artwork("t67x67"), 60.0);
                track_menu(&r, t);
                art_overlay(ui, &r, is_current);
                out.play |= r.clicked();
                ui.vertical(|ui| {
                    ui.set_max_width((ui.available_width() - 120.0).max(100.0));
                    ui.add(
                        egui::Label::new(RichText::new(t.artist()).size(12.0).color(c_muted())).truncate(),
                    );
                    let col = if is_current { c_accent() } else { c_text() };
                    let tr = ui.add(
                        egui::Label::new(RichText::new(t.title()).size(14.0).family(fam_semi()).color(col))
                            .truncate()
                            .sense(Sense::click()),
                    );
                    track_menu(&tr, t);
                    out.play |= tr.clicked();
                    ui.horizontal(|ui| {
                        stats(ui, t);
                        if t.is_preview() {
                            ui.label(
                                RichText::new(crate::i18n::t(" Go+ превью "))
                                    .size(10.0)
                                    .background_color(c_accent())
                                    .color(Color32::BLACK),
                            );
                        }
                        if let Some(g) = t.genre.as_deref().filter(|g| !g.is_empty()) {
                            ui.label(RichText::new(format!("#{g}")).size(11.0).color(c_muted()));
                        }
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let dur = t.full_duration.unwrap_or(t.duration);
                    ui.label(RichText::new(fmt_time(dur as f64 / 1000.0)).color(c_muted()));
                    if link_button(ui) {
                        out.copy = true;
                    }
                    download_button(ui, dl, &mut out);
                    if heart_btn(ui, liked) {
                        out.like = true;
                    }
                });
            });
        });
    row_background(ui, slot, framed.response.rect, egui::Id::new(("row", t.id)), fill);
    out
}

fn stats(ui: &mut Ui, t: &Track) {
    let mut parts = Vec::new();
    if let Some(n) = t.playback_count {
        parts.push(format!("▶ {}", fmt_count(n)));
    }
    if let Some(n) = t.likes_count {
        parts.push(format!("♥ {}", fmt_count(n)));
    }
    if let Some(n) = t.reposts_count {
        parts.push(format!("🔁 {}", fmt_count(n)));
    }
    if let Some(n) = t.comment_count {
        parts.push(format!("💬 {}", fmt_count(n)));
    }
    if !parts.is_empty() {
        ui.label(RichText::new(parts.join("   ")).size(11.0).color(c_muted()));
    }
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn fmt_count(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}K", n as f64 / 1e3),
        n => n.to_string(),
    }
}

fn rand_below(n: usize) -> usize {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(7);
    (nanos as usize).wrapping_mul(2654435761) % n
}

fn settings_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("scloud").join("settings.json"))
}

fn load_settings() -> serde_json::Value {
    settings_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn save_settings(v: &serde_json::Value) {
    if let Some(p) = settings_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::to_string_pretty(v).unwrap_or_default());
    }
}
