//! Oxidify application shell: owns every engine, pumps UI commands,
//! and feeds per-frame snapshots back to the interface.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use oxidify_api::{Codec, RemoteClient};
use oxidify_audio::{AudioEngine, EngineEvent};
use oxidify_core::{
    RepeatMode, SyncedLine, active_line_index, apply_lyrics_offset, next_index, parse_lrc,
    prev_index, word_progress,
};
use oxidify_graphics::{AetherBackground, extract_palette_rgba};
use oxidify_storage::secrets;
use oxidify_storage::{LibraryDb, StoredTrack};
use oxidify_ui::{
    LibraryView, LyricsLineView, LyricsView, PlaybackView, UiAlbum, UiCommand, UiPlaylist, UiState,
    UiTrack, push_toast, show_full_ui,
};

use crate::desktop::{DesktopCommand, DesktopHooks};
use crate::diag::diag;

const PREF_VOLUME: &str = "volume";
const PREF_FOLDERS: &str = "library_folders";
const PREF_SERVICE_URL: &str = "service_url";
const PREF_DEVICE: &str = "output_device";
const PREF_RATE: &str = "playback_rate";
const PREF_PITCH: &str = "pitch_semitones";
const PREF_LINKED: &str = "pitch_linked";
const PREF_REVERB_ON: &str = "reverb_enabled";
const PREF_REVERB_LVL: &str = "reverb_level";
const PREF_LYRICS_OFFSET: &str = "lyrics_offset_ms";
const PREF_REPEAT: &str = "repeat";
const PREF_SHUFFLE: &str = "shuffle";

const SERVICE_TOKEN_ACCOUNT: &str = "service_token";

#[derive(Debug, Clone)]
enum Origin {
    Local(PathBuf),
    Remote(String),
}

#[derive(Debug, Clone)]
struct QueueEntry {
    id: String,
    title: String,
    artists: String,
    album: String,
    duration_ms: u64,
    cover: Option<PathBuf>,
    origin: Origin,
}

impl QueueEntry {
    fn to_ui(&self) -> UiTrack {
        UiTrack {
            id: self.id.clone(),
            title: self.title.clone(),
            artists: self.artists.clone(),
            album: self.album.clone(),
            duration_ms: self.duration_ms,
            cover_path: self.cover.clone(),
            remote: matches!(self.origin, Origin::Remote(_)),
        }
    }
}

enum BgDone {
    ScanDone(String),
    DownloadDone { id: String, path: PathBuf, codec: Codec },
    DownloadFailed { id: String, message: String },
    LoginDone(Result<String, String>),
    LyricsDone { id: String, lines: Vec<SyncedLine>, synced: bool },
    RemoteSearchDone { query: String, items: Vec<QueueEntry> },
}

pub struct OxidifyShell {
    engine: AudioEngine,
    db: LibraryDb,
    ui: UiState,
    aether: AetherBackground,
    desktop: DesktopHooks,
    rt: tokio::runtime::Handle,
    bg_tx: Sender<BgDone>,
    bg_rx: Receiver<BgDone>,
    db_path: PathBuf,
    audio_cache_dir: PathBuf,
    cover_cache_dir: PathBuf,

    queue: Vec<QueueEntry>,
    index: i64,
    current_id: Option<String>,
    loading: bool,

    volume: f32,
    rate: f32,
    pitch_semitones: f32,
    pitch_linked: bool,
    reverb_enabled: bool,
    reverb_level: f32,
    repeat: u8,
    shuffle: bool,
    lyrics_offset_ms: i32,
    lyrics_lines: Vec<SyncedLine>,
    lyrics_synced: bool,

    search_query: String,
    remote_items: Vec<QueueEntry>,
    remote: Option<RemoteClient>,
    service_url: Option<String>,

    lib_view: LibraryView,
    lib_dirty: bool,
    scan_busy: bool,
    download_busy: bool,

    play_started_at: Option<Instant>,
    play_open_id: Option<String>,
    frames: u64,
    fps_ema: f32,
    last_frame: Instant,
}

impl OxidifyShell {
    pub fn create(ctx: &egui::Context, rt: tokio::runtime::Handle) -> anyhow::Result<Self> {
        diag("shell create start");
        // Slightly translucent panels so the Aether background glows through.
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgba_unmultiplied(12, 14, 20, 224);
        visuals.window_fill = egui::Color32::from_rgba_unmultiplied(12, 14, 20, 232);
        ctx.set_visuals(visuals);

        let proj = directories::ProjectDirs::from("com", "oxidify", "Oxidify")
            .ok_or_else(|| anyhow::anyhow!("no app directories"))?;
        let data_dir = proj.data_dir().to_path_buf();
        let cache_dir = proj.cache_dir().to_path_buf();
        let audio_cache_dir = cache_dir.join("audio");
        let cover_cache_dir = cache_dir.join("covers");
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&audio_cache_dir)?;
        std::fs::create_dir_all(&cover_cache_dir)?;

