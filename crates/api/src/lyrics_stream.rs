use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LyricsFormat {
    Plain,
    Lrc,
    Ttml,
    Qrc,
    /// Word-level JSON lyric payloads.
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LyricsSyncLevel {
    #[serde(rename = "plain")]
    Plain,
    #[serde(rename = "line_level")]
    LineLevel,
    #[serde(rename = "word_level")]
    WordLevel,
    #[serde(rename = "syllable_level")]
    SyllableLevel,
}

/// SSE frames from `GET /v1/tracks/{id}/lyrics`.
/// Wire format is `text/event-stream` with `data: <json>` frames;
/// a blank line dispatches one event. Malformed frames are discarded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LyricsStreamEvent {
    Metadata { track: LyricsTrackMeta },
    Provider(ProviderEvent),
    Done(DoneEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricsTrackMeta {
    pub id: String,
    pub title: String,
    pub artists: Vec<LyricsArtist>,
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LyricsArtist {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProviderEvent {
    Found {
        provider: String,
        #[serde(rename = "syncLevel")]
        sync_level: LyricsSyncLevel,
        candidate: serde_json::Value,
        #[serde(rename = "durationMs")]
        duration_ms: Option<u64>,
    },
    NotFound {
        provider: String,
        #[serde(rename = "durationMs")]
        duration_ms: Option<u64>,
    },
    Error {
        provider: String,
        message: String,
        #[serde(rename = "durationMs")]
        duration_ms: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoneEvent {
    pub cached: bool,
    pub best: Option<serde_json::Value>,
    pub status: Option<String>,
}

/// Split an SSE byte stream into `data:` payloads, mirroring the TS
/// `consumeLines` loop (comments `:` skipped, blank line dispatches).
pub fn split_sse_data_frames(chunk: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut data_lines: Vec<String> = Vec::new();
    for line in chunk.split_inclusive('\n') {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !data_lines.is_empty() {
                out.push(data_lines.join("\n"));
                data_lines.clear();
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let v = rest.strip_prefix(' ').unwrap_or(rest);
            data_lines.push(v.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_split_matches_ts_consume_lines() {
        let chunk = ": keep-alive\ndata: {\"type\":\"done\"}\n\n\
             data: {\"type\":\"metadata\"}\n\n";
        let frames = split_sse_data_frames(chunk);
        assert_eq!(frames.len(), 2);
        assert!(frames[0].contains("done"));
    }
}
