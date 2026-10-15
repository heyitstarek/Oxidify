use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Opus,
    Aac,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSession {
    pub session_id: String,
    pub track_id: String,
    pub stream_url: String,
    pub codec: Codec,
    pub bitrate: u32,
    pub mime_type: String,
    pub expires_at: String,
    pub transport: Option<String>,
    pub playback_id: Option<String>,
}

impl PlaybackSession {
    /// Sessions expire server-side; a session counts as stale 15s before
    /// `expiresAt`, and URLs past that skew are refreshed, never reused.
    pub const EXPIRY_SKEW_MS: i64 = 15_000;

    pub fn is_audio_only_mime(&self) -> bool {
        self.mime_type.starts_with("audio/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expiry_skew_is_15s() {
        assert_eq!(PlaybackSession::EXPIRY_SKEW_MS, 15_000);
    }
}
