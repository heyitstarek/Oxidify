//! Full Oxidify interface built with egui (original dark design).
//!
//! The shell owns all I/O and state snapshots. Every frame it supplies
//! [`LibraryView`], [`PlaybackView`] and [`LyricsView`]; this crate only
//! paints widgets and returns the [`UiCommand`] values the user triggered.
//! [`show_full_ui`] is side-effect free apart from painting (plus expiring
//! toasts and bumping an internal frame counter).

use oxidify_core::{PlayerStatus, Track};

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Public contract: snapshots in
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct UiTrack {
    pub id: String,
    pub title: String,
    pub artists: String,
    pub album: String,
    pub duration_ms: u64,
    pub cover_path: Option<PathBuf>,
    pub remote: bool,
}

#[derive(Debug, Clone)]
pub struct UiAlbum {
    pub title: String,
    pub artist: String,
    pub track_count: usize,
}

#[derive(Debug, Clone)]
pub struct UiPlaylist {
    pub id: String,
    pub title: String,
    pub count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct LibraryView {
    pub tracks: Vec<UiTrack>,
    pub albums: Vec<UiAlbum>,
    pub artists: Vec<String>,
    pub playlists: Vec<UiPlaylist>,
}

#[derive(Debug, Clone, Default)]
pub struct PlaybackView {
    pub current: Option<UiTrack>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub playing: bool,
    pub loading: bool,
    pub volume: f32,
    pub rate: f32,
    pub pitch_semitones: f32,
    pub pitch_linked: bool,
    pub reverb_enabled: bool,
    pub reverb_level: f32,
    pub queue: Vec<UiTrack>,
    pub queue_index: usize,
    pub shuffle: bool,
    pub repeat: u8,
}

#[derive(Debug, Clone)]
pub struct LyricsLineView {
    pub text: String,
    pub active: bool,
    pub words: Vec<(String, f32)>,
}

#[derive(Debug, Clone, Default)]
pub struct LyricsView {
    pub lines: Vec<LyricsLineView>,
    pub synced: bool,
    pub offset_ms: i32,
}

#[derive(Debug, Clone)]
pub enum UiCommand {
    PlayTrack { id: String },
    TogglePlay,
    Next,
    Prev,
    SeekMs(u64),
    SetVolume(f32),
    SetShuffle(bool),
    SetRepeat(u8),
    SetRate(f32),
    SetPitch { semitones: f32, linked: bool },
    SetReverb { enabled: bool, level: f32 },
    SetLyricsOffset(i32),
    AddLibraryFolder,
    RescanLibrary,
    Search(String),
    CreatePlaylist(String),
    DeletePlaylist(String),
    AddToPlaylist {
        playlist_id: String,
        track_id: String,
    },
    RemoveFromPlaylist {
        playlist_id: String,
        track_id: String,
    },
    ConfigureService { base_url: String },
    Login { email: String, password: String },
    Logout,
    SelectOutputDevice(String),
    Quit,
}

// ---------------------------------------------------------------------------
// Legacy view model (kept for compatibility)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PlayerViewModel {
    pub track_title: String,
    pub artists: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub status: PlayerStatus,
}

impl PlayerViewModel {
    pub fn from_track(track: Option<&Track>, position_ms: u64, status: PlayerStatus) -> Self {
        Self {
            track_title: track.map(|t| t.title.clone()).unwrap_or_default(),
            artists: track.map(|t| t.artist_display()).unwrap_or_default(),
            position_ms,
            duration_ms: track.map(|t| t.duration_ms).unwrap_or(0),
            status,
        }
    }

