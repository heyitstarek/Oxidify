use super::track::Track;

/// Queue semantics:
/// - `current_index == -1` means empty.
/// - changing to a different track resets the position.
/// - clearing the queue keeps the current track as a 1-item queue.
#[derive(Debug, Clone, Default)]
pub struct QueueState {
    pub queue: Vec<Track>,
    pub current_index: i64,
}

impl QueueState {
    pub fn current(&self) -> Option<&Track> {
        if self.current_index < 0 {
            return None;
        }
        self.queue.get(self.current_index as usize)
    }

    pub fn set_queue(&mut self, queue: Vec<Track>, start_index: usize) {
        self.queue = queue;
        self.current_index = if self.queue.is_empty() {
            -1
        } else {
            start_index.min(self.queue.len() - 1) as i64
        };
    }
}

/// Repeat-aware next index. Returns `None` when playback should stop.
pub fn next_index(
    current: usize,
    len: usize,
    shuffle: bool,
    repeat: &super::playback::RepeatMode,
    rand_next: impl FnOnce(usize) -> usize,
) -> Option<usize> {
    use super::playback::RepeatMode;
    if len == 0 {
        return None;
    }
    if repeat == &RepeatMode::One {
        return Some(current.min(len - 1));
    }
    if shuffle {
        if len == 1 {
            return Some(0);
        }
        let mut n = rand_next(len);
        if n == current {
            n = (n + 1) % len;
        }
        return Some(n);
    }
    let nxt = current + 1;
    if nxt < len {
        Some(nxt)
    } else if repeat == &RepeatMode::All {
        Some(0)
    } else {
        None
    }
}

pub fn prev_index(current: usize, len: usize, position_ms: u64) -> Option<usize> {
    if len == 0 {
        return None;
    }
    // Matches common player behaviour: restart track if >3s in,
    // otherwise go to previous item.
    if position_ms > 3000 {
        return Some(current.min(len - 1));
    }
    if current > 0 {
        Some(current - 1)
    } else {
        Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::RepeatMode;

    #[test]
    fn next_stops_at_end_when_repeat_off() {
        assert_eq!(next_index(2, 3, false, &RepeatMode::Off, |_| 0), None);
        assert_eq!(
            next_index(2, 3, false, &RepeatMode::All, |_| 0),
            Some(0)
        );
        assert_eq!(
            next_index(1, 3, false, &RepeatMode::One, |_| 0),
            Some(1)
        );
    }

    #[test]
    fn prev_restarts_when_far_in() {
        assert_eq!(prev_index(2, 5, 5000), Some(2));
        assert_eq!(prev_index(2, 5, 1000), Some(1));
    }
}