        let db_path = data_dir.join("library.sqlite");
        let db = LibraryDb::open(&db_path)?;
        diag("db open ok");

        let get = |k: &str| db.pref_get(k).unwrap_or(None);
        let volume: f32 = get(PREF_VOLUME).and_then(|v| v.parse().ok()).unwrap_or(0.9);
        let rate: f32 = get(PREF_RATE).and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0).clamp(0.25, 3.0);
        let pitch_semitones: f32 = get(PREF_PITCH).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0).clamp(-12.0, 12.0);
        let pitch_linked: bool = get(PREF_LINKED).map(|v| v != "0").unwrap_or(true);
        let reverb_enabled: bool = get(PREF_REVERB_ON).map(|v| v == "1").unwrap_or(false);
        let reverb_level: f32 = get(PREF_REVERB_LVL).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.35).clamp(0.0, 1.0);
        let repeat: u8 = get(PREF_REPEAT).and_then(|v| v.parse().ok()).unwrap_or(0);
        let shuffle: bool = get(PREF_SHUFFLE).map(|v| v == "1").unwrap_or(false);
        let lyrics_offset_ms: i32 = get(PREF_LYRICS_OFFSET)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let folders: Vec<PathBuf> = get(PREF_FOLDERS)
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default();
        let service_url: Option<String> = get(PREF_SERVICE_URL).filter(|s| !s.is_empty());

        let mut engine = AudioEngine::new().map_err(|e| anyhow::anyhow!("audio: {e}"))?;
        diag("audio engine ok");
        engine.set_volume(volume);
        engine.set_rate(rate);
        engine.set_pitch(pitch_semitones, pitch_linked);
        engine.set_reverb(reverb_enabled, reverb_level);
        if let Some(dev) = get(PREF_DEVICE).filter(|d| !d.is_empty()) {
            if let Err(e) = engine.set_output_device(Some(&dev)) {
                tracing::warn!("saved output device unavailable ({dev}): {e}");
            }
        }

        let mut remote = None;
        if let Some(url) = service_url.clone() {
            match RemoteClient::new(&url) {
                Ok(c) => {
                    if let Ok(Some(token)) = secrets::load_secret(SERVICE_TOKEN_ACCOUNT) {
                        c.set_bearer(Some(token));
                    }
                    remote = Some(c);
                }
                Err(e) => tracing::warn!("saved service url invalid: {e}"),
            }
        }

        let (bg_tx, bg_rx) = channel();
        diag("building desktop hooks");
        let mut shell = Self {
            engine,
            db,
            ui: UiState::new(),
            aether: AetherBackground::new(oxidify_graphics::AetherParams::default()),
            desktop: DesktopHooks::new(),
            rt,
            bg_tx,
            bg_rx,
            db_path,
            audio_cache_dir,
            cover_cache_dir,
            queue: Vec::new(),
            index: -1,
            current_id: None,
            loading: false,
            volume,
            rate,
            pitch_semitones,
            pitch_linked,
            reverb_enabled,
            reverb_level,
            repeat: repeat.min(2),
            shuffle,
            lyrics_offset_ms,
            lyrics_lines: Vec::new(),
            lyrics_synced: false,
            search_query: String::new(),
            remote_items: Vec::new(),
            remote,
            service_url,
            lib_view: LibraryView {
                tracks: Vec::new(),
                albums: Vec::new(),
                artists: Vec::new(),
                playlists: Vec::new(),
            },
            lib_dirty: true,
            scan_busy: false,
            download_busy: false,
            play_started_at: None,
            play_open_id: None,
            frames: 0,
            fps_ema: 60.0,
            last_frame: Instant::now(),
        };

        diag("shell struct built");
        if !folders.is_empty() {
            shell.spawn_scan(folders);
        }
        diag("shell create done");
        Ok(shell)
    }

    fn toast(&mut self, msg: impl Into<String>) {
        push_toast(&mut self.ui, msg.into());
    }
}

// ---------------------------------------------------------------------------
// Library snapshot
// ---------------------------------------------------------------------------

