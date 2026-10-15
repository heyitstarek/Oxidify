use rusqlite::{OptionalExtension, Row, params};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("db: {0}")]
    Db(#[from] crate::db::DbError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("track not found: {0}")]
    NotFound(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredTrack {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_ms: u64,
    pub path: PathBuf,
    pub cover_path: Option<PathBuf>,
    pub play_count: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    /// Per-file problems encountered while walking, tagging, or pruning.
    /// A scan never aborts on a single bad file.
    pub errors: Vec<String>,
}

impl ScanReport {
    pub fn error_count(&self) -> usize {
        self.errors.len()
    }

    pub fn is_clean(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub track: StoredTrack,
    pub played_at: i64,
    pub duration_ms: u64,
}

/// File-backed music library. The schema is applied by
/// [`crate::db::open_or_create`], so every opener shares one additive path.
pub struct LibraryDb {
    conn: rusqlite::Connection,
}

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "mp4", "ogg", "oga", "opus", "wav", "aac", "wma", "aiff", "aif",
];

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Deterministic 64-bit FNV-1a. Used instead of the default hasher so track
/// ids are stable across processes and restarts.
fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Stable track id: hash of the canonical file path.
pub fn track_id_for_path(canonical_path: &str) -> String {
    format!("{:016x}", fnv1a64(canonical_path))
}

/// Best-effort canonical path string for hashing and for the stored
/// `file_path`. Falls back to an absolute path when the file cannot be
/// canonicalized (e.g. it vanished mid-scan).
fn canonical_string(path: &Path) -> String {
    let s = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            if path.is_absolute() {
                path.to_string_lossy().into_owned()
            } else {
                std::env::current_dir()
                    .map(|c| c.join(path).to_string_lossy().into_owned())
                    .unwrap_or_else(|_| path.to_string_lossy().into_owned())
            }
        });
    #[cfg(target_os = "windows")]
    {
        s.to_lowercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        s
    }
}