    pub fn progress(&self) -> f32 {
        progress_ratio(self.position_ms, self.duration_ms)
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (all unit-tested below)
// ---------------------------------------------------------------------------

/// Format milliseconds as `m:ss`, or `h:mm:ss` once an hour is reached.
pub fn format_duration_ms(ms: u64) -> String {
    let total_s = ms / 1000;
    let h = total_s / 3600;
    let m = (total_s % 3600) / 60;
    let s = total_s % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Playback progress 0.0..=1.0; zero duration yields 0.0 instead of NaN.
pub fn progress_ratio(position_ms: u64, duration_ms: u64) -> f32 {
    if duration_ms == 0 {
        0.0
    } else {
        (position_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
    }
}

/// Case-insensitive substring match; an empty query matches everything.
pub fn matches_query(haystack: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    haystack.to_lowercase().contains(&q.to_lowercase())
}

/// Filter tracks by title / artists / album / id.
pub fn filter_tracks<'a>(tracks: &'a [UiTrack], query: &str) -> Vec<&'a UiTrack> {
    if query.trim().is_empty() {
        return tracks.iter().collect();
    }
    tracks
        .iter()
        .filter(|t| {
            matches_query(&t.title, query)
                || matches_query(&t.artists, query)
                || matches_query(&t.album, query)
                || matches_query(&t.id, query)
        })
        .collect()
}

/// Cycle repeat mode Off(0) -> All(1) -> One(2) -> Off.
pub fn cycle_repeat_mode(current: u8) -> u8 {
    match current {
        0 => 1,
        1 => 2,
        _ => 0,
    }
}

/// Short human label for a repeat mode value.
pub fn repeat_label(mode: u8) -> &'static str {
    match mode {
        1 => "All",
        2 => "One",
        _ => "Off",
    }
}

/// True once `ttl` has fully elapsed since `born`.
pub fn toast_expired(born: Instant, now: Instant, ttl: Duration) -> bool {
    now.duration_since(born) >= ttl
}

/// Mix two colors by `t` (0.0 = `a`, 1.0 = `b`).
pub fn mix_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    egui::Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

// ---------------------------------------------------------------------------
// Local UI state
// ---------------------------------------------------------------------------

const TOAST_TTL: Duration = Duration::from_secs(4);
const DEFAULT_ACCENT: (u8, u8, u8) = (64, 190, 150);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Section {
    #[default]
    Home,
    Search,
    Library,
    Playlists,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LibraryTab {
    #[default]
    Tracks,
    Albums,
    Artists,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SortKey {
    #[default]
    Title,
    Artist,
    Album,
    Duration,
}

#[derive(Debug, Clone)]
struct Toast {
    message: String,
    born: Instant,
}

pub struct UiState {
    section: Section,
    search_query: String,
    library_tab: LibraryTab,
    sort_key: SortKey,
    sort_ascending: bool,
    selected_playlist: Option<String>,
    add_pick: Option<String>,
    new_playlist_name: String,
    service_url: String,
    service_configured: bool,
    logged_in: bool,
    login_email: String,
    login_password: String,
    output_device: String,
    output_device_options: Vec<String>,
    accent: egui::Color32,
    fullscreen: bool,
    queue_open: bool,
    pre_mute_volume: f32,
    playlist_members: HashMap<String, Vec<String>>,
    toasts: Vec<Toast>,
    frame: u64,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            section: Section::Home,
            search_query: String::new(),
            library_tab: LibraryTab::Tracks,
            sort_key: SortKey::Title,
            sort_ascending: true,
            selected_playlist: None,
            add_pick: None,
            new_playlist_name: String::new(),
            service_url: String::new(),
            service_configured: false,
            logged_in: false,
            login_email: String::new(),
            login_password: String::new(),
            output_device: String::new(),
            output_device_options: Vec::new(),
            accent: egui::Color32::from_rgb(
                DEFAULT_ACCENT.0,
                DEFAULT_ACCENT.1,
                DEFAULT_ACCENT.2,
            ),
            fullscreen: false,
            queue_open: true,
            pre_mute_volume: 0.8,
            playlist_members: HashMap::new(),
            toasts: Vec::new(),
            frame: 0,
        }
    }

    fn prune_toasts(&mut self) {
        let now = Instant::now();
        self.toasts.retain(|t| !toast_expired(t.born, now, TOAST_TTL));
    }
}

/// Queue a transient message shown in the toast stack.
pub fn push_toast(state: &mut UiState, msg: String) {
    state.toasts.push(Toast {
        message: msg,
        born: Instant::now(),
    });
    if state.toasts.len() > 5 {
        let excess = state.toasts.len() - 5;
        state.toasts.drain(..excess);
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn show_full_ui(
    ctx: &egui::Context,
    state: &mut UiState,
    lib: &LibraryView,
    pb: &PlaybackView,
    lyrics: &LyricsView,
) -> Vec<UiCommand> {
    state.frame = state.frame.wrapping_add(1);
    state.prune_toasts();

    let mut out = Vec::new();
    handle_shortcuts(ctx, state, pb, &mut out);

    if state.fullscreen {
        render_fullscreen(ctx, state, pb, lyrics, &mut out);
    } else {
        render_sidebar(ctx, state, lib);
        render_queue_panel(ctx, state, pb, &mut out);
        render_player_bar(ctx, state, pb, &mut out);
        render_center(ctx, state, lib, pb, lyrics, &mut out);
    }
    render_toasts(ctx, state);

    if pb.playing || !state.toasts.is_empty() {
        ctx.request_repaint_after(Duration::from_millis(300));
    }
    out
}

// ---------------------------------------------------------------------------
// Keyboard shortcuts
// ---------------------------------------------------------------------------

fn handle_shortcuts(
    ctx: &egui::Context,
    state: &mut UiState,
    pb: &PlaybackView,
    out: &mut Vec<UiCommand>,
) {
    // Never steal keys while the user is typing in a field.
    if ctx.wants_keyboard_input() {
        return;
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space)) {
        out.push(UiCommand::TogglePlay);
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)) {
        out.push(UiCommand::SeekMs(pb.position_ms.saturating_sub(5000)));
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)) {
        out.push(UiCommand::SeekMs(
            pb.position_ms.saturating_add(5000).min(pb.duration_ms),
        ));
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::N)) {
        out.push(UiCommand::Next);
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::P)) {
        out.push(UiCommand::Prev);
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F)) {
        state.fullscreen = !state.fullscreen;
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::M)) {
        if pb.volume > 0.001 {
            state.pre_mute_volume = pb.volume;
            out.push(UiCommand::SetVolume(0.0));
        } else {
            out.push(UiCommand::SetVolume(state.pre_mute_volume));
        }
    }
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

fn render_sidebar(ctx: &egui::Context, state: &mut UiState, lib: &LibraryView) {
    egui::SidePanel::left("oxidify_nav")
        .default_width(216.0)
        .resizable(true)
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.heading("Oxidify");
                ui.add_space(4.0);

                let items = [
                    (Section::Home, "Home"),
                    (Section::Search, "Search"),
                    (Section::Library, "Library"),
                    (Section::Playlists, "Playlists"),
                    (Section::Settings, "Settings"),
                ];
                for (section, label) in items {
                    if ui
                        .selectable_label(state.section == section, label)
                        .clicked()
                    {
                        state.section = section;
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.small("Playlists");
                if lib.playlists.is_empty() {
                    ui.small("No playlists yet.");
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for pl in &lib.playlists {
                                let selected = state.selected_playlist.as_deref() == Some(&pl.id);
                                let label = format!("{} ({})", pl.title, pl.count);
                                if ui.selectable_label(selected, label).clicked() {
                                    state.selected_playlist = Some(pl.id.clone());
                                    state.section = Section::Playlists;
                                }
                            }
                        });
                }

                ui.add_space(8.0);
                ui.separator();

                // Service status dot.
                let (dot, status) = if state.logged_in {
                    (egui::Color32::from_rgb(80, 200, 120), "Signed in")
                } else if state.service_configured {
                    (egui::Color32::from_rgb(220, 170, 60), "Service set")
                } else {
                    (egui::Color32::from_rgb(110, 114, 128), "Local only")
                };
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 5.0, dot);
                    ui.small(status);
                });
            });
        });
}

