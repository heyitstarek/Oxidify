use serde::de::DeserializeOwned;
use serde::ser::Serialize;
use serde_json::Value;
use std::path::Path;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::client::{ApiError, CLIENT_VERSION};
use crate::{AuthTokens, AuthUser, Codec, PlaybackSession};

/// Async client for the remote catalog/service API.
///
/// The base URL is always explicit (no baked-in default); plain HTTP is
/// rejected except for loopback development URLs. Every request carries
/// `x-request-id`, `x-client-version`, and `x-platform`, plus
/// `Authorization: Bearer` when a token has been set.
pub struct RemoteClient {
    http: reqwest::Client,
    base_url: String,
    bearer: RwLock<Option<String>>,
}

impl std::fmt::Debug for RemoteClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteClient")
            .field("base_url", &self.base_url)
            .field("bearer", &"<redacted>")
            .finish()
    }
}

impl Clone for RemoteClient {
    fn clone(&self) -> Self {
        Self {
            http: self.http.clone(),
            base_url: self.base_url.clone(),
            bearer: RwLock::new(self.bearer.read().ok().and_then(|g| g.clone())),
        }
    }
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

fn uuid_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("oxidify-{nanos:x}")
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

fn login_request_body(email: &str, password: &str) -> Value {
    serde_json::json!({ "email": email, "password": password })
}

fn register_request_body(email: &str, password: &str, username: Option<&str>) -> Value {
    let mut body = serde_json::json!({ "email": email, "password": password });
    if let Some(name) = username {
        body["username"] = Value::String(name.to_string());
    }
    body
}

fn playback_request_body(codec: Option<Codec>) -> Value {
    serde_json::json!({
        "transport": "proxy",
        "preferredCodec": codec.map(|c| match c {
            Codec::Opus => "opus",
            Codec::Aac => "aac",
        }),
    })
}

impl RemoteClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self, ApiError> {
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
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| ApiError::Network(e.to_string()))?;
        Ok(Self {
            http,
            base_url: url,
            bearer: RwLock::new(None),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn set_bearer(&self, token: Option<String>) {
        if let Ok(mut guard) = self.bearer.write() {
            *guard = token;
        }
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

    fn authed(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut b = builder
            .header("x-request-id", uuid_simple())
            .header("x-client-version", CLIENT_VERSION)
            .header("x-platform", platform());
        if let Ok(guard) = self.bearer.read() {
            if let Some(token) = guard.as_deref() {
                b = b.header("Authorization", format!("Bearer {token}"));
            }
        }
        b
    }

    async fn read_json<T: DeserializeOwned>(res: reqwest::Response) -> Result<T, ApiError> {
        let status = res.status();
        if !status.is_success() {
            let message = res
                .text()
                .await
                .unwrap_or_else(|_| status.to_string())
                .chars()
                .take(500)
                .collect();
            return Err(ApiError::Http {
                status: status.as_u16(),
                message,
            });
        }
        res.json()
            .await
            .map_err(|e| ApiError::Network(format!("invalid response: {e}")))
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, ApiError> {
        let req = self.authed(self.http.get(format!("{}{path}", self.base_url)));
        let res = req
            .query(query)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;
        Self::read_json(res).await
    }

    async fn post_json<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let req = self.authed(self.http.post(format!("{}{path}", self.base_url)));
        let res = req
            .json(body)
            .send()
            .await
            .map_err(|e| ApiError::Network(e.to_string()))?;
        Self::read_json(res).await
    }

    /// POST /v1/auth/login. On success the access token becomes the client
    /// bearer automatically.
    pub async fn login(&self, email: &str, password: &str) -> Result<AuthTokens, ApiError> {
        let tokens: AuthTokens = self
            .post_json("/v1/auth/login", &login_request_body(email, password))
            .await?;
        self.set_bearer(Some(tokens.access_token.clone()));
        Ok(tokens)
    }

    /// POST /v1/auth/register. On success the access token becomes the
    /// client bearer automatically.
    pub async fn register(&self, email: &str, password: &str) -> Result<AuthTokens, ApiError> {
        let tokens: AuthTokens = self
            .post_json(
                "/v1/auth/register",
                &register_request_body(email, password, None),
            )
            .await?;
        self.set_bearer(Some(tokens.access_token.clone()));
        Ok(tokens)
    }

    /// POST /v1/auth/register with an explicit display name.
    pub async fn register_with_username(
        &self,
        email: &str,
        password: &str,
        username: &str,
    ) -> Result<AuthTokens, ApiError> {
        let tokens: AuthTokens = self
            .post_json(
                "/v1/auth/register",
                &register_request_body(email, password, Some(username)),
            )
            .await?;
        self.set_bearer(Some(tokens.access_token.clone()));
        Ok(tokens)
    }

    /// GET /v1/me.
    pub async fn me(&self) -> Result<AuthUser, ApiError> {
        self.get_json("/v1/me", &[]).await
    }

    /// GET /v1/search?q=.
    pub async fn search(&self, q: &str) -> Result<Value, ApiError> {
        self.get_json("/v1/search", &[("q", q)]).await
    }

    /// GET /v1/tracks/{id}.
    pub async fn track_details(&self, id: &str) -> Result<Value, ApiError> {
        self.get_json(&format!("/v1/tracks/{}", url_encode(id)), &[])
            .await
    }

    /// POST /v1/tracks/{id}/playback with `{"transport":"proxy"}`.
    pub async fn playback_session(
        &self,
        id: &str,
        codec: Option<Codec>,
    ) -> Result<PlaybackSession, ApiError> {
        self.post_json(
            &format!("/v1/tracks/{}/playback", url_encode(id)),
            &playback_request_body(codec),
        )
        .await
    }

    /// GET /v1/tracks/{id}/lyrics. The body is `text/event-stream`; frames
    /// are split with the shared SSE splitter and each `data:` payload is
    /// parsed as JSON. Malformed frames are discarded.
    pub async fn lyric_frames(&self, id: &str) -> Result<Vec<Value>, ApiError> {
        let req = self.authed(
            self.http
                .get(format!("{}/v1/tracks/{}/lyrics", self.base_url, url_encode(id)))
                .header("Accept", "text/event-stream"),
        );
        let mut res = req.send().await.map_err(|e| ApiError::Network(e.to_string()))?;
        let status = res.status();
        if !status.is_success() {
            return Err(ApiError::Http {
                status: status.as_u16(),
                message: status.to_string(),
            });
        }
        let mut buf = Vec::new();
        loop {
            match res.chunk().await {
                Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
                Ok(None) => break,
                Err(e) => return Err(ApiError::Network(e.to_string())),
            }
        }
        let text = String::from_utf8_lossy(&buf);
        Ok(crate::lyrics_stream::split_sse_data_frames(&text)
            .iter()
            .filter_map(|frame| serde_json::from_str::<Value>(frame).ok())
            .collect())
    }

    /// Download `url` (absolute, or resolved against the base URL) to
    /// `dest`. With `range_from`, resumes via `Range: bytes=<from>-`.
    /// Returns the number of bytes written.
    pub async fn download_to_file(
        &self,
        url: &str,
        dest: &Path,
        range_from: Option<u64>,
    ) -> Result<u64, ApiError> {
        let full = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            self.media_url(url)
        };
        let mut req = self.authed(self.http.get(full));
        if let Some(from) = range_from {
            req = req.header("Range", format!("bytes={from}-"));
        }
        let mut res = req.send().await.map_err(|e| ApiError::Network(e.to_string()))?;
        let status = res.status();
        if !(status.is_success() || status.as_u16() == 206) {
            return Err(ApiError::Http {
                status: status.as_u16(),
                message: status.to_string(),
            });
        }
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ApiError::Network(format!("io: {e}")))?;
            }
        }
        let mut file = tokio::fs::File::create(dest)
            .await
            .map_err(|e| ApiError::Network(format!("io: {e}")))?;
        let mut written: u64 = 0;
        loop {
            use tokio::io::AsyncWriteExt;
            match res.chunk().await {
                Ok(Some(chunk)) => {
                    file.write_all(&chunk)
                        .await
                        .map_err(|e| ApiError::Network(format!("io: {e}")))?;
                    written += chunk.len() as u64;
                }
                Ok(None) => break,
                Err(e) => return Err(ApiError::Network(e.to_string())),
            }
        }
        Ok(written)
    }
}

