//! Optional remote service client: auth, catalog, streaming sessions,
//! and lyric event streams over HTTPS.
//!
//! The base URL is always explicit (no baked-in default); plain HTTP is
//! rejected except for loopback development URLs. Requests carry
//! `x-request-id`, `x-client-version`, and `x-platform` for tracing, plus an
//! HMAC signer when the deployment requires one.

pub mod auth;
pub mod client;
pub mod lyrics_stream;
pub mod playback;
pub mod remote;

pub use auth::{AuthTokens, AuthUser};
pub use client::{ApiClient, ApiError, CLIENT_VERSION};
pub use lyrics_stream::LyricsStreamEvent;
pub use playback::{Codec, PlaybackSession};
pub use remote::{RemoteClient, needs_refresh};
