use rusqlite::Connection;
use thiserror::Error;

pub const DB_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Single additive schema. Fresh databases get every table/column at once;
/// pre-existing databases are brought up to date by
/// [`apply_column_migrations`] (new tables via `IF NOT EXISTS`, new columns
/// via `ALTER TABLE ... ADD COLUMN` only when missing).
pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tracks (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  artists_json TEXT NOT NULL,
  album TEXT,
  duration_ms INTEGER NOT NULL,
  file_path TEXT,
  cover_path TEXT,
  play_count INTEGER NOT NULL DEFAULT 0,
  added_at INTEGER NOT NULL DEFAULT 0,
  cover_url TEXT,
  json TEXT NOT NULL DEFAULT '{}',
  updated_at INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_file_path ON tracks(file_path);
CREATE TABLE IF NOT EXISTS audio_blobs (
  track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
  mime TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  path TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS lyrics (
  track_id TEXT PRIMARY KEY,
  sync_level TEXT NOT NULL,
  provider TEXT NOT NULL,
  raw TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS playlists (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS playlist_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  position INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_playlist_items_order ON playlist_items(playlist_id, position);
CREATE TABLE IF NOT EXISTS play_history (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  played_at INTEGER NOT NULL,
  duration_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_play_history_recent ON play_history(played_at DESC);
CREATE TABLE IF NOT EXISTS prefs (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"#;

/// Columns that must exist on `tracks`, with the definition used when they
/// have to be added to a database created by an older schema revision.
const TRACK_COLUMN_DEFS: &[(&str, &str)] = &[
    ("title", "TEXT NOT NULL DEFAULT ''"),
    ("artists_json", "TEXT NOT NULL DEFAULT '[]'"),
    ("duration_ms", "INTEGER NOT NULL DEFAULT 0"),
    ("album", "TEXT"),
    ("file_path", "TEXT"),
    ("cover_path", "TEXT"),
    ("play_count", "INTEGER NOT NULL DEFAULT 0"),
    ("added_at", "INTEGER NOT NULL DEFAULT 0"),
    ("cover_url", "TEXT"),
    ("json", "TEXT NOT NULL DEFAULT '{}'"),
    ("updated_at", "INTEGER NOT NULL DEFAULT 0"),
];

pub(crate) fn apply_column_migrations(conn: &Connection) -> Result<(), DbError> {
    let existing: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA table_info(tracks)")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (name, def) in TRACK_COLUMN_DEFS {
        if !existing.iter().any(|c| c == name) {
            conn.execute(
                &format!("ALTER TABLE tracks ADD COLUMN {name} {def}"),
                [],
            )?;
        }
    }
    Ok(())
}

pub fn open_or_create(path: &std::path::Path) -> Result<Connection, DbError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            DbError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    apply_column_migrations(&conn)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creates_expected_tables() {
        let dir = std::env::temp_dir().join(format!("oxidify-test-{}", std::process::id()));
        let db_path = dir.join("meta.sqlite");
        let conn = open_or_create(&db_path).unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for t in ["tracks", "audio_blobs", "lyrics"] {
            assert!(tables.iter().any(|x| x == t), "missing {t}: {tables:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