// ---------------------------------------------------------------------------
// Center content
// ---------------------------------------------------------------------------

fn render_center(
    ctx: &egui::Context,
    state: &mut UiState,
    lib: &LibraryView,
    pb: &PlaybackView,
    lyrics: &LyricsView,
    out: &mut Vec<UiCommand>,
) {
    egui::CentralPanel::default().show(ctx, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| match state.section {
            Section::Home => render_home(ui, state, lib, pb, out),
            Section::Search => render_search(ui, state, lib, out),
            Section::Library => render_library(ui, state, lib, out),
            Section::Playlists => render_playlists(ui, state, lib, out),
            Section::Settings => render_settings(ui, state, pb, lyrics, out),
        });
    });
}

fn empty_library_hint(ui: &mut egui::Ui, out: &mut Vec<UiCommand>) {
    ui.label("Your library is empty.");
    ui.small("Add a folder with your audio files, then rescan to get started.");
    if ui.button("Add library folder").clicked() {
        out.push(UiCommand::AddLibraryFolder);
    }
}

fn render_home(
    ui: &mut egui::Ui,
    state: &mut UiState,
    lib: &LibraryView,
    pb: &PlaybackView,
    out: &mut Vec<UiCommand>,
) {
    ui.heading("Home");
    ui.add_space(4.0);

    if lib.tracks.is_empty() {
        empty_library_hint(ui, out);
        return;
    }

    ui.small("Quick resume");
    match &pb.current {
        Some(cur) => {
            ui.horizontal(|ui| {
                cover_placeholder(ui, &cur.title, 44.0, state.accent);
                ui.vertical(|ui| {
                    ui.strong(&cur.title);
                    ui.small(&cur.artists);
                });
            });
            if ui.button(if pb.playing { "Pause" } else { "Resume" }).clicked() {
                out.push(UiCommand::TogglePlay);
            }
        }
        None => {
            ui.label("Nothing playing yet.");
            if let Some(first) = lib.tracks.first() {
                let id = first.id.clone();
                if ui.button(format!("Play {}", first.title)).clicked() {
                    out.push(UiCommand::PlayTrack { id });
                }
            }
        }
    }

    ui.add_space(8.0);
    ui.separator();
    ui.small("Recent");
    egui::Grid::new("home_recent")
        .striped(true)
        .show(ui, |ui| {
            for t in lib.tracks.iter().take(6) {
                cover_placeholder(ui, &t.title, 30.0, state.accent);
                ui.label(&t.title);
                ui.small(&t.artists);
                let id = t.id.clone();
                if ui.small_button("Play").clicked() {
                    out.push(UiCommand::PlayTrack { id });
                }
                ui.end_row();
            }
        });
}

fn render_search(
    ui: &mut egui::Ui,
    state: &mut UiState,
    lib: &LibraryView,
    out: &mut Vec<UiCommand>,
) {
    ui.heading("Search");
    let resp = ui.text_edit_singleline(&mut state.search_query);
    if resp.changed() {
        out.push(UiCommand::Search(state.search_query.clone()));
    }
    ui.add_space(4.0);

    if lib.tracks.is_empty() {
        empty_library_hint(ui, out);
        return;
    }
    if state.search_query.trim().is_empty() {
        ui.small("Type above to search tracks, albums, artists and playlists.");
        return;
    }

    let tracks = filter_tracks(&lib.tracks, &state.search_query);
    ui.small(format!("Tracks ({})", tracks.len()));
    if tracks.is_empty() {
        ui.small("No matching tracks.");
    } else {
        for t in tracks.iter().take(8) {
            ui.horizontal(|ui| {
                ui.label(&t.title);
                ui.small(&t.artists);
                let id = t.id.clone();
                if ui.small_button("Play").clicked() {
                    out.push(UiCommand::PlayTrack { id });
                }
            });
        }
        if tracks.len() > 8 {
            ui.small(format!("…and {} more", tracks.len() - 8));
        }
    }

    ui.add_space(4.0);
    ui.small("Albums");
    let mut shown = 0;
    for a in &lib.albums {
        if matches_query(&a.title, &state.search_query)
            || matches_query(&a.artist, &state.search_query)
        {
            ui.label(format!("{} — {} ({} tracks)", a.title, a.artist, a.track_count));
            shown += 1;
            if shown >= 8 {
                break;
            }
        }
    }
    if shown == 0 {
        ui.small("No matching albums.");
    }

    ui.add_space(4.0);
    ui.small("Artists");
    let mut shown = 0;
    for a in &lib.artists {
        if matches_query(a, &state.search_query) {
            ui.label(a);
            shown += 1;
            if shown >= 8 {
                break;
            }
        }
    }
    if shown == 0 {
        ui.small("No matching artists.");
    }

    ui.add_space(4.0);
    ui.small("Playlists");
    let mut shown = 0;
    for p in &lib.playlists {
        if matches_query(&p.title, &state.search_query) {
            ui.label(format!("{} ({})", p.title, p.count));
            shown += 1;
            if shown >= 8 {
                break;
            }
        }
    }
    if shown == 0 {
        ui.small("No matching playlists.");
    }
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

fn sorted_track_indices(tracks: &[UiTrack], key: SortKey, ascending: bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..tracks.len()).collect();
    idx.sort_by(|&a, &b| {
        let ord = match key {
            SortKey::Title => tracks[a].title.cmp(&tracks[b].title),
            SortKey::Artist => tracks[a].artists.cmp(&tracks[b].artists),
            SortKey::Album => tracks[a].album.cmp(&tracks[b].album),
            SortKey::Duration => tracks[a].duration_ms.cmp(&tracks[b].duration_ms),
        };
        if ascending { ord } else { ord.reverse() }
    });
    idx
}