/// Best-effort check for proactive token refresh: true when the token is
/// expired or expires within ~60s. Unknown formats return true (refresh).
pub fn needs_refresh(expires_at: &str) -> bool {
    match parse_expiry_ms(expires_at) {
        Some(ms) => ms.saturating_sub(now_ms()) < 60_000,
        None => true,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse an expiry timestamp to unix milliseconds. Accepts unix seconds or
/// milliseconds (disambiguated by magnitude) and RFC3339 UTC/offset forms.
fn parse_expiry_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.chars().all(|c| c.is_ascii_digit() || c == '+' || c == '-') {
        let v: i64 = s.parse().ok()?;
        // Seconds-vs-milliseconds disambiguation: every realistic millis
        // timestamp (post-1973) is >= 1e11, every realistic seconds
        // timestamp (pre-5138) is < 1e11.
        if v.unsigned_abs() >= 100_000_000_000 {
            return Some(v);
        }
        return v.checked_mul(1000);
    }
    rfc3339_to_ms(s)
}

/// Dependency-free RFC3339 reader (`YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM]`).
fn rfc3339_to_ms(s: &str) -> Option<i64> {
    let (date, time) = s.split_once(['T', 't', ' '])?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if d.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Split off the timezone suffix.
    let (clock, offset_s) = if let Some(t) = time.strip_suffix(['Z', 'z']) {
        (t, 0)
    } else {
        let idx = time.rfind(['+', '-'])?;
        let (c, z) = time.split_at(idx);
        (c, parse_tz_offset_s(z)?)
    };
    let mut c = clock.split(':');
    let hour: i64 = c.next()?.parse().ok()?;
    let min: i64 = c.next()?.parse().ok()?;
    let sec_frac = c.next()?;
    if c.next().is_some() || hour > 23 || min > 59 {
        return None;
    }
    let (sec_s, frac_ms) = match sec_frac.split_once('.') {
        Some((ss, frac)) => {
            let frac = frac.trim_end_matches('Z');
            let scale = match frac.len() {
                0 => 0,
                1 => 100,
                2 => 10,
                _ => 1,
            };
            let digits: i64 = frac.get(..3.min(frac.len()))?.parse().ok()?;
            let ms = if frac.len() <= 3 {
                digits * scale
            } else {
                digits
            };
            (ss, ms)
        }
        None => (sec_frac, 0),
    };
    let sec: i64 = sec_s.parse().ok()?;
    if sec > 60 || frac_ms < 0 || frac_ms > 999 {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let epoch_s = days * 86_400 + hour * 3600 + min * 60 + sec - offset_s;
    epoch_s.checked_mul(1000)?.checked_add(frac_ms)
}

fn parse_tz_offset_s(z: &str) -> Option<i64> {
    let (sign, rest) = match z.strip_prefix('+') {
        Some(r) => (1i64, r),
        None => (-1i64, z.strip_prefix('-')?),
    };
    let (hh, mm) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None if rest.len() == 4 => (
            rest.get(..2)?.parse::<i64>().ok()?,
            rest.get(2..)?.parse::<i64>().ok()?,
        ),
        None => (rest.parse::<i64>().ok()?, 0),
    };
    if hh.abs() > 23 || !(0..60).contains(&mm.abs()) {
        return None;
    }
    Some(sign * (hh * 3600 + mm * 60))
}

/// Days since the unix epoch (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=9999).contains(&y) {
        return None;
    }
    let dim = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => return None,
    };
    if d > dim {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9).rem_euclid(12);
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_plaintext_remote() {
        assert!(RemoteClient::new("http://api.example.com").is_err());
        assert!(RemoteClient::new("https://music.example.com/").is_ok());
        assert!(RemoteClient::new("http://localhost:4317").is_ok());
        assert!(RemoteClient::new("http://127.0.0.1:4317/v1").is_ok());
    }

    #[test]
    fn media_url_joins_relative() {
        let c = RemoteClient::new("https://music.example.com").unwrap();
        assert_eq!(
            c.media_url("/stream/abc"),
            "https://music.example.com/stream/abc"
        );
        assert_eq!(
            c.media_url("stream/abc"),
            "https://music.example.com/stream/abc"
        );
        assert_eq!(
            c.media_url("https://cdn.example.com/a"),
            "https://cdn.example.com/a"
        );
    }

    #[test]
    fn bearer_can_be_set_and_cleared_headless() {
        let c = RemoteClient::new("https://music.example.com").unwrap();
        c.set_bearer(Some("tok-1".into()));
        c.set_bearer(None);
        let d = c.clone();
        assert_eq!(d.base_url(), "https://music.example.com");
        assert!(format!("{d:?}").contains("<redacted>"));
    }

    #[test]
    fn sse_frames_skip_comments_and_malformed() {
        let chunk = ": keep-alive\n\
             data: {\"type\":\"metadata\",\"track\":\"t1\"}\n\
             \n\
             : another comment\n\
             data: not json {{\n\
             \n\
             data: {\"type\":\"done\",\n\
             data: \"cached\":true}\n\
             \n";
        let frames = crate::lyrics_stream::split_sse_data_frames(chunk);
        assert_eq!(frames.len(), 3);
        let parsed: Vec<Value> = frames
            .iter()
            .filter_map(|f| serde_json::from_str::<Value>(f).ok())
            .collect();
        // Malformed frame is tolerated (dropped), multi-line frame joins.
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["type"], Value::String("metadata".into()));
    }

    #[test]
    fn sse_splitter_handles_crlf_and_multi_frame() {
        let chunk = "data: {\"a\":1}\r\n\r\ndata: {\"b\":2}\r\n\r\n";
        let frames = crate::lyrics_stream::split_sse_data_frames(chunk);
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn needs_refresh_cases() {
        assert!(needs_refresh(""));
        assert!(needs_refresh("not-a-date"));
        assert!(needs_refresh("2000-01-01T00:00:00Z"));
        assert!(!needs_refresh("2099-01-01T00:00:00Z"));
        assert!(!needs_refresh("2099-01-01T00:00:00.250Z"));
        assert!(!needs_refresh("2099-06-01T12:00:00+02:00"));
        // Numeric forms: past seconds/millis refresh, far-future do not.
        assert!(needs_refresh("946684800"));
        assert!(needs_refresh("946684800000"));
        assert!(!needs_refresh("4070908800"));
        assert!(!needs_refresh("4070908800000"));
        // Expiring within ~60s counts as needing refresh.
        let soon_ms = now_ms() + 10_000;
        assert!(needs_refresh(&soon_ms.to_string()));
        let later_ms = now_ms() + 3_600_000;
        assert!(!needs_refresh(&later_ms.to_string()));
        // Bad calendar values are unknown => refresh.
        assert!(needs_refresh("2026-13-01T00:00:00Z"));
        assert!(needs_refresh("2026-02-30T00:00:00Z"));
    }

    #[test]
    fn request_bodies_have_exact_shapes() {
        let login = login_request_body("a@b.c", "pw");
        assert_eq!(login["email"], Value::String("a@b.c".into()));
        assert_eq!(login["password"], Value::String("pw".into()));

        let reg = register_request_body("a@b.c", "pw", None);
        assert_eq!(reg["email"], Value::String("a@b.c".into()));
        assert!(reg.get("username").is_none());
        let reg2 = register_request_body("a@b.c", "pw", Some("ava"));
        assert_eq!(reg2["username"], Value::String("ava".into()));

        let pb = playback_request_body(None);
        assert_eq!(pb["transport"], Value::String("proxy".into()));
        assert!(pb["preferredCodec"].is_null());
        assert_eq!(
            playback_request_body(Some(Codec::Opus))["preferredCodec"],
            Value::String("opus".into())
        );
        assert_eq!(
            playback_request_body(Some(Codec::Aac))["preferredCodec"],
            Value::String("aac".into())
        );
    }
}