impl OxidifyShell {
    fn refresh_library_view(&mut self) {
        let tracks: Vec<StoredTrack> = self.db.all_tracks(5000, 0).unwrap_or_default();
        let q = self.search_query.trim().to_lowercase();
        let mut ui_tracks: Vec<UiTrack> = tracks
            .iter()
            .filter(|t| {
                q.is_empty()
                    || t.title.to_lowercase().contains(&q)
                    || t.artists.iter().any(|a| a.to_lowercase().contains(&q))
                    || t.album.as_deref().unwrap_or("").to_lowercase().contains(&q)
            })
            .map(stored_to_entry)
            .map(|e| e.to_ui())
            .collect();
        if !q.is_empty() {
            ui_tracks.extend(self.remote_items.iter().map(QueueEntry::to_ui));
        }
        let albums: Vec<UiAlbum> = self
            .db
            .albums()
            .unwrap_or_default()
            .into_iter()
            .map(|(title, artist, count)| UiAlbum {
                title,
                artist,
                track_count: count as usize,
            })
            .collect();
        let artists: Vec<String> = self.db.artists().unwrap_or_default().into_iter().map(|(n, _)| n).collect();
        let playlists: Vec<UiPlaylist> = self
            .db
            .list_playlists()
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                let count = self.db.playlist_items(&p.id).map(|t| t.len()).unwrap_or(0);
                UiPlaylist { id: p.id, title: p.title, count }
            })
            .collect();
        self.lib_view = LibraryView { tracks: ui_tracks, albums, artists, playlists };
        self.lib_dirty = false;
    }

    fn library_entries(&self) -> Vec<QueueEntry> {
        let tracks = self.db.all_tracks(5000, 0).unwrap_or_default();
        let q = self.search_query.trim().to_lowercase();
        let mut out: Vec<QueueEntry> = tracks
            .iter()
            .filter(|t| {
                q.is_empty()
                    || t.title.to_lowercase().contains(&q)
                    || t.artists.iter().any(|a| a.to_lowercase().contains(&q))
            })
            .map(stored_to_entry)
            .collect();
        if !q.is_empty() {
            out.extend(self.remote_items.iter().cloned());
        }
        out
    }
}

fn stored_to_entry(t: &StoredTrack) -> QueueEntry {
    QueueEntry {
        id: t.id.clone(),
        title: t.title.clone(),
        artists: t.artists.join(", "),
        album: t.album.clone().unwrap_or_default(),
        duration_ms: t.duration_ms,
        cover: t.cover_path.clone(),
        origin: Origin::Local(t.path.clone()),
    }
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

impl OxidifyShell {
    fn current_entry(&self) -> Option<&QueueEntry> {
        if self.index < 0 {
            None
        } else {
            self.queue.get(self.index as usize)
        }
    }

    fn repeat_mode(&self) -> RepeatMode {
        match self.repeat {
            1 => RepeatMode::All,
            2 => RepeatMode::One,
            _ => RepeatMode::Off,
        }
    }

    fn play_entry(&mut self, entry: QueueEntry, start_ms: u64) {
        self.finish_open_play();
        match entry.origin.clone() {
            Origin::Local(path) => {
                match self.engine.load_file(&path, start_ms) {
                    Ok(info) => {
                        tracing::info!(codec = %info.codec, "loaded local file");
                        self.engine.play();
                        self.on_started(entry);
                    }
                    Err(e) => {
                        self.toast(format!("Could not play {}: {e}", entry.title));
                    }
                }
            }
            Origin::Remote(track_id) => {
                self.loading = true;
                self.download_busy = true;
                self.current_id = Some(entry.id.clone());
                self.spawn_download(entry, track_id);
            }
        }
    }

    fn on_started(&mut self, entry: QueueEntry) {
        self.loading = false;
        self.current_id = Some(entry.id.clone());
        self.play_started_at = Some(Instant::now());
        self.play_open_id = Some(entry.id.clone());
        self.desktop.notify_track(&entry.title, &entry.artists);
        self.load_lyrics(&entry);
        self.update_aether(&entry);
    }

    fn play_track_by_id(&mut self, id: &str) {
        // Prefer the live queue, then the current library context.
        if let Some(pos) = self.queue.iter().position(|e| e.id == id) {
            self.index = pos as i64;
            let entry = self.queue[pos].clone();
            self.play_entry(entry, 0);
            return;
        }
        let context = self.library_entries();
        if let Some(pos) = context.iter().position(|e| e.id == id) {
            self.queue = context;
            self.index = pos as i64;
            let entry = self.queue[pos].clone();
            self.play_entry(entry, 0);
            return;
        }
        self.toast("Track not found in library.");
    }

    fn toggle_play(&mut self) {
        if self.engine.is_playing() {
            let pos = self.engine.position_ms();
            self.engine.pause();
            let _ = pos;
        } else if self.current_entry().is_some() {
            self.engine.play();
            if self.play_started_at.is_none() {
                self.play_started_at = Some(Instant::now());
            }
        } else if !self.queue.is_empty() {
            self.index = 0;
            let entry = self.queue[0].clone();
            self.play_entry(entry, 0);
        } else {
            let context = self.library_entries();
            if !context.is_empty() {
                self.queue = context;
                self.index = 0;
                let entry = self.queue[0].clone();
                self.play_entry(entry, 0);
            }
        }
    }

    fn step_next(&mut self, auto: bool) {
        self.finish_open_play();
        if self.queue.is_empty() {
            return;
        }
        let cur = (self.index.max(0) as usize).min(self.queue.len() - 1);
        let nxt = next_index(cur, self.queue.len(), self.shuffle, &self.repeat_mode(), rand_below);
        match nxt {
            Some(i) => {
                // Avoid re-choosing the same track on manual shuffle-next.
                let mut i = i;
                if !auto && self.shuffle && self.queue.len() > 1 && i == cur {
                    i = (cur + 1) % self.queue.len();
                }
                self.index = i as i64;
                let entry = self.queue[i].clone();
                self.play_entry(entry, 0);
            }
            None => {
                self.engine.pause();
                self.toast("End of queue.");
            }
        }
    }

    fn step_prev(&mut self) {
        self.finish_open_play();
        if self.queue.is_empty() {
            return;
        }
        let cur = (self.index.max(0) as usize).min(self.queue.len() - 1);
        let pos = self.engine.position_ms();
        if let Some(i) = prev_index(cur, self.queue.len(), pos) {
            self.index = i as i64;
            let entry = self.queue[i].clone();
            self.play_entry(entry, 0);
        }
    }

    fn on_track_ended(&mut self) {
        if self.repeat == 2 {
            // Repeat-one: replay the same entry.
            if let Some(entry) = self.current_entry().cloned() {
                self.play_entry(entry, 0);
                return;
            }
        }
        // Count the finished play, then advance.
        if let Some(id) = self.play_open_id.take() {
            let duration = self
                .engine
                .duration_ms()
                .or_else(|| self.current_entry().map(|e| e.duration_ms))
                .unwrap_or(0);
            let _ = self.db.record_play(&id, duration);
            self.play_started_at = None;
        }
        self.step_next(true);
    }

    /// Close out the open play for history when the user moves on early.
    fn finish_open_play(&mut self) {
        if let (Some(id), Some(t0)) = (self.play_open_id.take(), self.play_started_at.take()) {
            let played_ms = t0.elapsed().as_millis() as u64;
            if played_ms > 30_000 {
                let _ = self.db.record_play(&id, played_ms);
            }
        }
    }

    fn handle_engine_events(&mut self) {
        for ev in self.engine.poll_events() {
            match ev {
                EngineEvent::TrackEnded => self.on_track_ended(),
                EngineEvent::Stalled(b) => self.loading = b,
                EngineEvent::Error(msg) => {
                    self.loading = false;
                    self.toast(format!("Playback error: {msg}"));
                }
                EngineEvent::PositionMs(_) => {}
            }
        }
    }
}

fn rand_below(n: usize) -> usize {
    let x = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as usize)
        .unwrap_or(0x9E37);
    (x ^ (x >> 17) ^ (x >> 31)) % n.max(1)
}

