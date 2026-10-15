//! Cache, SQLite metadata, preferences, and secrets.
//!
//! Rules:
//! - Audio blobs cached per track id; tracks longer than 20 minutes are
//!   streamed, never fully background-cached.
//! - Categories: covers / audio / lyrics / metadata, each clearable.
//! - Auth tokens live in OS-protected storage via the `keyring` crate,
//!   never in plain SQLite.

pub mod cache;
pub mod db;
pub mod library;
pub mod secrets;

pub use cache::{CacheCategory, CachePaths, MAX_BACKGROUND_CACHE_TRACK_MS, cache_file_for};
pub use db::{DB_SCHEMA_VERSION, open_or_create};
pub use library::{
    HistoryEntry, LibraryDb, LibraryError, Playlist, ScanReport, StoredTrack, track_id_for_path,
};
pub use secrets::{SECRET_SERVICE, SecretError, delete_secret, load_secret, store_secret};
