use thiserror::Error;

pub const CLIENT_VERSION: &str = "0.1.0-oxidify";

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("network error: {0}")]
    Network(String),
    #[error("http {status}: {message}")]
    Http { status: u16, message: String },
    #[error("auth session changed while request was pending")]
    SessionChanged,
    #[error("aborted")]
    Aborted,
}

fn platform() -> &'static str {
    #[cfg(target_os = "windows")]
    return "windows";
    #[cfg(target_os = "macos")]
    return "macos";
    #[cfg(target_os = "linux")]
    return "linux";
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    return "desktop";
}

/// Minimal async client. Endpoint coverage grows with the milestones; this
/// revision proves the transport, headers, and playback-session contract.
#[derive(Debug, Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    pub base_url: String,
}

impl ApiClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        let mut url = base_url.into();
        while url.ends_with('/') {
            url.pop();
        }
        if !(url.starts_with("https://")
            || url.starts_with("http://localhost")
            || url.starts_with("http://127.0.0.1"))
        {
            return Err(ApiError::Network(
                "API URL must use HTTPS (HTTP loopback allowed in dev only)".into(),
            ));
        }
        Ok(Self { http, base_url: url })
    }

    fn correlation_headers(
        &self,
        builder: reqwest::RequestBuilder,
        access_token: Option<&str>,
        track_id: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut b = builder
            .header("x-request-id", uuid_simple())
            .header("x-client-version", CLIENT_VERSION)
            .header("x-platform", platform());
        if let Some(t) = access_token {
            b = b.header("Authorization", format!("Bearer {t}"));
        }
        if let Some(id) = track_id {
            b = b.header("x-session-id", id);
        }
        b
    }

    pub async fn create_playback_session(
        &self,
        access_token: &str,
        track_id: &str,
        preferred_codec: Option<crate::playback::Codec>,
    ) -> Result<crate::playback::PlaybackSession, ApiError> {
        let body = serde_json::json!({
            "transport": "proxy",
            "preferredCodec": preferred_codec.map(|c| match c {
                crate::playback::Codec::Opus => "opus",
                crate::playback::Codec::Aac => "aac",
            }),
        });
        let req = self.correlation_headers(
            self.http.post(format!(
                "{}/v1/tracks/{}/playback",
                self.base_url,
                url_encode(track_id)
            )),
            Some(access_token),
            Some(track_id),
        );
        let res = req
            .json(&body)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;
        let status = res.status();
        if !status.is_success() {
            return Err(ApiError::Http {
                status: status.as_u16(),
                message: status.to_string(),
            });
        }
        res.json()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))
    }

    pub fn media_url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            return path.to_string();
        }
        if let Some(p) = path.strip_prefix('/') {
            format!("{}/{}", self.base_url, p)
        } else {
            format!("{}/{}", self.base_url, path)
        }
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn uuid_simple() -> String {
    // Uniqueness here only needs to be per-request for tracing,
    // not cryptographic.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("oxidify-{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_plaintext_remote() {
        assert!(ApiClient::new("http://api.example.com").is_err());
        assert!(ApiClient::new("https://music.example.com/").is_ok());
    }

    #[test]
    fn media_url_joins_relative() {
        let c = ApiClient::new("https://music.example.com").unwrap();
        assert_eq!(
            c.media_url("/stream/abc"),
            "https://music.example.com/stream/abc"
        );
        assert_eq!(
            c.media_url("https://cdn.example.com/a"),
            "https://cdn.example.com/a"
        );
    }
}