fn render_library(
    ui: &mut egui::Ui,
    state: &mut UiState,
    lib: &LibraryView,
    out: &mut Vec<UiCommand>,
) {
    ui.heading("Library");
    ui.horizontal(|ui| {
        for (tab, label) in [
            (LibraryTab::Tracks, "Tracks"),
            (LibraryTab::Albums, "Albums"),
            (LibraryTab::Artists, "Artists"),
        ] {
            if ui.selectable_label(state.library_tab == tab, label).clicked() {
                state.library_tab = tab;
            }
        }
    });
    ui.add_space(4.0);

    if lib.tracks.is_empty() && lib.albums.is_empty() && lib.artists.is_empty() {
        empty_library_hint(ui, out);
        return;
    }

    match state.library_tab {
        LibraryTab::Tracks => render_track_table(ui, state, &lib.tracks, out),
        LibraryTab::Albums => {
            if lib.albums.is_empty() {
                ui.small("No albums in the library yet.");
            } else {
                egui::Grid::new("album_grid").striped(true).show(ui, |ui| {
                    ui.small("Title");
                    ui.small("Artist");
                    ui.small("Tracks");
                    ui.end_row();
                    for a in &lib.albums {
                        ui.label(&a.title);
                        ui.small(&a.artist);
                        ui.small(a.track_count.to_string());
                        ui.end_row();
                    }
                });
            }
        }
        LibraryTab::Artists => {
            if lib.artists.is_empty() {
                ui.small("No artists in the library yet.");
            } else {
                for a in &lib.artists {
                    ui.label(a);
                }
            }
        }
    }
}