// ---------------------------------------------------------------------------
// Lyrics
// ---------------------------------------------------------------------------

impl OxidifyShell {
    fn load_lyrics(&mut self, entry: &QueueEntry) {
        self.lyrics_lines.clear();
        self.lyrics_synced = false;
        match &entry.origin {
            Origin::Local(path) => {
                let lrc = path.with_extension("lrc");
                let lrc_up = path.with_extension("LRC");
                let candidate = [lrc, lrc_up].into_iter().find(|p| p.exists());
                if let Some(p) = candidate {
                    match std::fs::read_to_string(&p) {
                        Ok(text) => {
                            let lines = parse_lrc(&text, entry.duration_ms);
                            if !lines.is_empty() {
                                self.lyrics_synced =
                                    lines.iter().any(|l| l.time_ms > 0 || l.words.is_some());
                                self.lyrics_lines = lines;
                            }
                        }
                        Err(e) => tracing::warn!("lyric file unreadable: {e}"),
                    }
                }
            }
            Origin::Remote(track_id) => {
                if let Some(client) = self.remote.clone() {
                    let id = track_id.clone();
                    let tx = self.bg_tx.clone();
                    let handle = self.rt.clone();
                    std::thread::spawn(move || {
                        let frames = handle.block_on(client.lyric_frames(&id));
                        match frames {
                            Ok(frames) => {
                                let (lines, synced) = frames_to_lines(&frames);
                                let _ = tx.send(BgDone::LyricsDone { id, lines, synced });
                            }
                            Err(e) => tracing::warn!("lyrics fetch failed: {e}"),
                        }
                    });
                }
            }
        }
    }
}

/// Best-effort conversion of generic lyric payloads into timed lines.
fn frames_to_lines(frames: &[serde_json::Value]) -> (Vec<SyncedLine>, bool) {
    use oxidify_core::{SyncedLine, WordData};
    let mut timed: Vec<(u64, String)> = Vec::new();
    let mut plain: Vec<String> = Vec::new();
    for f in frames {
        collect_lyric_text(f, &mut timed, &mut plain);
    }
    if timed.is_empty() {
        let lines = plain
            .into_iter()
            .map(|text| SyncedLine {
                time_ms: 0,
                duration_ms: None,
                text,
                is_instrumental: false,
                words: None,
            })
            .collect();
        return (lines, false);
    }
    timed.sort_by_key(|(ms, _)| *ms);
    let lines = timed
        .into_iter()
        .map(|(time_ms, text)| SyncedLine {
            time_ms,
            duration_ms: None,
            text,
            is_instrumental: false,
            words: None::<Vec<WordData>>,
        })
        .collect();
    (lines, true)
}

