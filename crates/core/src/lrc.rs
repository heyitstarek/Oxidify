//! Line- and word-level lyric file parsing (LRC and LRC-like text).
//!
//! Supported: `[mm:ss.xx] line` (centisecond or millisecond precision),
//! multiple timestamps on one line, `[offset:+/-ms]` global shift, and
//! inline `<mm:ss.xx>` word tags. Metadata tags (`[ar:]`, `[ti:]`, …) are
//! ignored. Malformed lines are skipped; a file with no valid timestamps
//! yields plain unsynced lines.

use super::{SyncedLine, WordData};

/// Parse lyric file text into synced lines. `song_duration_ms` caps absurd
/// trailing timestamps; pass 0 to skip capping.
pub fn parse_lrc(raw: &str, song_duration_ms: u64) -> Vec<SyncedLine> {
    let mut global_offset_ms: i64 = 0;
    let mut timed: Vec<(u64, String)> = Vec::new();
    let mut plain: Vec<String> = Vec::new();

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // [offset:+500] / [offset:-200]
        if let Some(rest) = line.strip_prefix("[offset:").and_then(|s| s.strip_suffix(']')) {
            if let Ok(v) = rest.trim().parse::<i64>() {
                global_offset_ms = v;
            }
            continue;
        }
        let (tags, text) = split_leading_tags(line);
        if tags.is_empty() {
            // Possible metadata tag or plain line.
            if looks_like_tag_line(line) {
                continue;
            }
            plain.push(line.to_string());
            continue;
        }
        let text = text.trim().to_string();
        for tag in tags {
            if let Some(ms) = parse_timestamp(&tag) {
                timed.push((ms, text.clone()));
            }
        }
    }

    if timed.is_empty() {
        return plain
            .into_iter()
            .map(|text| SyncedLine {
                time_ms: 0,
                duration_ms: None,
                text,
                is_instrumental: false,
                words: None,
            })
            .collect();
    }

    timed.sort_by_key(|(ms, _)| *ms);
    timed
        .into_iter()
        .map(|(ms, text)| {
            let mut ms = (ms as i64 + global_offset_ms).max(0) as u64;
            if song_duration_ms > 0 && ms > song_duration_ms {
                ms = song_duration_ms;
            }
            let (clean_text, words) = split_inline_words(&text, ms);
            SyncedLine {
                time_ms: ms,
                duration_ms: None,
                text: clean_text,
                is_instrumental: text_is_instrumental(&text),
                words,
            }
        })
        .collect()
}

/// Split `[..][..] remainder` into tags + remainder text.
fn split_leading_tags(line: &str) -> (Vec<String>, String) {
    let mut tags = Vec::new();
    let mut rest = line;
    loop {
        let r = rest.trim_start();
        if !r.starts_with('[') {
            break;
        }
        let Some(end) = r.find(']') else { break };
        let tag = r[1..end].to_string();
        // Only treat timestamp-shaped tags as line tags here.
        if parse_timestamp(&tag).is_some() {
            tags.push(tag);
            rest = &r[end + 1..];
        } else {
            break;
        }
    }
    (tags, rest.to_string())
}

fn looks_like_tag_line(line: &str) -> bool {
    line.starts_with('[') && line.contains(']') && line.contains(':')
}

/// Parse `mm:ss`, `mm:ss.x`, `mm:ss.xx`, `mm:ss.xxx`, `hh:mm:ss.xx`.
fn parse_timestamp(tag: &str) -> Option<u64> {
    let tag = tag.trim();
    let mut parts: Vec<&str> = tag.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let (h, m, s) = if parts.len() == 3 {
        let h: u64 = parts.remove(0).parse().ok()?;
        let m: u64 = parts.remove(0).parse().ok()?;
        (h, m, parts.remove(0))
    } else {
        let m: u64 = parts.remove(0).parse().ok()?;
        (0, m, parts.remove(0))
    };
    if m >= 100 && h == 0 {
        return None;
    }
    let (sec_part, frac_ms) = match s.split_once(['.', ',']) {
        Some((a, b)) => {
            let digits = b.trim();
            if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let scale = match digits.len() {
                1 => 100,
                2 => 10,
                3 => 1,
                _ => return None,
            };
            let v: u64 = digits.parse().ok()?;
            (a, v * scale)
        }
        None => (s, 0),
    };
    let sec: u64 = sec_part.parse().ok()?;
    if sec >= 60 {
        return None;
    }
    Some(h * 3_600_000 + m * 60_000 + sec * 1000 + frac_ms)
}