fn render_track_table(
    ui: &mut egui::Ui,
    state: &mut UiState,
    tracks: &[UiTrack],
    out: &mut Vec<UiCommand>,
) {
    if tracks.is_empty() {
        ui.small("No tracks in the library yet.");
        return;
    }

    let arrow = if state.sort_ascending { " ▲" } else { " ▼" };
    let mut header_clicked: Option<SortKey> = None;
    egui::Grid::new("track_table").striped(true).show(ui, |ui| {
        ui.small("#");
        for (key, name) in [
            (SortKey::Title, "Title"),
            (SortKey::Artist, "Artist"),
            (SortKey::Album, "Album"),
            (SortKey::Duration, "Time"),
        ] {
            let label = if state.sort_key == key {
                format!("{name}{arrow}")
            } else {
                name.to_string()
            };
            if ui.small_button(label).clicked() {
                header_clicked = Some(key);
            }
        }
        ui.small("");
        ui.end_row();

        let order = sorted_track_indices(tracks, state.sort_key, state.sort_ascending);
        for (n, i) in order.iter().enumerate() {
            let t = &tracks[*i];
            ui.small(n.to_string());
            let title_resp = ui.label(&t.title);
            if title_resp.double_clicked() {
                out.push(UiCommand::PlayTrack { id: t.id.clone() });
            }
            ui.small(&t.artists);
            ui.small(&t.album);
            ui.small(format_duration_ms(t.duration_ms));
            let play_id = t.id.clone();
            let tag = if t.remote { "Play · web" } else { "Play" };
            if ui.small_button(tag).clicked() {
                out.push(UiCommand::PlayTrack { id: play_id });
            }
            ui.end_row();
        }
    });

    if let Some(key) = header_clicked {
        if state.sort_key == key {
            state.sort_ascending = !state.sort_ascending;
        } else {
            state.sort_key = key;
            state.sort_ascending = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Playlists
// ---------------------------------------------------------------------------

fn render_playlists(
    ui: &mut egui::Ui,
    state: &mut UiState,
    lib: &LibraryView,
    out: &mut Vec<UiCommand>,
) {
    ui.heading("Playlists");

    ui.horizontal(|ui| {
        let resp = ui.text_edit_singleline(&mut state.new_playlist_name);
        let _ = resp;
        if ui.button("Create").clicked() && !state.new_playlist_name.trim().is_empty() {
            let title = state.new_playlist_name.trim().to_string();
            state.new_playlist_name.clear();
            out.push(UiCommand::CreatePlaylist(title));
        }
    });
    ui.add_space(4.0);

    if lib.playlists.is_empty() {
        ui.small("No playlists yet — create one above.");
        return;
    }

    if state
        .selected_playlist
        .as_ref()
        .is_none_or(|id| !lib.playlists.iter().any(|p| &p.id == id))
    {
        state.selected_playlist = lib.playlists.first().map(|p| p.id.clone());
    }
    let selected = state.selected_playlist.clone().unwrap_or_default();

    ui.horizontal(|ui| {
        for pl in &lib.playlists {
            let is_sel = pl.id == selected;
            if ui
                .selectable_label(is_sel, format!("{} ({})", pl.title, pl.count))
                .clicked()
            {
                state.selected_playlist = Some(pl.id.clone());
            }
        }
    });
    ui.add_space(4.0);

    if let Some(pl) = lib.playlists.iter().find(|p| p.id == selected) {
        ui.strong(&pl.title);
        let del_id = pl.id.clone();
        if ui.small_button("Delete playlist").clicked() {
            state.playlist_members.remove(&del_id);
            state.selected_playlist = None;
            out.push(UiCommand::DeletePlaylist(del_id));
            return;
        }

        let members = state.playlist_members.entry(selected.clone()).or_default();
        // Resolve member ids against the current library snapshot.
        let resolved: Vec<UiTrack> = members
            .iter()
            .filter_map(|id| lib.tracks.iter().find(|t| &t.id == id).cloned())
            .collect();
        ui.small(format!("{} track(s)", resolved.len()));
        if resolved.is_empty() {
            ui.small("This playlist is empty — add tracks below.");
        } else {
            for t in &resolved {
                ui.horizontal(|ui| {
                    ui.label(&t.title);
                    ui.small(&t.artists);
                    let rid = t.id.clone();
                    if ui.small_button("Remove").clicked() {
                        if let Some(list) = state.playlist_members.get_mut(&selected) {
                            list.retain(|id| id != &rid);
                        }
                        out.push(UiCommand::RemoveFromPlaylist {
                            playlist_id: selected.clone(),
                            track_id: rid,
                        });
                    }
                });
            }
        }

        ui.add_space(4.0);
        ui.small("Add a track");
        if lib.tracks.is_empty() {
            ui.small("Library is empty — nothing to add.");
        } else {
            if state
                .add_pick
                .as_ref()
                .is_none_or(|id| !lib.tracks.iter().any(|t| &t.id == id))
            {
                state.add_pick = lib.tracks.first().map(|t| t.id.clone());
            }
            ui.horizontal(|ui| {
                let current_title = state
                    .add_pick
                    .as_ref()
                    .and_then(|id| lib.tracks.iter().find(|t| &t.id == id))
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                egui::ComboBox::from_label("")
                    .selected_text(current_title)
                    .show_ui(ui, |ui| {
                        for t in &lib.tracks {
                            ui.selectable_value(
                                &mut state.add_pick,
                                Some(t.id.clone()),
                                format!("{} — {}", t.title, t.artists),
                            );
                        }
                    });
                if ui.button("Add").clicked() {
                    if let Some(track_id) = state.add_pick.clone() {
                        state
                            .playlist_members
                            .entry(selected.clone())
                            .or_default()
                            .push(track_id.clone());
                        // De-duplicate optimistic membership.
                        if let Some(list) = state.playlist_members.get_mut(&selected) {
                            list.sort();
                            list.dedup();
                        }
                        out.push(UiCommand::AddToPlaylist {
                            playlist_id: selected.clone(),
                            track_id,
                        });
                    }
                }
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_settings(
    ui: &mut egui::Ui,
    state: &mut UiState,
    pb: &PlaybackView,
    lyrics: &LyricsView,
    out: &mut Vec<UiCommand>,
) {
    ui.heading("Settings");

    ui.small("Library");
    ui.horizontal(|ui| {
        if ui.button("Add library folder").clicked() {
            out.push(UiCommand::AddLibraryFolder);
        }
        if ui.button("Rescan library").clicked() {
            out.push(UiCommand::RescanLibrary);
        }
    });

    ui.add_space(8.0);
    ui.separator();
    ui.small("Playback");
    let mut rate = pb.rate;
    if ui
        .add(egui::Slider::new(&mut rate, 0.5..=2.0).text("Speed"))
        .changed()
    {
        out.push(UiCommand::SetRate(rate));
    }
    let mut linked = pb.pitch_linked;
    let mut semi = pb.pitch_semitones;
    let pitch_a = ui.checkbox(&mut linked, "Link pitch to speed").changed();
    let pitch_b = ui
        .add(egui::Slider::new(&mut semi, -12.0..=12.0).text("Pitch (semitones)"))
        .changed();
    if pitch_a || pitch_b {
        out.push(UiCommand::SetPitch {
            semitones: semi,
            linked,
        });
    }
    let mut rev_on = pb.reverb_enabled;
    let mut rev_level = pb.reverb_level;
    let rev_a = ui.checkbox(&mut rev_on, "Room echo").changed();
    let rev_b = ui
        .add(egui::Slider::new(&mut rev_level, 0.0..=1.0).text("Echo amount"))
        .changed();
    if rev_a || rev_b {
        out.push(UiCommand::SetReverb {
            enabled: rev_on,
            level: rev_level,
        });
    }
    let mut vol = pb.volume;
    if ui
        .add(egui::Slider::new(&mut vol, 0.0..=1.0).text("Volume"))
        .changed()
    {
        out.push(UiCommand::SetVolume(vol));
    }

    ui.add_space(8.0);
    ui.separator();
    ui.small("Words timing");
    ui.horizontal(|ui| {
        if ui.small_button("−100ms").clicked() {
            out.push(UiCommand::SetLyricsOffset(lyrics.offset_ms - 100));
        }
        let mut off = lyrics.offset_ms as f32;
        if ui
            .add(egui::Slider::new(&mut off, -5000.0..=5000.0).text("Offset"))
            .changed()
        {
            out.push(UiCommand::SetLyricsOffset(off as i32));
        }
        if ui.small_button("+100ms").clicked() {
            out.push(UiCommand::SetLyricsOffset(lyrics.offset_ms + 100));
        }
        if ui.small_button("Reset").clicked() {
            out.push(UiCommand::SetLyricsOffset(0));
        }
    });

    ui.add_space(8.0);
    ui.separator();
    ui.small("Remote service");
    ui.horizontal(|ui| {
        ui.text_edit_singleline(&mut state.service_url);
        if ui.button("Save address").clicked() {
            let base_url = state.service_url.trim().to_string();
            if !base_url.is_empty() {
                state.service_configured = true;
                out.push(UiCommand::ConfigureService { base_url });
            }
        }
    });
    if state.logged_in {
        ui.small(format!(
            "Signed in as {}",
            if state.login_email.is_empty() {
                "unknown"
            } else {
                &state.login_email
            }
        ));
        if ui.button("Sign out").clicked() {
            state.logged_in = false;
            out.push(UiCommand::Logout);
        }
    } else {
        ui.text_edit_singleline(&mut state.login_email);
        ui.add(egui::TextEdit::singleline(&mut state.login_password).password(true));
        if ui.button("Sign in").clicked() {
            state.logged_in = true;
            out.push(UiCommand::Login {
                email: state.login_email.clone(),
                password: state.login_password.clone(),
            });
        }
    }

    ui.add_space(8.0);
    ui.separator();
    ui.small("Output device");
    ui.horizontal(|ui| {
        ui.text_edit_singleline(&mut state.output_device);
        if ui.button("Use device").clicked() && !state.output_device.trim().is_empty() {
            let dev = state.output_device.trim().to_string();
            if !state.output_device_options.contains(&dev) {
                state.output_device_options.push(dev.clone());
            }
            out.push(UiCommand::SelectOutputDevice(dev));
        }
    });
    for dev in state.output_device_options.clone() {
        let is_sel = state.output_device == dev;
        if ui.selectable_label(is_sel, &dev).clicked() {
            state.output_device = dev.clone();
            out.push(UiCommand::SelectOutputDevice(dev));
        }
    }

    ui.add_space(8.0);
    ui.separator();
    ui.small("Accent color");
    ui.horizontal(|ui| {
        let mut rgb = [state.accent.r(), state.accent.g(), state.accent.b()];
        if egui::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
            state.accent = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        }
        if ui.small_button("Reset").clicked() {
            state.accent = egui::Color32::from_rgb(
                DEFAULT_ACCENT.0,
                DEFAULT_ACCENT.1,
                DEFAULT_ACCENT.2,
            );
        }
    });

    ui.add_space(8.0);
    ui.separator();
    ui.small("About");
    ui.small("Oxidify — a native Rust desktop music player.");
    ui.small("Shortcuts: Space play/pause · ←/→ seek 5s · N/P track · F full view · M mute.");
    if ui.button("Quit").clicked() {
        out.push(UiCommand::Quit);
    }
}

// ---------------------------------------------------------------------------
// Queue panel
// ---------------------------------------------------------------------------

fn render_queue_panel(
    ctx: &egui::Context,
    state: &mut UiState,
    pb: &PlaybackView,
    out: &mut Vec<UiCommand>,
) {
    if !state.queue_open {
        return;
    }
    egui::SidePanel::right("oxidify_queue")
        .default_width(240.0)
        .resizable(true)
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.heading("Up next");
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui.small_button("Hide").clicked() {
                                state.queue_open = false;
                            }
                        },
                    );
                });
                ui.small(format!(
                    "{} of {}",
                    if pb.queue.is_empty() {
                        0
                    } else {
                        pb.queue_index + 1
                    },
                    pb.queue.len()
                ));
                if pb.queue.is_empty() {
                    ui.small("The queue is empty — play something first.");
                    return;
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, t) in pb.queue.iter().enumerate() {
                        let is_current = i == pb.queue_index;
                        let label = format!(
                            "{}{} — {}",
                            if is_current { "▶ " } else { "" },
                            t.title,
                            t.artists
                        );
                        let resp = if is_current {
                            ui.selectable_label(
                                true,
                                egui::RichText::new(label).color(state.accent).strong(),
                            )
                        } else {
                            ui.selectable_label(false, label)
                        };
                        if resp.clicked() {
                            out.push(UiCommand::PlayTrack { id: t.id.clone() });
                        }
                    }
                });
            });
        });
}

