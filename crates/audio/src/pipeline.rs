use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioPipelineError {
    #[error("unsupported codec: {0}")]
    UnsupportedCodec(String),
    #[error("stream error: {0}")]
    Stream(String),
}

/// Decode routing decision. Symphonia covers AAC/MP3/FLAC; Opus needs a
/// dedicated path (e.g. `audiopus`/`opus` crate). This enum forces the call
/// site to handle that *before* playback is attempted — the M1 prototype must
/// test both AAC and Opus `streamUrl` payloads and report which path decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecSupport {
    Symphonia,
    OpusNative,
    Unsupported,
}

pub fn decode_plan(codec: &str, mime: &str) -> Result<CodecSupport, AudioPipelineError> {
    let c = codec.to_ascii_lowercase();
    let m = mime.to_ascii_lowercase();
    if c == "opus" || m.contains("opus") || m.contains("webm") || m.contains("ogg") {
        return Ok(CodecSupport::OpusNative);
    }
    if c == "aac" || m.contains("mp4") || m.contains("aac") || m.contains("m4a") {
        return Ok(CodecSupport::Symphonia);
    }
    if m.contains("mpeg") || m.contains("mp3") || m.contains("flac") || m.contains("wav") {
        return Ok(CodecSupport::Symphonia);
    }
    Err(AudioPipelineError::UnsupportedCodec(format!(
        "{codec}/{mime}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_routes_to_native_path() {
        assert_eq!(
            decode_plan("opus", "audio/webm").unwrap(),
            CodecSupport::OpusNative
        );
    }

    #[test]
    fn aac_routes_to_symphonia() {
        assert_eq!(
            decode_plan("aac", "audio/mp4").unwrap(),
            CodecSupport::Symphonia
        );
    }

    #[test]
    fn unknown_is_an_error_not_silence() {
        assert!(decode_plan("x", "application/octet-stream").is_err());
    }
}
