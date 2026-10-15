use std::path::PathBuf;

/// 20-minute cap: longer remote audio is streamed, never fully
/// background-cached.
pub const MAX_BACKGROUND_CACHE_TRACK_MS: u64 = 20 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheCategory {
    Covers,
    Audio,
    Lyrics,
    Metadata,
}

impl CacheCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Covers => "covers",
            Self::Audio => "audio",
            Self::Lyrics => "lyrics",
            Self::Metadata => "metadata",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CachePaths {
    root: PathBuf,
}

impl CachePaths {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn default_root(app: &str) -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "oxidify", app)
            .map(|d| d.cache_dir().to_path_buf())
    }

    pub fn file_for(&self, category: CacheCategory, key: &str) -> PathBuf {
        cache_file_for(&self.root, category, key)
    }
}

/// Filesystem-safe cache file derivation. Keys are track ids / hashes;
/// anything outside `[A-Za-z0-9_-]` is percent-encoded to avoid traversal.
pub fn cache_file_for(root: &std::path::Path, category: CacheCategory, key: &str) -> PathBuf {
    let mut safe = String::with_capacity(key.len());
    for b in key.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            safe.push(b as char);
        } else {
            safe.push_str(&format!("%{b:02X}"));
        }
    }
    root.join(category.as_str()).join(safe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn traversal_keys_are_escaped() {
        let p = cache_file_for(Path::new("/cache"), CacheCategory::Audio, "../../etc/passwd");
        assert!(!p.to_string_lossy().contains("../"));
        assert!(p.to_string_lossy().contains("audio"));
    }

    #[test]
    fn background_cache_cap_is_20min() {
        assert_eq!(MAX_BACKGROUND_CACHE_TRACK_MS, 1_200_000);
    }
}