// ---------------------------------------------------------------------------
// Bottom player bar
// ---------------------------------------------------------------------------

fn render_player_bar(
    ctx: &egui::Context,
    state: &mut UiState,
    pb: &PlaybackView,
    out: &mut Vec<UiCommand>,
) {
    egui::TopBottomPanel::bottom("oxidify_player_bar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            match &pb.current {
                Some(cur) => {
                    cover_placeholder(ui, &cur.title, 46.0, state.accent);
                    ui.vertical(|ui| {
                        ui.strong(&cur.title);
                        ui.small(&cur.artists);
                    });
                }
                None => {
                    cover_placeholder(ui, "?", 46.0, state.accent);
                    ui.vertical(|ui| {
                        ui.strong("Nothing playing");
                        ui.small("Pick a track to start");
                    });
                }
            }

            if ui.small_button("⏮").clicked() {
                out.push(UiCommand::Prev);
            }
            let play_label = if pb.playing { "⏸" } else { "▶" };
            if ui
                .add_enabled(!pb.loading, egui::Button::new(play_label))
                .clicked()
            {
                out.push(UiCommand::TogglePlay);
            }
            if ui.small_button("⏭").clicked() {
                out.push(UiCommand::Next);
            }
            if pb.loading {
                ui.small("Loading…");
            }

            ui.small(format_duration_ms(pb.position_ms));
            let mut pos = pb.position_ms as f32;
            let max = pb.duration_ms.max(1) as f32;
            if ui
                .add(
                    egui::Slider::new(&mut pos, 0.0..=max)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .changed()
            {
                out.push(UiCommand::SeekMs(pos as u64));
            }
            ui.small(format_duration_ms(pb.duration_ms));

            let mut vol = pb.volume;
            if ui
                .add(
                    egui::Slider::new(&mut vol, 0.0..=1.0)
                        .text("Vol")
                        .show_value(false),
                )
                .changed()
            {
                out.push(UiCommand::SetVolume(vol));
            }
            if ui
                .small_button(if pb.volume <= 0.001 { "Unmute" } else { "Mute" })
                .clicked()
            {
                if pb.volume > 0.001 {
                    state.pre_mute_volume = pb.volume;
                    out.push(UiCommand::SetVolume(0.0));
                } else {
                    out.push(UiCommand::SetVolume(state.pre_mute_volume));
                }
            }

            let shuffle_label = if pb.shuffle { "Shuf ●" } else { "Shuf ○" };
            if ui.small_button(shuffle_label).clicked() {
                out.push(UiCommand::SetShuffle(!pb.shuffle));
            }
            if ui
                .small_button(format!("Rep {}", repeat_label(pb.repeat)))
                .clicked()
            {
                out.push(UiCommand::SetRepeat(cycle_repeat_mode(pb.repeat)));
            }
            if ui
                .small_button(if state.queue_open { "Queue ✓" } else { "Queue" })
                .clicked()
            {
                state.queue_open = !state.queue_open;
            }
            if ui.small_button("Full view").clicked() {
                state.fullscreen = true;
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Fullscreen overlay
// ---------------------------------------------------------------------------

fn render_fullscreen(
    ctx: &egui::Context,
    state: &mut UiState,
    pb: &PlaybackView,
    lyrics: &LyricsView,
    out: &mut Vec<UiCommand>,
) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            match &pb.current {
                Some(cur) => {
                    cover_placeholder(ui, &cur.title, 190.0, state.accent);
                    ui.add_space(6.0);
                    ui.heading(&cur.title);
                    ui.small(&cur.artists);
                    ui.small(&cur.album);
                }
                None => {
                    cover_placeholder(ui, "?", 190.0, state.accent);
                    ui.heading("Nothing playing");
                    ui.small("Pick a track to start");
                }
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.small(format_duration_ms(pb.position_ms));
            let mut pos = pb.position_ms as f32;
            let max = pb.duration_ms.max(1) as f32;
            if ui
                .add(
                    egui::Slider::new(&mut pos, 0.0..=max)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .changed()
            {
                out.push(UiCommand::SeekMs(pos as u64));
            }
            ui.small(format_duration_ms(pb.duration_ms));
        });

        ui.horizontal(|ui| {
            if ui.button("⏮ Prev").clicked() {
                out.push(UiCommand::Prev);
            }
            if ui
                .add_enabled(!pb.loading, egui::Button::new(if pb.playing { "⏸" } else { "▶" }))
                .clicked()
            {
                out.push(UiCommand::TogglePlay);
            }
            if ui.button("Next ⏭").clicked() {
                out.push(UiCommand::Next);
            }
            if ui.button("Exit full view").clicked() {
                state.fullscreen = false;
            }
        });

        ui.add_space(6.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.small(format!("Words offset: {}ms", lyrics.offset_ms));
            if ui.small_button("−100").clicked() {
                out.push(UiCommand::SetLyricsOffset(lyrics.offset_ms - 100));
            }
            if ui.small_button("+100").clicked() {
                out.push(UiCommand::SetLyricsOffset(lyrics.offset_ms + 100));
            }
            if ui.small_button("Reset").clicked() {
                out.push(UiCommand::SetLyricsOffset(0));
            }
        });

        egui::ScrollArea::vertical().show(ui, |ui| {
            render_karaoke(ui, state, lyrics);
        });
    });
}