/// Split `words <t1>more words <t2>final` into clean text + word timings.
/// Text before a tag belongs to the previous timing (line time for the
/// first segment). A line without at least two timed segments is returned
/// as plain text with no word data.
fn split_inline_words(text: &str, line_ms: u64) -> (String, Option<Vec<WordData>>) {
    if !text.contains('<') {
        return (text.to_string(), None);
    }
    let mut segments: Vec<(u64, String)> = Vec::new();
    let mut pending_ms = line_ms;
    let mut buf = String::new();
    let mut clean = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '<' {
            buf.push(c);
            clean.push(c);
            continue;
        }
        let mut tag = String::new();
        let mut closed = false;
        for c2 in chars.by_ref() {
            if c2 == '>' {
                closed = true;
                break;
            }
            tag.push(c2);
        }
        if closed {
            if let Some(ms) = parse_timestamp(&tag) {
                if !buf.trim().is_empty() {
                    segments.push((pending_ms, std::mem::take(&mut buf)));
                } else {
                    buf.clear();
                }
                pending_ms = ms;
                continue;
            }
        }
        // Not a timestamp tag: keep it literally in both buffers.
        buf.push('<');
        buf.push_str(&tag);
        clean.push('<');
        clean.push_str(&tag);
        if closed {
            buf.push('>');
            clean.push('>');
        }
    }
    if !buf.trim().is_empty() {
        segments.push((pending_ms, buf));
    }
    if segments.len() < 2 {
        return (clean.trim().to_string(), None);
    }
    let mut chained: Vec<WordData> = segments
        .into_iter()
        .map(|(t, w)| WordData {
            time_ms: t,
            end_ms: t + 1500,
            text: w,
            is_background: false,
        })
        .collect();
    // Chain each word's end to the next word's start.
    for i in 0..chained.len() {
        let end = if i + 1 < chained.len() {
            chained[i + 1].time_ms
        } else {
            chained[i].time_ms + 1500
        };
        chained[i].end_ms = end.max(chained[i].time_ms);
    }
    (clean.trim().to_string(), Some(chained))
}

fn text_is_instrumental(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    t.contains("instrumental") || t == "♪" || t == "♫" || t == "..."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_lrc() {
        let lines = parse_lrc("[00:01.00]hello\n[00:03.50]world\n", 0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time_ms, 1000);
        assert_eq!(lines[1].time_ms, 3500);
        assert_eq!(lines[0].text, "hello");
    }

    #[test]
    fn multiple_tags_share_text() {
        let lines = parse_lrc("[00:01.00][00:05.00]repeat\n", 0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "repeat");
        assert_eq!(lines[1].time_ms, 5000);
    }

    #[test]
    fn offset_shifts_and_clamps() {
        let lines = parse_lrc("[offset:-2000]\n[00:01.00]a\n[00:10.00]b\n", 0);
        assert_eq!(lines[0].time_ms, 0);
        assert_eq!(lines[1].time_ms, 8000);
    }

    #[test]
    fn metadata_ignored_plain_kept_unsynced() {
        let lines = parse_lrc("[ar:Someone]\njust words\n", 0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].time_ms, 0);
        assert!(lines[0].words.is_none());
    }

    #[test]
    fn inline_word_tags_split() {
        let lines = parse_lrc("[00:01.00]hi <00:01.50>there\n", 0);
        assert_eq!(lines.len(), 1);
        let words = lines[0].words.as_ref().unwrap();
        assert!(words.len() >= 2);
        assert!(words[0].time_ms <= words[1].time_ms);
    }

    #[test]
    fn garbage_timestamps_rejected() {
        assert_eq!(parse_timestamp("ab:cd"), None);
        assert_eq!(parse_timestamp("99:99.99"), None);
        assert_eq!(parse_timestamp("01:02.0000"), None);
        assert_eq!(parse_timestamp("1:02.5"), Some(62_500));
        assert_eq!(parse_timestamp("01:02:03.25"), Some(3_723_250));
    }
}