fn collect_lyric_text(v: &serde_json::Value, timed: &mut Vec<(u64, String)>, plain: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => {
            let s = s.trim();
            if !s.is_empty() && s.len() < 500 {
                plain.push(s.to_string());
            }
        }
        serde_json::Value::Array(a) => {
            for item in a {
                collect_lyric_text(item, timed, plain);
            }
        }
        serde_json::Value::Object(m) => {
            let ms = ["startMs", "start_ms", "startMs_", "ts", "timeMs", "time_ms", "time"]
                .iter()
                .find_map(|k| m.get(*k))
                .and_then(json_to_ms);
            let text = ["text", "words", "content", "c", "line"]
                .iter()
                .find_map(|k| m.get(*k))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if let Some(t) = text {
                if let Some(ms) = ms {
                    timed.push((ms, t.to_string()));
                } else {
                    // Recurse: timed words may hide deeper.
                    let mut sub_timed = Vec::new();
                    let mut sub_plain = Vec::new();
                    for val in m.values() {
                        collect_lyric_text(val, &mut sub_timed, &mut sub_plain);
                    }
                    timed.extend(sub_timed);
                    if sub_plain.is_empty() {
                        plain.push(t.to_string());
                    } else {
                        plain.extend(sub_plain);
                    }
                }
            } else {
                for val in m.values() {
                    collect_lyric_text(val, timed, plain);
                }
            }
        }
        _ => {}
    }
}