fn render_karaoke(ui: &mut egui::Ui, state: &UiState, lyrics: &LyricsView) {
    if lyrics.lines.is_empty() {
        ui.small("No words for this track yet.");
        return;
    }
    let dim = egui::Color32::from_rgb(128, 132, 148);
    for line in &lyrics.lines {
        if line.words.is_empty() {
            let color = if line.active {
                egui::Color32::WHITE
            } else {
                dim
            };
            let prefix = if line.active { "▸ " } else { "" };
            ui.label(strong_if(
                egui::RichText::new(format!("{prefix}{}", line.text)).color(color),
                line.active,
            ));
        } else {
            ui.horizontal_wrapped(|ui| {
                if line.active {
                    ui.small("▸");
                }
                for (word, fill) in &line.words {
                    let f = fill.clamp(0.0, 1.0);
                    // Per-word fill: dim -> bright as the word is sung,
                    // fully-sung words take the accent color.
                    let color = if f >= 1.0 {
                        state.accent
                    } else if f <= 0.0 {
                        dim
                    } else {
                        mix_color(dim, egui::Color32::WHITE, f)
                    };
                    ui.label(strong_if(
                        egui::RichText::new(format!("{word} ")).color(color),
                        line.active,
                    ));
                }
            });
        }
    }
    if !lyrics.synced {
        ui.small("Showing plain words (not time synced).");
    }
}

// ---------------------------------------------------------------------------
// Toasts + cover art stand-in
// ---------------------------------------------------------------------------

fn render_toasts(ctx: &egui::Context, state: &UiState) {
    if state.toasts.is_empty() {
        return;
    }
    egui::Window::new("oxidify_toasts")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -84.0))
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                for t in &state.toasts {
                    ui.small(&t.message);
                }
            });
        });
}

/// Apply `.strong()` only when `yes` is true (`RichText::strong` takes no arg).
fn strong_if(text: egui::RichText, yes: bool) -> egui::RichText {
    if yes {
        text.strong()
    } else {
        text
    }
}