fn parse_artists_json(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn row_to_track(row: &Row) -> Result<StoredTrack, rusqlite::Error> {
    let id: String = row.get("id")?;
    let title: String = row.get("title")?;
    let artists_json: String = row.get("artists_json")?;
    let album: Option<String> = row.get("album")?;
    let duration_ms: i64 = row.get("duration_ms")?;
    let file_path: Option<String> = row.get("file_path")?;
    let cover_path: Option<String> = row.get("cover_path")?;
    let play_count: i64 = row.get("play_count")?;
    Ok(StoredTrack {
        title,
        artists: parse_artists_json(&artists_json),
        album: album.filter(|a| !a.is_empty()),
        duration_ms: duration_ms.max(0) as u64,
        path: file_path.map(PathBuf::from).unwrap_or_default(),
        cover_path: cover_path.filter(|c| !c.is_empty()).map(PathBuf::from),
        play_count: play_count.max(0) as u64,
        id,
    })
}

const TRACK_COLUMNS: &str =
    "id, title, artists_json, album, duration_ms, file_path, cover_path, play_count";

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanStatus {
    Added,
    Updated,
    Unchanged,
}

impl LibraryDb {
    pub fn open(path: &Path) -> Result<Self, LibraryError> {
        Ok(Self {
            conn: crate::db::open_or_create(path)?,
        })
    }

    pub fn open_in_memory() -> Result<Self, LibraryError> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.execute_batch(crate::db::SCHEMA)?;
        crate::db::apply_column_migrations(&conn)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(Self { conn })
    }

    /// Insert or replace a track row by id. `added_at` is set for new rows
    /// and preserved for existing ones.
    pub fn save_track(&self, track: &StoredTrack) -> Result<(), LibraryError> {
        let artists_json = serde_json::to_string(&track.artists)?;
        let file_path = track.path.to_string_lossy();
        let file_path: Option<&str> = if file_path.is_empty() {
            None
        } else {
            Some(&file_path)
        };
        let cover_path: Option<String> = track
            .cover_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        self.conn.execute(
            "INSERT INTO tracks (id, title, artists_json, album, duration_ms, file_path, cover_path, play_count, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
               title = excluded.title,
               artists_json = excluded.artists_json,
               album = excluded.album,
               duration_ms = excluded.duration_ms,
               file_path = excluded.file_path,
               cover_path = excluded.cover_path,
               play_count = excluded.play_count",
            params![
                track.id,
                track.title,
                artists_json,
                track.album,
                track.duration_ms as i64,
                file_path,
                cover_path,
                track.play_count as i64,
                now_ms(),
            ],
        )?;
        Ok(())
    }

    /// Walk `dir` recursively, decode tags with `lofty`, extract the first
    /// embedded picture per file into `cover_cache_dir` (filename derived
    /// from the stable track id), upsert rows by `file_path`, and prune rows
    /// whose files no longer exist. Never aborts on a single bad file.
    pub fn scan_directory(
        &self,
        dir: &Path,
        cover_cache_dir: &Path,
    ) -> Result<ScanReport, LibraryError> {
        if !dir.is_dir() {
            return Err(LibraryError::InvalidPath(format!(
                "not a directory: {}",
                dir.display()
            )));
        }
        std::fs::create_dir_all(cover_cache_dir)?;
        let mut report = ScanReport::default();
        for entry in walkdir::WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
        {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    report.errors.push(format!("walk error: {e}"));
                    continue;
                }
            };
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            if !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }
            match self.scan_one_file(path, cover_cache_dir) {
                Ok(ScanStatus::Added) => report.added += 1,
                Ok(ScanStatus::Updated) => report.updated += 1,
                Ok(ScanStatus::Unchanged) => {}
                Err(msg) => report.errors.push(msg),
            }
        }
        report.removed = self.prune_missing_files()?;
        Ok(report)
    }

    fn scan_one_file(
        &self,
        path: &Path,
        cover_cache_dir: &Path,
    ) -> Result<ScanStatus, String> {
        use lofty::file::{AudioFile as _, TaggedFileExt as _};
        use lofty::prelude::Accessor as _;
        let canonical = canonical_string(path);
        let id = track_id_for_path(&canonical);
        let tagged =
            lofty::read_from_path(path).map_err(|e| format!("{}: tag read: {e}", path.display()))?;
        let props = tagged.properties();
        let duration_ms = props.duration().as_millis().min(u128::from(u32::MAX)) as u64;
        let (title, artists, album, picture) = match tagged.primary_tag().or(tagged.first_tag()) {
            Some(tag) => {
                let title = tag
                    .title()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty());
                let artists = tag
                    .artist()
                    .map(|a| {
                        a.split([';', '|'])
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let album = tag
                    .album()
                    .map(|a| a.trim().to_string())
                    .filter(|a| !a.is_empty());
                let picture = tag.pictures().first().map(|p| p.data().to_vec());
                (title, artists, album, picture)
            }
            None => (None, Vec::new(), None, None),
        };
        let title = title.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Unknown Title".to_string())
        });
        let cover_path = match picture {
            Some(bytes) if !bytes.is_empty() => {
                let ext = sniff_image_ext(&bytes);
                let dest = cover_cache_dir.join(format!("{id}.{ext}"));
                match std::fs::write(&dest, &bytes) {
                    Ok(()) => Some(dest.to_string_lossy().into_owned()),
                    Err(e) => {
                        return Err(format!("{}: cover write: {e}", path.display()));
                    }
                }
            }
            _ => None,
        };
        let artists_json =
            serde_json::to_string(&artists).map_err(|e| format!("{}: encode: {e}", path.display()))?;
        let existing: Option<(String, String, Option<String>, i64, Option<String>)> = self
            .conn
            .query_row(
                "SELECT title, artists_json, album, duration_ms, cover_path FROM tracks WHERE file_path = ?1",
                params![canonical],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("{}: db: {e}", path.display()))?;
        match existing {
            None => {
                self.conn
                    .execute(
                        "INSERT INTO tracks (id, title, artists_json, album, duration_ms, file_path, cover_path, play_count, added_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
                        params![
                            id,
                            title,
                            artists_json,
                            album,
                            duration_ms as i64,
                            canonical,
                            cover_path,
                            now_ms(),
                        ],
                    )
                    .map_err(|e| format!("{}: db: {e}", path.display()))?;
                Ok(ScanStatus::Added)
            }
            Some((t, a, al, d, c))
                if t == title
                    && a == artists_json
                    && al == album
                    && d == duration_ms as i64
                    && c == cover_path =>
            {
                Ok(ScanStatus::Unchanged)
            }
            Some(_) => {
                self.conn
                    .execute(
                        "UPDATE tracks SET id = ?1, title = ?2, artists_json = ?3, album = ?4, duration_ms = ?5, cover_path = ?6 WHERE file_path = ?7",
                        params![id, title, artists_json, album, duration_ms as i64, cover_path, canonical],
                    )
                    .map_err(|e| format!("{}: db: {e}", path.display()))?;
                Ok(ScanStatus::Updated)
            }
        }
    }

    /// Delete rows whose `file_path` no longer exists on disk. Returns the
    /// number of rows removed.
    fn prune_missing_files(&self) -> Result<usize, LibraryError> {
        let rows: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, file_path FROM tracks WHERE file_path IS NOT NULL")?;
            let mapped = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            mapped.collect::<Result<Vec<_>, _>>()?
        };
        let mut removed = 0;
        for (id, file_path) in rows {
            if !Path::new(&file_path).exists() {
                self.conn
                    .execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn search(&self, q: &str, limit: usize) -> Result<Vec<StoredTrack>, LibraryError> {
        let like = format!("%{}%", escape_like(q));
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks
             WHERE title LIKE ?1 ESCAPE '\\' OR artists_json LIKE ?1 ESCAPE '\\' OR album LIKE ?1 ESCAPE '\\'
             ORDER BY title COLLATE NOCASE, id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![like, limit.min(i64::MAX as usize) as i64], row_to_track)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(LibraryError::from)
    }

    pub fn all_tracks(&self, limit: usize, offset: usize) -> Result<Vec<StoredTrack>, LibraryError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks ORDER BY title COLLATE NOCASE, id LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = stmt.query_map(
            params![limit.min(i64::MAX as usize) as i64, offset.min(i64::MAX as usize) as i64],
            row_to_track,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(LibraryError::from)
    }

    fn track_rows_ordered(&self) -> Result<Vec<StoredTrack>, LibraryError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TRACK_COLUMNS} FROM tracks ORDER BY album COLLATE NOCASE, title COLLATE NOCASE, id"
        ))?;
        let rows = stmt.query_map([], row_to_track)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(LibraryError::from)
    }

    /// `(album title, representative artist, track count)`, ordered by title.
    /// Tracks without an album are grouped under the empty title.
    pub fn albums(&self) -> Result<Vec<(String, String, u64)>, LibraryError> {
        let mut order: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut artist_for: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for t in self.track_rows_ordered()? {
            let title = t.album.clone().unwrap_or_default();
            if !seen.contains(&title) {
                seen.insert(title.clone());
                order.push(title.clone());
                artist_for.insert(
                    title.clone(),
                    t.artists.first().cloned().unwrap_or_default(),
                );
            }
            *counts.entry(title).or_insert(0) += 1;
        }
        Ok(order
            .into_iter()
            .map(|title| {
                let count = counts.get(&title).copied().unwrap_or(0);
                let artist = artist_for.remove(&title).unwrap_or_default();
                (title, artist, count)
            })
            .collect())
    }

    /// `(artist name, track count)`, ordered by name. A track with several
    /// artists counts once for each of them.
    pub fn artists(&self) -> Result<Vec<(String, u64)>, LibraryError> {
        let mut counts: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for t in self.track_rows_ordered()? {
            for a in &t.artists {
                if !a.is_empty() {
                    *counts.entry(a.clone()).or_insert(0) += 1;
                }
            }
        }
        let mut out: Vec<(String, u64)> = counts.into_iter().collect();
        out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then(a.0.cmp(&b.0)));
        Ok(out)
    }

    pub fn create_playlist(&self, title: &str) -> Result<String, LibraryError> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!(
            "pl_{:x}_{}",
            fnv1a64(&format!("{}-{}-{}", title, now_ms(), n)),
            std::process::id()
        );
        self.conn.execute(
            "INSERT INTO playlists (id, title, created_at) VALUES (?1, ?2, ?3)",
            params![id, title, now_ms()],
        )?;
        Ok(id)
    }

    pub fn rename_playlist(&self, id: &str, title: &str) -> Result<(), LibraryError> {
        let changed = self.conn.execute(
            "UPDATE playlists SET title = ?1 WHERE id = ?2",
            params![title, id],
        )?;
        if changed == 0 {
            return Err(LibraryError::NotFound(id.to_string()));
        }
        Ok(())
    }

    pub fn delete_playlist(&self, id: &str) -> Result<(), LibraryError> {
        self.conn
            .execute("DELETE FROM playlist_items WHERE playlist_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn list_playlists(&self) -> Result<Vec<Playlist>, LibraryError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, title, created_at FROM playlists ORDER BY created_at, rowid")?;
        let rows = stmt.query_map([], |row| {
            Ok(Playlist {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(LibraryError::from)
    }

    fn require_playlist(&self, playlist_id: &str) -> Result<(), LibraryError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM playlists WHERE id = ?1",
            params![playlist_id],
            |row| row.get(0),
        )?;
        if count == 0 {
            return Err(LibraryError::NotFound(playlist_id.to_string()));
        }
        Ok(())
    }

    fn require_track(&self, track_id: &str) -> Result<(), LibraryError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tracks WHERE id = ?1",
            params![track_id],
            |row| row.get(0),
        )?;
        if count == 0 {
            return Err(LibraryError::NotFound(track_id.to_string()));
        }
        Ok(())
    }

    pub fn playlist_add(&self, playlist_id: &str, track_id: &str) -> Result<(), LibraryError> {
        self.require_playlist(playlist_id)?;
        self.require_track(track_id)?;
        let next: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_items WHERE playlist_id = ?1",
            params![playlist_id],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO playlist_items (playlist_id, track_id, position) VALUES (?1, ?2, ?3)",
            params![playlist_id, track_id, next],
        )?;
        Ok(())
    }

    pub fn playlist_items(&self, playlist_id: &str) -> Result<Vec<StoredTrack>, LibraryError> {
        let cols = TRACK_COLUMNS
            .split(',')
            .map(|c| format!("t.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {cols} FROM tracks t
             JOIN playlist_items i ON i.track_id = t.id
             WHERE i.playlist_id = ?1 ORDER BY i.position, i.id"
        ))?;
        let rows = stmt.query_map(params![playlist_id], row_to_track)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(LibraryError::from)
    }

    pub fn playlist_remove(&self, playlist_id: &str, track_id: &str) -> Result<(), LibraryError> {
        self.conn.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1 AND track_id = ?2",
            params![playlist_id, track_id],
        )?;
        self.renumber_playlist(playlist_id)?;
        Ok(())
    }

    /// Move the item at `from` to index `to` within the playlist ordering.
    pub fn playlist_move(
        &self,
        playlist_id: &str,
        from: usize,
        to: usize,
    ) -> Result<(), LibraryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position, id",
        )?;
        let mut ids: Vec<i64> = stmt
            .query_map(params![playlist_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if from >= ids.len() || to >= ids.len() {
            return Err(LibraryError::InvalidArgument(format!(
                "move {from} -> {to} out of range for {} items",
                ids.len()
            )));
        }
        let row_id = ids.remove(from);
        ids.insert(to, row_id);
        for (position, row_id) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE playlist_items SET position = ?1 WHERE id = ?2",
                params![position as i64, row_id],
            )?;
        }
        Ok(())
    }

    fn renumber_playlist(&self, playlist_id: &str) -> Result<(), LibraryError> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position, id",
        )?;
        let ids: Vec<i64> = stmt
            .query_map(params![playlist_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for (position, row_id) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE playlist_items SET position = ?1 WHERE id = ?2",
                params![position as i64, row_id],
            )?;
        }
        Ok(())
    }

    /// Bump `play_count` and append a `play_history` row stamped with the
    /// current time.
    pub fn record_play(&self, track_id: &str, duration_ms: u64) -> Result<(), LibraryError> {
        let changed = self.conn.execute(
            "UPDATE tracks SET play_count = play_count + 1 WHERE id = ?1",
            params![track_id],
        )?;
        if changed == 0 {
            return Err(LibraryError::NotFound(track_id.to_string()));
        }
        self.conn.execute(
            "INSERT INTO play_history (track_id, played_at, duration_ms) VALUES (?1, ?2, ?3)",
            params![track_id, now_ms(), duration_ms as i64],
        )?;
        Ok(())
    }

    pub fn recent_history(&self, limit: usize) -> Result<Vec<HistoryEntry>, LibraryError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT h.played_at, h.duration_ms, {cols} FROM play_history h
             JOIN tracks t ON t.id = h.track_id
             ORDER BY h.played_at DESC, h.id DESC LIMIT ?1",
            cols = TRACK_COLUMNS
                .split(',')
                .map(|c| format!("t.{}", c.trim()))
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        let rows = stmt.query_map(params![limit.min(i64::MAX as usize) as i64], |row| {
            let track = StoredTrack {
                id: row.get(2)?,
                title: row.get(3)?,
                artists: parse_artists_json(&row.get::<_, String>(4)?),
                album: {
                    let a: Option<String> = row.get(5)?;
                    a.filter(|a| !a.is_empty())
                },
                duration_ms: row.get::<_, i64>(6)?.max(0) as u64,
                path: {
                    let p: Option<String> = row.get(7)?;
                    p.map(PathBuf::from).unwrap_or_default()
                },
                cover_path: {
                    let c: Option<String> = row.get(8)?;
                    c.filter(|c| !c.is_empty()).map(PathBuf::from)
                },
                play_count: row.get::<_, i64>(9)?.max(0) as u64,
            };
            Ok(HistoryEntry {
                played_at: row.get(0)?,
                duration_ms: row.get::<_, i64>(1)?.max(0) as u64,
                track,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(LibraryError::from)
    }

    pub fn pref_get(&self, key: &str) -> Result<Option<String>, LibraryError> {
        self.conn
            .query_row("SELECT value FROM prefs WHERE key = ?1", params![key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(LibraryError::from)
    }

    pub fn pref_set(&self, key: &str, value: &str) -> Result<(), LibraryError> {
        self.conn.execute(
            "INSERT INTO prefs (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

/// Identify a container image format from magic bytes so cover files get a
/// usable extension without trusting tag metadata.
fn sniff_image_ext(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        "jpg"
    } else if bytes.len() >= 8
        && bytes[0] == 0x89
        && bytes[1] == 0x50
        && bytes[2] == 0x4E
        && bytes[3] == 0x47
    {
        "png"
    } else if bytes.len() >= 6 && &bytes[0..6] == b"GIF87a" || bytes.len() >= 6 && &bytes[0..6] == b"GIF89a" {
        "gif"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else if bytes.len() >= 2 && bytes[0] == 0x42 && bytes[1] == 0x4D {
        "bmp"
    } else {
        "bin"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str, title: &str, artists: &[&str], album: Option<&str>) -> StoredTrack {
        StoredTrack {
            id: id.to_string(),
            title: title.to_string(),
            artists: artists.iter().map(|s| s.to_string()).collect(),
            album: album.map(|s| s.to_string()),
            duration_ms: 180_000,
            path: PathBuf::from(format!("/music/{id}.mp3")),
            cover_path: None,
            play_count: 0,
        }
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oxidify-lib-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn track_id_is_stable_for_same_path() {
        let a = track_id_for_path("/music/song.mp3");
        let b = track_id_for_path("/music/song.mp3");
        let c = track_id_for_path("/music/other.mp3");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn crud_search_and_aggregations() {
        let db = LibraryDb::open_in_memory().unwrap();
        db.save_track(&sample("t1", "Blue Sky", &["Ava"], Some("Horizon"))).unwrap();
        db.save_track(&sample("t2", "Blue River", &["Ava", "Bo"], Some("Horizon"))).unwrap();
        db.save_track(&sample("t3", "Green Hill", &["Bo"], None)).unwrap();

        let hits = db.search("blue", 10).unwrap();
        assert_eq!(hits.len(), 2);

        // LIKE metacharacters are escaped, not treated as wildcards.
        db.save_track(&sample("t4", "100% Pure", &["Zed"], None)).unwrap();
        assert_eq!(db.search("100%", 10).unwrap().len(), 1);
        assert!(db.search("100", 10).unwrap().iter().any(|t| t.id == "t4"));

        let page = db.all_tracks(2, 0).unwrap();
        assert_eq!(page.len(), 2);
        let rest = db.all_tracks(10, 2).unwrap();
        assert_eq!(rest.len(), 2);

        let artists = db.artists().unwrap();
        let ava = artists.iter().find(|(n, _)| n == "Ava").unwrap();
        assert_eq!(ava.1, 2);
        let bo = artists.iter().find(|(n, _)| n == "Bo").unwrap();
        assert_eq!(bo.1, 2);

        let albums = db.albums().unwrap();
        let horizon = albums.iter().find(|(t, _, _)| t == "Horizon").unwrap();
        assert_eq!(horizon.2, 2);
        assert!(!horizon.1.is_empty());
    }

    #[test]
    fn playlists_items_and_move() {
        let db = LibraryDb::open_in_memory().unwrap();
        db.save_track(&sample("t1", "One", &["A"], None)).unwrap();
        db.save_track(&sample("t2", "Two", &["A"], None)).unwrap();
        db.save_track(&sample("t3", "Three", &["A"], None)).unwrap();

        let pl = db.create_playlist("Mix").unwrap();
        db.playlist_add(&pl, "t1").unwrap();
        db.playlist_add(&pl, "t2").unwrap();
        db.playlist_add(&pl, "t3").unwrap();
        assert_eq!(db.playlist_items(&pl).unwrap().len(), 3);

        db.playlist_move(&pl, 0, 2).unwrap();
        let ids: Vec<String> = db.playlist_items(&pl).unwrap().iter().map(|t| t.id.clone()).collect();
        assert_eq!(ids, vec!["t2", "t3", "t1"]);

        db.playlist_remove(&pl, "t3").unwrap();
        let ids: Vec<String> = db.playlist_items(&pl).unwrap().iter().map(|t| t.id.clone()).collect();
        assert_eq!(ids, vec!["t2", "t1"]);

        db.rename_playlist(&pl, "Renamed").unwrap();
        assert_eq!(db.list_playlists().unwrap()[0].title, "Renamed");
        assert!(db.rename_playlist("missing", "x").is_err());

        db.delete_playlist(&pl).unwrap();
        assert!(db.list_playlists().unwrap().is_empty());
        assert!(db.playlist_move(&pl, 0, 0).is_err());
    }

    #[test]
    fn play_history_and_prefs() {
        let db = LibraryDb::open_in_memory().unwrap();
        db.save_track(&sample("t1", "One", &["A"], None)).unwrap();
        db.record_play("t1", 181_000).unwrap();
        db.record_play("t1", 182_000).unwrap();
        assert!(db.record_play("ghost", 10).is_err());

        let history = db.recent_history(10).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history[0].played_at >= history[1].played_at);
        assert_eq!(history[0].track.id, "t1");

        let all = db.all_tracks(10, 0).unwrap();
        assert_eq!(all[0].play_count, 2);

        assert_eq!(db.pref_get("theme").unwrap(), None);
        db.pref_set("theme", "dark").unwrap();
        assert_eq!(db.pref_get("theme").unwrap().as_deref(), Some("dark"));
        db.pref_set("theme", "light").unwrap();
        assert_eq!(db.pref_get("theme").unwrap().as_deref(), Some("light"));
    }

    #[test]
    fn scan_empty_dir_and_bad_file_tolerance() {
        let db = LibraryDb::open_in_memory().unwrap();
        let dir = scratch_dir("scan");
        let covers = dir.join("covers");
        let report = db.scan_directory(&dir, &covers).unwrap();
        assert_eq!(report.added, 0);
        assert!(report.is_clean());

        std::fs::write(dir.join("broken.mp3"), b"this is not audio").unwrap();
        std::fs::write(dir.join("notes.txt"), b"not scanned").unwrap();
        let report = db.scan_directory(&dir, &covers).unwrap();
        assert_eq!(report.added, 0);
        assert_eq!(report.error_count(), 1);
        assert!(report.errors[0].contains("broken.mp3"));

        assert!(db.scan_directory(&dir.join("nope"), &covers).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_prunes_vanished_files() {
        let db = LibraryDb::open_in_memory().unwrap();
        let dir = scratch_dir("prune");
        let covers = dir.join("covers");
        let mut ghost = sample("g1", "Ghost", &["A"], None);
        ghost.path = dir.join("ghost.mp3");
        db.save_track(&ghost).unwrap();
        assert_eq!(db.all_tracks(10, 0).unwrap().len(), 1);
        let report = db.scan_directory(&dir, &covers).unwrap();
        assert_eq!(report.removed, 1);
        assert!(db.all_tracks(10, 0).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_imports_tagged_mp3_with_cover() {
        use lofty::config::WriteOptions;
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::tag::{Accessor as _, Tag, TagExt as _, TagType};

        let db = LibraryDb::open_in_memory().unwrap();
        let dir = scratch_dir("roundtrip");
        let covers = dir.join("covers");
        let song = dir.join("roundtrip.mp3");
        std::fs::write(&song, b"").unwrap();

        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("Roundtrip Song".to_string());
        tag.set_artist("Test Artist".to_string());
        tag.set_album("Test Album".to_string());
        let art = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01,
        ];
        tag.push_picture(Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::Jpeg),
            None,
            art,
        ));
        // Raw tag bytes plus synthetic silent MPEG1 Layer III frames
        // (128 kbps, 44.1 kHz): lofty requires audio frames and validates
        // each header against the next, but never decodes them.
        // `read_from_path` detects the container from the `.mp3`
        // extension, which is all the scanner needs.
        let mut out = std::fs::File::create(&song).unwrap();
        tag.dump_to(&mut out, WriteOptions::default()).unwrap();
        use std::io::Write as _;
        let mut frame = vec![0u8; 417];
        frame[0] = 0xFF;
        frame[1] = 0xFB;
        frame[2] = 0x90;
        frame[3] = 0x00;
        for _ in 0..3 {
            out.write_all(&frame).unwrap();
        }
        drop(out);

        let report = db.scan_directory(&dir, &covers).unwrap();
        assert!(report.is_clean(), "errors: {:?}", report.errors);
        assert_eq!(report.added, 1);

        let tracks = db.all_tracks(10, 0).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title, "Roundtrip Song");
        assert_eq!(tracks[0].artists, vec!["Test Artist".to_string()]);
        assert_eq!(tracks[0].album.as_deref(), Some("Test Album"));
        let cover = tracks[0].cover_path.as_ref().expect("cover extracted");
        assert_eq!(cover.extension().and_then(|s| s.to_str()), Some("jpg"));
        assert!(cover.is_file());

        // A second scan is a no-op: stable ids plus change detection.
        let again = db.scan_directory(&dir, &covers).unwrap();
        assert_eq!((again.added, again.updated, again.removed), (0, 0, 0));
        assert!(again.is_clean());

        assert_eq!(db.search("roundtrip", 10).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_sniffing_covers_common_types() {
        assert_eq!(sniff_image_ext(&[0xFF, 0xD8, 0xFF, 0x00]), "jpg");
        assert_eq!(
            sniff_image_ext(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
            "png"
        );
        assert_eq!(sniff_image_ext(b"GIF89a..."), "gif");
        assert_eq!(sniff_image_ext(b"RIFF\x00\x00\x00\x00WEBP"), "webp");
        assert_eq!(sniff_image_ext(&[0x42, 0x4D, 0x00]), "bmp");
        assert_eq!(sniff_image_ext(&[0x00, 0x01]), "bin");
    }
}
