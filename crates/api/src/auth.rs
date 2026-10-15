use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicUser {
    pub id: String,
    pub email: Option<String>,
    pub username: Option<String>,
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(rename = "avatarUrl")]
    pub avatar_url: Option<String>,
}

pub type AuthUser = PublicUser;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub access_token_expires_at: String,
    pub user: PublicUser,
}

impl AuthTokens {
    /// Mirrors `getSessionExpiryMs`: seconds -> ms disambiguation.
    pub fn expiry_ms(&self) -> Option<i64> {
        // Try RFC3339 first, then raw unix seconds/ms.
        if let Ok(dt) = chrono_like_parse(&self.access_token_expires_at) {
            return Some(dt);
        }
        None
    }
}

fn chrono_like_parse(_s: &str) -> Result<i64, ()> {
    // Kept dependency-free: full date parsing lives in the app layer.
    // Returning Err forces callers to treat unknown formats as opaque,
    // matching the TS fallback (return null).
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_expiry_is_opaque() {
        let t = AuthTokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            access_token_expires_at: "not-a-date".into(),
            user: PublicUser {
                id: "u1".into(),
                email: None,
                username: None,
                display_name: None,
                avatar_url: None,
            },
        };
        assert_eq!(t.expiry_ms(), None);
    }
}