/// Stand-in artwork: tinted square with the title's first letter.
fn cover_placeholder(ui: &mut egui::Ui, title: &str, size: f32, accent: egui::Color32) {
    let bg = mix_color(accent, egui::Color32::from_rgb(22, 24, 30), 0.55);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    ui.painter().rect_filled(rect, 6.0, bg);
    let letter = title.chars().next().unwrap_or('?').to_string().to_uppercase();
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        letter,
        egui::FontId::proportional(size * 0.42),
        egui::Color32::WHITE,
    );
}

// ---------------------------------------------------------------------------
// Tests (headless)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tracks() -> Vec<UiTrack> {
        vec![
            UiTrack {
                id: "1".into(),
                title: "Morning Glow".into(),
                artists: "Aurora Field".into(),
                album: "Daybreak".into(),
                duration_ms: 183_000,
                cover_path: None,
                remote: false,
            },
            UiTrack {
                id: "2".into(),
                title: "Night Drive".into(),
                artists: "Velvet Static".into(),
                album: "Afterhours".into(),
                duration_ms: 241_000,
                cover_path: None,
                remote: true,
            },
        ]
    }

    #[test]
    fn progress_clamps() {
        let vm = PlayerViewModel {
            track_title: "t".into(),
            artists: "a".into(),
            position_ms: 5000,
            duration_ms: 4000,
            status: PlayerStatus::Playing,
        };
        assert_eq!(vm.progress(), 1.0);
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration_ms(0), "0:00");
        assert_eq!(format_duration_ms(61_000), "1:01");
        assert_eq!(format_duration_ms(3_600_000), "1:00:00");
        assert_eq!(format_duration_ms(3_661_000), "1:01:01");
        assert_eq!(format_duration_ms(5_000), "0:05");
        assert_eq!(format_duration_ms(599_999), "9:59");
    }

    #[test]
    fn search_filtering() {
        let tracks = sample_tracks();
        assert_eq!(filter_tracks(&tracks, "").len(), 2);
        assert_eq!(filter_tracks(&tracks, "   ").len(), 2);
        let by_title = filter_tracks(&tracks, "morning");
        assert_eq!(by_title.len(), 1);
        assert_eq!(by_title[0].id, "1");
        // Case-insensitive artist match.
        let by_artist = filter_tracks(&tracks, "VELVET");
        assert_eq!(by_artist.len(), 1);
        assert_eq!(by_artist[0].id, "2");
        // Album match.
        assert_eq!(filter_tracks(&tracks, "afterhours").len(), 1);
        // No match.
        assert!(filter_tracks(&tracks, "nope-not-here").is_empty());
        // matches_query basics.
        assert!(matches_query("Hello World", "hello"));
        assert!(matches_query("Hello", ""));
        assert!(!matches_query("Hello", "bye"));
    }

    #[test]
    fn repeat_mode_cycling() {
        assert_eq!(cycle_repeat_mode(0), 1);
        assert_eq!(cycle_repeat_mode(1), 2);
        assert_eq!(cycle_repeat_mode(2), 0);
        // Out-of-range values reset to Off.
        assert_eq!(cycle_repeat_mode(7), 0);
        assert_eq!(repeat_label(0), "Off");
        assert_eq!(repeat_label(1), "All");
        assert_eq!(repeat_label(2), "One");
        assert_eq!(repeat_label(9), "Off");
    }

    #[test]
    fn progress_computation() {
        assert_eq!(progress_ratio(0, 0), 0.0);
        assert_eq!(progress_ratio(5000, 0), 0.0);
        assert_eq!(progress_ratio(0, 4000), 0.0);
        assert_eq!(progress_ratio(2000, 4000), 0.5);
        assert_eq!(progress_ratio(5000, 4000), 1.0);
        assert_eq!(progress_ratio(60_000, 60_000), 1.0);
    }

    #[test]
    fn toast_expiry_logic() {
        let ttl = Duration::from_secs(4);
        let born = Instant::now();
        assert!(!toast_expired(born, born, ttl));
        assert!(toast_expired(born - ttl, born, ttl));
        assert!(toast_expired(
            born - Duration::from_secs(10),
            born,
            ttl
        ));
        assert!(!toast_expired(
            born - Duration::from_secs(3),
            born,
            ttl
        ));

        // push_toast caps the stack at 5 entries.
        let mut state = UiState::new();
        for i in 0..7 {
            push_toast(&mut state, format!("msg {i}"));
        }
        assert_eq!(state.toasts.len(), 5);
        assert_eq!(state.toasts[0].message, "msg 2");

        // Fresh toasts survive pruning.
        state.prune_toasts();
        assert_eq!(state.toasts.len(), 5);
    }

    #[test]
    fn sorted_indices_follow_key_and_direction() {
        let tracks = sample_tracks();
        let asc = sorted_track_indices(&tracks, SortKey::Title, true);
        assert_eq!(tracks[asc[0]].title, "Morning Glow");
        let desc = sorted_track_indices(&tracks, SortKey::Title, false);
        assert_eq!(tracks[desc[0]].title, "Night Drive");
        let by_dur = sorted_track_indices(&tracks, SortKey::Duration, true);
        assert_eq!(tracks[by_dur[0]].duration_ms, 183_000);
    }

    #[test]
    fn color_mix_clamps() {
        let a = egui::Color32::BLACK;
        let b = egui::Color32::WHITE;
        assert_eq!(mix_color(a, b, 0.0), a);
        assert_eq!(mix_color(a, b, 1.0), b);
        assert_eq!(mix_color(a, b, -1.0), a);
        assert_eq!(mix_color(a, b, 99.0), b);
    }
}
