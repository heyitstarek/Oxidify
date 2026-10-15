use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumRef {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cover {
    pub small: Option<String>,
    pub medium: Option<String>,
    pub large: Option<String>,
}

impl Cover {
    pub fn best(&self) -> Option<&str> {
        self.large
            .as_deref()
            .or(self.medium.as_deref())
            .or(self.small.as_deref())
    }
}

/// Canonical track used across the app.
///
/// The contract shape (id, title, artist list, album, duration, cover) is
/// authoritative; compact UI projections (`artists: string`, single cover
/// URL) are built at the UI boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artists: Vec<ArtistRef>,
    pub album: Option<AlbumRef>,
    pub duration_ms: u64,
    pub cover: Option<Cover>,
    pub explicit: bool,
}

impl Track {
    pub fn artist_display(&self) -> String {
        self.artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artist_display_joins_names() {
        let t = Track {
            id: "t1".into(),
            title: "Song".into(),
            artists: vec![
                ArtistRef { id: "a1".into(), name: "A".into() },
                ArtistRef { id: "a2".into(), name: "B".into() },
            ],
            album: None,
            duration_ms: 180_000,
            cover: None,
            explicit: false,
        };
        assert_eq!(t.artist_display(), "A, B");
    }
}
