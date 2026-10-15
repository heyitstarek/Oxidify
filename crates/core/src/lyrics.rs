//! Lyrics model: line/word timing in milliseconds, plus the offset and
//! lookup helpers the animator and fullscreen player share.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordData {
    pub time_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub is_background: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncedLine {
    pub time_ms: u64,
    pub duration_ms: Option<u64>,
    pub text: String,
    pub is_instrumental: bool,
    pub words: Option<Vec<WordData>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedLyrics {
    pub synced_lines: Vec<SyncedLine>,
    pub plain_lyrics: Option<String>,
}

/// Mirrors `applyLyricsOffset`: shift all timestamps, clamp at 0.
pub fn apply_lyrics_offset(lines: &[SyncedLine], offset_ms: i64) -> Vec<SyncedLine> {
    lines
        .iter()
        .map(|l| {
            let shift = |t: u64| (t as i64 + offset_ms).max(0) as u64;
            SyncedLine {
                time_ms: shift(l.time_ms),
                duration_ms: l.duration_ms,
                text: l.text.clone(),
                is_instrumental: l.is_instrumental,
                words: l.words.as_ref().map(|ws| {
                    ws.iter()
                        .map(|w| WordData {
                            time_ms: shift(w.time_ms),
                            end_ms: shift(w.end_ms),
                            text: w.text.clone(),
                            is_background: w.is_background,
                        })
                        .collect()
                }),
            }
        })
        .collect()
}

/// Active-line lookup for the animator: the last content line whose
/// timestamp is at or before the given position.
/// Callers must pass the audio playback clock, not a UI timer.
pub fn active_line_index(lines: &[SyncedLine], position_ms: u64) -> Option<usize> {
    let mut active: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if position_ms >= line.time_ms {
            if !line.text.trim_start().starts_with('<') {
                active = Some(i);
            }
        } else {
            break;
        }
    }
    active
}

/// Word progress 0.0..=1.0 for karaoke fill.
pub fn word_progress(word: &WordData, position_ms: u64) -> f32 {
    if word.end_ms <= word.time_ms {
        return if position_ms >= word.time_ms { 1.0 } else { 0.0 };
    }
    if position_ms >= word.end_ms {
        1.0
    } else if position_ms >= word.time_ms {
        (position_ms - word.time_ms) as f32 / (word.end_ms - word.time_ms) as f32
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Vec<SyncedLine> {
        vec![
            SyncedLine {
                time_ms: 0,
                duration_ms: Some(2000),
                text: "hello".into(),
                is_instrumental: false,
                words: Some(vec![WordData {
                    time_ms: 0,
                    end_ms: 1000,
                    text: "hello".into(),
                    is_background: false,
                }]),
            },
            SyncedLine {
                time_ms: 2000,
                duration_ms: Some(2000),
                text: "world".into(),
                is_instrumental: false,
                words: None,
            },
        ]
    }

    #[test]
    fn active_line_follows_audio_clock() {
        let l = lines();
        assert_eq!(active_line_index(&l, 0), Some(0));
        assert_eq!(active_line_index(&l, 1999), Some(0));
        assert_eq!(active_line_index(&l, 2000), Some(1));
    }

    #[test]
    fn offset_clamps_at_zero() {
        let shifted = apply_lyrics_offset(&lines(), -5000);
        assert_eq!(shifted[0].time_ms, 0);
        assert_eq!(shifted[1].time_ms, 0);
    }

    #[test]
    fn word_progress_karaoke_fill() {
        let w = WordData {
            time_ms: 1000,
            end_ms: 2000,
            text: "w".into(),
            is_background: false,
        };
        assert_eq!(word_progress(&w, 500), 0.0);
        assert_eq!(word_progress(&w, 1500), 0.5);
        assert_eq!(word_progress(&w, 2500), 1.0);
    }
}
