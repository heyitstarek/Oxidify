//! Oxidify core: domain models and playback rules.

pub mod lyrics;
pub mod lrc;
pub mod playback;
pub mod queue;
pub mod track;

pub use lrc::parse_lrc;
pub use lyrics::{ParsedLyrics, SyncedLine, WordData, active_line_index, apply_lyrics_offset, word_progress};
pub use playback::{EffectsState, PlayerCommand, PlayerStatus, RepeatMode};
pub use queue::{QueueState, next_index, prev_index};
pub use track::{AlbumRef, ArtistRef, Cover, Track};