fn json_to_ms(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                // Heuristic: values < 10_000 are seconds.
                Some(if u < 10_000 { u * 1000 } else { u })
            } else {
                n.as_f64().map(|f| {
                    if f < 10_000.0 {
                        (f * 1000.0) as u64
                    } else {
                        f as u64
                    }
                })
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Aether background
// ---------------------------------------------------------------------------

impl OxidifyShell {
    fn update_aether(&mut self, entry: &QueueEntry) {
        let mut palette = Vec::new();
        if let Some(cover) = entry.cover.clone() {
            palette = cover_palette(&cover, 6);
        }
        self.aether.set_palette(palette);
        self.aether.note_track_change();
    }
}

fn cover_palette(path: &Path, max: usize) -> Vec<[u8; 3]> {
    let img = match image::open(path) {
        Ok(i) => i.to_rgb8(),
        Err(_) => return Vec::new(),
    };
    // Downsample to keep quantization cheap.
    let small = image::imageops::resize(&img, 64, 64, image::imageops::FilterType::Triangle);
    let pixels: Vec<[u8; 3]> = small.pixels().map(|p| [p[0], p[1], p[2]]).collect();
    extract_palette_rgba(&pixels, max)
}

// ---------------------------------------------------------------------------
// Background tasks
// ---------------------------------------------------------------------------

impl OxidifyShell {
    fn spawn_scan(&mut self, folders: Vec<PathBuf>) {
        if self.scan_busy {
            return;
        }
        self.scan_busy = true;
        let tx = self.bg_tx.clone();
        let cover_cache = self.cover_cache_dir.clone();
        let db_path = self.db_path();
        std::thread::spawn(move || {
            let msg = match LibraryDb::open(&db_path) {
                Ok(db) => {
                    let mut added = 0;
                    let mut errors = 0;
                    for dir in &folders {
                        match db.scan_directory(dir, &cover_cache) {
                            Ok(r) => {
                                added += r.added + r.updated;
                                errors += r.errors.len();
                            }
                            Err(e) => {
                                errors += 1;
                                tracing::warn!("scan failed for {}: {e}", dir.display());
                            }
                        }
                    }
                    format!("Library updated: {added} tracks ({errors} errors).")
                }
                Err(e) => format!("Library database error: {e}"),
            };
            let _ = tx.send(BgDone::ScanDone(msg));
        });
    }

    fn spawn_download(&mut self, entry: QueueEntry, track_id: String) {
        let Some(client) = self.remote.clone() else {
            self.loading = false;
            self.download_busy = false;
            self.toast("No service configured.");
            return;
        };
        let tx = self.bg_tx.clone();
        let handle = self.rt.clone();
        let dest_dir = self.audio_cache_dir.clone();
        let id = entry.id.clone();
        let send_id = id.clone();
        std::thread::spawn(move || {
            let out = handle.block_on(async move {
                let session = client.playback_session(&track_id, None).await?;
                let ext = match session.codec {
                    Codec::Opus => "opus",
                    Codec::Aac => "m4a",
                };
                let dest = dest_dir.join(format!("{id}.{ext}"));
                let url = session.stream_url.clone();
                client.download_to_file(&url, &dest, None).await?;
                Ok::<_, oxidify_api::ApiError>((dest, session.codec))
            });
            match out {
                Ok((path, codec)) => {
                    let _ = tx.send(BgDone::DownloadDone { id: send_id, path, codec });
                }
                Err(e) => {
                    let _ = tx.send(BgDone::DownloadFailed {
                        id: send_id,
                        message: e.to_string(),
                    });
                }
            }
        });
    }

    fn drain_background(&mut self) {
        let mut done: Vec<BgDone> = Vec::new();
        while let Ok(d) = self.bg_rx.try_recv() {
            done.push(d);
        }
        for d in done {
            match d {
                BgDone::ScanDone(msg) => {
                    self.scan_busy = false;
                    self.lib_dirty = true;
                    self.toast(msg);
                }
                BgDone::DownloadDone { id, path, codec } => {
                    self.download_busy = false;
                    if self.current_id.as_deref() != Some(&id) {
                        continue; // superseded while downloading
                    }
                    let ext = match codec {
                        Codec::Opus => "opus",
                        Codec::Aac => "m4a",
                    };
                    match std::fs::read(&path) {
                        Ok(bytes) => match self.engine.load_bytes(bytes, ext, 0) {
                            Ok(_) => {
                                self.engine.play();
                                if let Some(entry) = self.current_entry().cloned() {
                                    self.on_started(entry);
                                }
                            }
                            Err(e) => {
                                self.loading = false;
                                self.toast(format!("Could not decode stream: {e}"));
                            }
                        },
                        Err(e) => {
                            self.loading = false;
                            self.toast(format!("Cache read failed: {e}"));
                        }
                    }
                }
                BgDone::DownloadFailed { id, message } => {
                    self.download_busy = false;
                    if self.current_id.as_deref() == Some(&id) {
                        self.loading = false;
                        self.toast(format!("Stream failed: {message}"));
                    }
                }
                BgDone::LyricsDone { id, lines, synced } => {
                    if self.current_id.as_deref() == Some(&id) {
                        self.lyrics_lines = lines;
                        self.lyrics_synced = synced;
                    }
                }
                BgDone::LoginDone(res) => match res {
                    Ok(_) => self.toast("Signed in."),
                    Err(e) => self.toast(format!("Sign in failed: {e}")),
                },
                BgDone::RemoteSearchDone { query, items } => {
                    if query == self.search_query && !query.is_empty() {
                        self.remote_items = items;
                        self.lib_dirty = true;
                    }
                }
            }
        }
    }

    fn db_path(&self) -> PathBuf {
        self.db_path.clone()
    }
}

// ---------------------------------------------------------------------------
// UI commands
// ---------------------------------------------------------------------------

impl OxidifyShell {
    fn handle_ui_command(&mut self, cmd: UiCommand) {
        match cmd {
            UiCommand::PlayTrack { id } => self.play_track_by_id(&id),
            UiCommand::TogglePlay => self.toggle_play(),
            UiCommand::Next => self.step_next(false),
            UiCommand::Prev => self.step_prev(),
            UiCommand::SeekMs(ms) => self.engine.seek_ms(ms),
            UiCommand::SetVolume(v) => {
                self.volume = v.clamp(0.0, 1.0);
                self.engine.set_volume(self.volume);
                let _ = self.db.pref_set(PREF_VOLUME, &self.volume.to_string());
            }
            UiCommand::SetShuffle(b) => {
                self.shuffle = b;
                let _ = self.db.pref_set(PREF_SHUFFLE, if b { "1" } else { "0" });
            }
            UiCommand::SetRepeat(r) => {
                self.repeat = r.min(2);
                let _ = self.db.pref_set(PREF_REPEAT, &self.repeat.to_string());
            }
            UiCommand::SetRate(r) => {
                self.rate = r.clamp(0.25, 3.0);
                self.engine.set_rate(self.rate);
                let _ = self.db.pref_set(PREF_RATE, &self.rate.to_string());
            }
            UiCommand::SetPitch { semitones, linked } => {
                self.pitch_semitones = semitones.clamp(-12.0, 12.0);
                self.pitch_linked = linked;
                self.engine.set_pitch(self.pitch_semitones, linked);
                let _ = self.db.pref_set(PREF_PITCH, &self.pitch_semitones.to_string());
                let _ = self.db.pref_set(PREF_LINKED, if linked { "1" } else { "0" });
            }
            UiCommand::SetReverb { enabled, level } => {
                self.reverb_enabled = enabled;
                self.reverb_level = level.clamp(0.0, 1.0);
                self.engine.set_reverb(enabled, self.reverb_level);
                let _ = self.db.pref_set(PREF_REVERB_ON, if enabled { "1" } else { "0" });
                let _ = self.db.pref_set(PREF_REVERB_LVL, &self.reverb_level.to_string());
            }
            UiCommand::SetLyricsOffset(ms) => {
                self.lyrics_offset_ms = ms.clamp(-5000, 5000);
                let _ = self.db.pref_set(PREF_LYRICS_OFFSET, &self.lyrics_offset_ms.to_string());
            }
            UiCommand::AddLibraryFolder => {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    let mut folders: Vec<PathBuf> = self
                        .db
                        .pref_get(PREF_FOLDERS)
                        .unwrap_or(None)
                        .and_then(|v| serde_json::from_str(&v).ok())
                        .unwrap_or_default();
                    if !folders.contains(&dir) {
                        folders.push(dir);
                    }
                    if self.db.pref_set(PREF_FOLDERS, &serde_json::to_string(&folders).unwrap_or_default()).is_ok() {
                        self.spawn_scan(folders);
                    }
                }
            }
            UiCommand::RescanLibrary => {
                let folders: Vec<PathBuf> = self
                    .db
                    .pref_get(PREF_FOLDERS)
                    .unwrap_or(None)
                    .and_then(|v| serde_json::from_str(&v).ok())
                    .unwrap_or_default();
                if folders.is_empty() {
                    self.toast("Add a library folder first.");
                } else {
                    self.spawn_scan(folders);
                }
            }
            UiCommand::Search(q) => {
                let changed = q != self.search_query;
                self.search_query = q.clone();
                self.lib_dirty = true;
                if changed && !q.trim().is_empty() {
                    if let Some(client) = self.remote.clone() {
                        let tx = self.bg_tx.clone();
                        let handle = self.rt.clone();
                        std::thread::spawn(move || {
                            let items = handle.block_on(client.search(&q)).map(|v| {
                                remote_search_to_entries(&v)
                            });
                            match items {
                                Ok(items) => {
                                    let _ = tx.send(BgDone::RemoteSearchDone { query: q, items });
                                }
                                Err(e) => tracing::warn!("remote search failed: {e}"),
                            }
                        });
                    }
                }
            }
            UiCommand::CreatePlaylist(title) => {
                let title = title.trim().to_string();
                if title.is_empty() {
                    return;
                }
                match self.db.create_playlist(&title) {
                    Ok(_) => {
                        self.lib_dirty = true;
                        self.toast(format!("Playlist “{title}” created."));
                    }
                    Err(e) => self.toast(format!("Could not create playlist: {e}")),
                }
            }
            UiCommand::DeletePlaylist(id) => {
                if self.db.delete_playlist(&id).is_ok() {
                    self.lib_dirty = true;
                }
            }
            UiCommand::AddToPlaylist { playlist_id, track_id } => {
                match self.db.playlist_add(&playlist_id, &track_id) {
                    Ok(_) => {
                        self.lib_dirty = true;
                        self.toast("Added to playlist.");
                    }
                    Err(e) => self.toast(format!("Could not add: {e}")),
                }
            }
            UiCommand::RemoveFromPlaylist { playlist_id, track_id } => {
                if self.db.playlist_remove(&playlist_id, &track_id).is_ok() {
                    self.lib_dirty = true;
                }
            }
            UiCommand::ConfigureService { base_url } => {
                let base_url = base_url.trim().to_string();
                if base_url.is_empty() {
                    self.remote = None;
                    self.service_url = None;
                    let _ = self.db.pref_set(PREF_SERVICE_URL, "");
                    self.toast("Service disconnected.");
                    return;
                }
                match RemoteClient::new(&base_url) {
                    Ok(c) => {
                        if let Ok(Some(token)) = secrets::load_secret(SERVICE_TOKEN_ACCOUNT) {
                            c.set_bearer(Some(token));
                        }
                        self.remote = Some(c);
                        self.service_url = Some(base_url.clone());
                        let _ = self.db.pref_set(PREF_SERVICE_URL, &base_url);
                        self.toast("Service configured.");
                    }
                    Err(e) => self.toast(format!("Invalid service URL: {e}")),
                }
            }
            UiCommand::Login { email, password } => {
                if let Some(client) = self.remote.clone() {
                    let tx = self.bg_tx.clone();
                    let handle = self.rt.clone();
                    std::thread::spawn(move || {
                        let res = handle.block_on(async move {
                            let tokens = client.login(&email, &password).await?;
                            secrets::store_secret(SERVICE_TOKEN_ACCOUNT, &tokens.access_token)
                                .map_err(|e| oxidify_api::ApiError::Network(e.to_string()))?;
                            Ok::<_, oxidify_api::ApiError>(tokens.user.id.clone())
                        });
                        let _ = tx.send(BgDone::LoginDone(res.map_err(|e| e.to_string())));
                    });
                } else {
                    self.toast("Configure a service first.");
                }
            }
            UiCommand::Logout => {
                let _ = secrets::delete_secret(SERVICE_TOKEN_ACCOUNT);
                if let Some(c) = self.remote.as_mut() {
                    c.set_bearer(None);
                }
                self.toast("Signed out.");
            }
            UiCommand::SelectOutputDevice(name) => {
                let target = if name.is_empty() { None } else { Some(name.as_str()) };
                match self.engine.set_output_device(target) {
                    Ok(_) => {
                        let _ = self.db.pref_set(PREF_DEVICE, &name);
                    }
                    Err(e) => self.toast(format!("Output device error: {e}")),
                }
            }
            UiCommand::Quit => {
                self.finish_open_play();
            }
        }
    }

    fn handle_desktop_command(&mut self, ctx: &egui::Context, cmd: DesktopCommand) {
        match cmd {
            DesktopCommand::Show => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            DesktopCommand::TogglePlay => self.toggle_play(),
            DesktopCommand::Next => self.step_next(false),
            DesktopCommand::Prev => self.step_prev(),
            DesktopCommand::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }
}

fn remote_search_to_entries(v: &serde_json::Value) -> Vec<QueueEntry> {
    let mut out = Vec::new();
    let items = v.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
    for item in items {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("Unknown").to_string();
        let artists = item
            .get("artists")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| {
                        x.as_str().map(str::to_string).or_else(|| {
                            x.get("name").and_then(|n| n.as_str()).map(str::to_string)
                        })
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let album = item
            .get("album")
            .and_then(|v| v.as_str().map(str::to_string))
            .or_else(|| {
                item.get("album")
                    .and_then(|v| v.get("title"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let duration_ms = item
            .get("durationMs")
            .and_then(|v| v.as_u64())
            .or_else(|| item.get("duration_ms").and_then(|v| v.as_u64()))
            .unwrap_or(0);
        out.push(QueueEntry {
            id: id.clone(),
            title,
            artists,
            album,
            duration_ms,
            cover: None,
            origin: Origin::Remote(id),
        });
    }
    out.truncate(50);
    out
}

// ---------------------------------------------------------------------------
// eframe::App
// ---------------------------------------------------------------------------

impl eframe::App for OxidifyShell {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frames += 1;
        if self.frames == 1 || self.frames % 600 == 0 {
            diag(&format!(
                "update frame {} playing={} loading={}",
                self.frames,
                self.engine.is_playing(),
                self.loading
            ));
        }
        let dt = self.last_frame.elapsed().as_secs_f32();
        self.last_frame = Instant::now();
        if dt > 0.0 && dt < 1.0 {
            let fps = 1.0 / dt;
            self.fps_ema += (fps - self.fps_ema) * 0.05;
        }
        self.aether.set_degraded(self.fps_ema < 45.0 && self.frames > 120);

        // Hide-to-tray instead of quitting when the window is closed.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        for cmd in self.desktop.poll() {
            if matches!(cmd, DesktopCommand::Quit) {
                self.finish_open_play();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            self.handle_desktop_command(ctx, cmd);
        }

        self.handle_engine_events();
        self.drain_background();
        if self.lib_dirty {
            self.refresh_library_view();
        }

        let position_ms = self.engine.position_ms();
        let duration_ms = self.engine.duration_ms().unwrap_or_else(|| {
            self.current_entry().map(|e| e.duration_ms).unwrap_or(0)
        });
        let queue_ui: Vec<UiTrack> = self.queue.iter().map(QueueEntry::to_ui).collect();
        let pb = PlaybackView {
            current: self.current_entry().map(QueueEntry::to_ui),
            position_ms,
            duration_ms,
            playing: self.engine.is_playing(),
            loading: self.loading || self.download_busy,
            volume: self.volume,
            rate: self.rate,
            pitch_semitones: self.pitch_semitones,
            pitch_linked: self.pitch_linked,
            reverb_enabled: self.reverb_enabled,
            reverb_level: self.reverb_level,
            queue: queue_ui,
            queue_index: self.index.max(0) as usize,
            shuffle: self.shuffle,
            repeat: self.repeat,
        };

        let shifted = apply_lyrics_offset(&self.lyrics_lines, self.lyrics_offset_ms as i64);
        let active = active_line_index(&shifted, position_ms);
        let lyric_lines: Vec<LyricsLineView> = shifted
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let is_active = Some(i) == active;
                let words = l
                    .words
                    .as_ref()
                    .map(|ws| {
                        ws.iter()
                            .map(|w| (w.text.clone(), word_progress(w, position_ms)))
                            .collect()
                    })
                    .unwrap_or_default();
                LyricsLineView { text: l.text.clone(), active: is_active, words }
            })
            .collect();
        let lyrics = LyricsView {
            lines: lyric_lines,
            synced: self.lyrics_synced,
            offset_ms: self.lyrics_offset_ms,
        };

        // Aether behind everything.
        egui::Area::new(egui::Id::new("aether_bg"))
            .order(egui::Order::Background)
            .interactable(false)
            .fixed_pos(ctx.screen_rect().min)
            .show(ctx, |ui| {
                let r = ctx.screen_rect();
                ui.allocate_space(r.size());
                self.aether.paint(ui, r);
            });

        let commands = show_full_ui(ctx, &mut self.ui, &self.lib_view, &pb, &lyrics);
        let quit = commands.iter().any(|c| matches!(c, UiCommand::Quit));
        for cmd in commands {
            self.handle_ui_command(cmd);
        }
        if quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        if pb.playing || pb.loading || self.scan_busy || self.download_busy {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }
}
