//! Spotify Web API calls on SpoTerm's behalf, so the Python UI needs no TLS or HTTP stack.
//!
//! Uses the token SpoTerm's own login wrote (your app's client id, PKCE) and refreshes it
//! in place. Requests run concurrently; each answers with one `api` event.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

const API: &str = "https://api.spotify.com/";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY: usize = 16 << 20;
const REFRESH_MARGIN: f64 = 60.0;
const MAX_WAIT_429: u64 = 5;
const MALFORMED: &str = "Spotify sent a malformed response";

/// The token file, in the format SpoTerm's login writes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Token {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_at: f64,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    pkce: bool,
    #[serde(default = "bearer")]
    token_type: String,
}

fn bearer() -> String {
    "Bearer".into()
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub struct Web {
    http: reqwest::Client,
    client_id: String,
    path: PathBuf,
    token: Mutex<Option<Token>>,
}

/// A failed call: `status` 0 means it never got an HTTP answer.
#[derive(Debug)]
pub struct Failure {
    pub status: u16,
    pub msg: String,
    pub body: Value,
    pub retry_after: u64,
}

impl Failure {
    fn net(msg: impl Into<String>) -> Self {
        Failure {
            status: 0,
            msg: msg.into(),
            body: Value::Null,
            retry_after: 0,
        }
    }
}

impl Web {
    pub fn new(client_id: String, path: PathBuf) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(TIMEOUT)
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(2)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Web {
            http,
            client_id,
            path,
            token: Mutex::new(None),
        })
    }

    /// A valid access token, refreshing it first when it's about to expire or was rejected.
    async fn access(&self, rejected: Option<&str>) -> Result<String, Failure> {
        let mut slot = self.token.lock().await;
        if slot.is_none() || rejected.is_some() {
            // The file may have been rewritten by a fresh login: prefer what's on disk.
            if let Some(disk) = self.read() {
                if Some(disk.access_token.as_str()) != rejected {
                    *slot = Some(disk);
                }
            }
        }
        let tok = slot.clone().ok_or_else(|| login_needed("not signed in"))?;
        let stale = tok.expires_at - now() < REFRESH_MARGIN;
        if !stale && Some(tok.access_token.as_str()) != rejected {
            return Ok(tok.access_token);
        }
        let fresh = self.refresh(&tok).await?;
        let access = fresh.access_token.clone();
        self.write(&fresh);
        *slot = Some(fresh);
        Ok(access)
    }

    async fn refresh(&self, tok: &Token) -> Result<Token, Failure> {
        if tok.refresh_token.is_empty() {
            return Err(login_needed("sign-in expired"));
        }
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", tok.refresh_token.as_str()),
            ("client_id", self.client_id.as_str()),
        ];
        let resp = self
            .http
            .post(TOKEN_URL)
            .form(&form)
            .send()
            .await
            .map_err(net_failure)?;
        let status = resp.status().as_u16();
        let body = read_json(resp).await?;
        if status != 200 {
            // invalid_grant: the refresh token was revoked or belongs to another client.
            return Err(if status == 400 || status == 401 {
                login_needed("sign-in expired")
            } else {
                Failure {
                    status,
                    msg: format!("token refresh failed ({status})"),
                    body,
                    retry_after: 0,
                }
            });
        }
        let access = body["access_token"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if access.is_empty() {
            return Err(Failure::net("token refresh returned no token"));
        }
        Ok(Token {
            access_token: access,
            refresh_token: body["refresh_token"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| tok.refresh_token.clone()),
            expires_at: now() + body["expires_in"].as_f64().unwrap_or(3600.0),
            scope: body["scope"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| tok.scope.clone()),
            pkce: tok.pkce,
            token_type: "Bearer".into(),
        })
    }

    fn read(&self) -> Option<Token> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Atomic replace, so a crash mid-write can't leave a half-written token.
    fn write(&self, tok: &Token) {
        let tmp = self.path.with_extension("json.tmp");
        let mut v = serde_json::to_value(tok).unwrap_or(Value::Null);
        v["expires_in"] = json!((tok.expires_at - now()).max(0.0) as u64);
        v["expires_at"] = json!(tok.expires_at as u64);
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
            }
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    /// One Web API call. `path` is relative to /v1/, or a full api.spotify.com URL (paging).
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        query: Option<Map<String, Value>>,
        body: Option<Value>,
    ) -> Result<(u16, Value), Failure> {
        let url = if path.starts_with("https://") {
            if !path.starts_with(API) {
                return Err(Failure::net("refusing a link to another host"));
            }
            path.to_string()
        } else {
            format!("{API}v1/{}", path.trim_start_matches('/'))
        };
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| Failure::net("bad method"))?;
        let idempotent = method != reqwest::Method::POST;
        let query: Vec<(String, String)> = query
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| {
                (
                    k,
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string()),
                )
            })
            .collect();

        let mut rejected: Option<String> = None;
        let mut retries = Retries::default();
        loop {
            let token = self.access(rejected.as_deref()).await?;
            let mut req = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(&token)
                .query(&query);
            req = match &body {
                Some(b) => req
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(serde_json::to_vec(b).unwrap_or_default()),
                None if method == reqwest::Method::PUT || method == reqwest::Method::POST => {
                    req.header(reqwest::header::CONTENT_LENGTH, "0")
                }
                None => req,
            };
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    if let Some(wait) = after_network_error(&mut retries, idempotent) {
                        log::warn!("retrying after network error: {e}");
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    return Err(net_failure(e));
                }
            };
            let status = resp.status().as_u16();
            if status == 401 && rejected.is_none() {
                rejected = Some(token); // expired or revoked early: refresh once and retry
                continue;
            }
            let retry_after = parse_retry_after(
                resp.headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok()),
            );
            if let Some(wait) = after_status(&mut retries, status, retry_after, idempotent) {
                tokio::time::sleep(wait).await;
                continue;
            }
            let body = read_body(resp).await?;
            return classify(status, retry_after, &body);
        }
    }
}

fn login_needed(msg: &str) -> Failure {
    Failure {
        status: 401,
        msg: msg.into(),
        body: json!({"login": true}),
        retry_after: 0,
    }
}

fn net_failure(e: reqwest::Error) -> Failure {
    Failure::net(if e.is_timeout() {
        "Spotify took too long to respond".to_string()
    } else if e.is_connect() {
        "Network error: can't reach Spotify".to_string()
    } else {
        format!("Network error: {}", e.without_url())
    })
}

/// Pure: header value to seconds (today's inline `RETRY_AFTER` parsing).
fn parse_retry_after(header: Option<&str>) -> u64 {
    header
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

const MAX_RETRIES_PER_KIND: u8 = 1;

#[derive(Default, Debug, PartialEq)]
struct Retries {
    net: u8,
    rate: u8,
    server: u8,
}

/// After a transport error (no HTTP answer). Some(wait) means retry after `wait`.
fn after_network_error(r: &mut Retries, idempotent: bool) -> Option<Duration> {
    if idempotent && r.net < MAX_RETRIES_PER_KIND {
        r.net += 1;
        Some(Duration::from_millis(500))
    } else {
        None
    }
}

/// After an HTTP status. Some(wait) means retry after `wait`.
fn after_status(
    r: &mut Retries,
    status: u16,
    retry_after: u64,
    idempotent: bool,
) -> Option<Duration> {
    if status == 429 {
        if retry_after <= MAX_WAIT_429 && r.rate < MAX_RETRIES_PER_KIND {
            r.rate += 1;
            Some(Duration::from_secs(retry_after.max(1)))
        } else {
            None
        }
    } else if status >= 500 {
        if idempotent && r.server < MAX_RETRIES_PER_KIND {
            r.server += 1;
            Some(Duration::from_millis(500))
        } else {
            None
        }
    } else {
        None
    }
}

/// Size-limited body read (the streaming part of today's `read_json`).
async fn read_body(mut resp: reqwest::Response) -> Result<Vec<u8>, Failure> {
    if resp.content_length().is_some_and(|n| n as usize > MAX_BODY) {
        return Err(Failure::net("response too large"));
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(net_failure)? {
        if buf.len() + chunk.len() > MAX_BODY {
            return Err(Failure::net("response too large"));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Pure: empty or whitespace is null; otherwise JSON, else the MALFORMED failure.
fn parse_body(buf: &[u8]) -> Result<Value, Failure> {
    if buf.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(buf).map_err(|_| Failure::net(MALFORMED))
}

/// Pure: today's decision logic, unchanged.
fn classify(status: u16, retry_after: u64, body: &[u8]) -> Result<(u16, Value), Failure> {
    let value = match parse_body(body) {
        // Player commands (shuffle, repeat, pause, ...) now answer 200 with a bare
        // text id rather than JSON or nothing: that is still a success.
        Err(f) if f.status == 0 && f.msg == MALFORMED && status < 400 => Value::Null,
        other => other?,
    };
    if status >= 400 {
        let msg = value["error"]["message"]
            .as_str()
            .or_else(|| value["error_description"].as_str())
            .or_else(|| value["error"].as_str())
            .unwrap_or("request failed")
            .to_string();
        return Err(Failure {
            status,
            msg,
            body: value,
            retry_after,
        });
    }
    Ok((status, value))
}

/// The body as JSON (empty is null), refusing anything over MAX_BODY.
async fn read_json(resp: reqwest::Response) -> Result<Value, Failure> {
    let body = read_body(resp).await?;
    parse_body(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_200_json_object() {
        let (status, val) = classify(200, 0, b"{\"foo\":\"bar\"}").unwrap();
        assert_eq!(status, 200);
        assert_eq!(val["foo"], "bar");
    }

    #[test]
    fn classify_200_empty_whitespace_and_204() {
        let (s1, v1) = classify(200, 0, b"").unwrap();
        assert_eq!(s1, 200);
        assert_eq!(v1, Value::Null);
        let (s2, v2) = classify(200, 0, b"   \r\n\t ").unwrap();
        assert_eq!(s2, 200);
        assert_eq!(v2, Value::Null);
        let (s3, v3) = classify(204, 0, b"").unwrap();
        assert_eq!(s3, 204);
        assert_eq!(v3, Value::Null);
    }

    #[test]
    fn classify_200_bare_text_id() {
        let (status, val) = classify(200, 0, b"6mdiAmATAx73kdxrNrnlao").unwrap();
        assert_eq!(status, 200);
        assert_eq!(val, Value::Null);
    }

    #[test]
    fn classify_400_with_error_object() {
        let err =
            classify(400, 0, b"{\"error\":{\"status\":400,\"message\":\"bad\"}}").unwrap_err();
        assert_eq!(err.status, 400);
        assert_eq!(err.msg, "bad");
    }

    #[test]
    fn classify_401_with_error_description() {
        let err = classify(
            401,
            0,
            b"{\"error\":\"invalid_grant\",\"error_description\":\"gone\"}",
        )
        .unwrap_err();
        assert_eq!(err.status, 401);
        assert_eq!(err.msg, "gone");
    }

    #[test]
    fn classify_error_is_only_string() {
        let err = classify(400, 0, b"{\"error\":\"oops\"}").unwrap_err();
        assert_eq!(err.status, 400);
        assert_eq!(err.msg, "oops");
    }

    #[test]
    fn classify_json_error_no_usable_message() {
        let err = classify(400, 0, b"{\"other\":\"field\"}").unwrap_err();
        assert_eq!(err.status, 400);
        assert_eq!(err.msg, "request failed");
    }

    #[test]
    fn classify_429_json_with_retry_after() {
        let err = classify(429, 7, b"{\"error\":{\"message\":\"slow down\"}}").unwrap_err();
        assert_eq!(err.status, 429);
        assert_eq!(err.retry_after, 7);
        assert_eq!(err.msg, "slow down");
    }

    #[test]
    fn classify_500_json() {
        let err = classify(500, 0, b"{\"error\":{\"message\":\"server error\"}}").unwrap_err();
        assert_eq!(err.status, 500);
        assert_eq!(err.msg, "server error");
    }

    #[test]
    fn classify_200_array_number_string() {
        let (s_arr, v_arr) = classify(200, 0, b"[1, 2, 3]").unwrap();
        assert_eq!(s_arr, 200);
        assert_eq!(v_arr, json!([1, 2, 3]));

        let (s_num, v_num) = classify(200, 0, b"42").unwrap();
        assert_eq!(s_num, 200);
        assert_eq!(v_num, json!(42));

        let (s_str, v_str) = classify(200, 0, b"\"hello\"").unwrap();
        assert_eq!(s_str, 200);
        assert_eq!(v_str, json!("hello"));
    }

    #[test]
    fn parse_body_nested_array_limit() {
        let bad = vec![b'['; 10_000];
        let err = parse_body(&bad).unwrap_err();
        assert_eq!(err.msg, MALFORMED);
    }

    #[test]
    fn parse_body_invalid_utf8() {
        let bad = &[0xFF, 0xFE, 0xFD];
        let res = parse_body(bad);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().msg, MALFORMED);
    }

    #[test]
    fn bug_02_current_behavior_429_non_json() {
        // BUG-02: current behavior: non-JSON 429 body drops status 429 and retry_after, returning status 0
        let err = classify(429, 7, b"API rate limit exceeded").unwrap_err();
        assert_eq!(err.status, 0);
        assert_eq!(err.msg, MALFORMED);
    }

    #[test]
    #[ignore = "BUG-02"]
    fn bug_02_desired_behavior_429_non_json() {
        let err = classify(429, 7, b"API rate limit exceeded").unwrap_err();
        assert_eq!(err.status, 429);
        assert_eq!(err.retry_after, 7);
    }

    #[test]
    fn test_parse_retry_after() {
        assert_eq!(parse_retry_after(Some("30")), 30);
        assert_eq!(parse_retry_after(Some(" 30 ")), 30);
        assert_eq!(parse_retry_after(Some("abc")), 0);
        assert_eq!(parse_retry_after(Some("-1")), 0);
        assert_eq!(parse_retry_after(Some("")), 0);
        assert_eq!(parse_retry_after(None), 0);
        assert_eq!(parse_retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")), 0);
    }

    #[test]
    fn token_file_contract_python_format() {
        let json_str = r#"{"access_token":"a","token_type":"Bearer","expires_in":3599,"expires_at":1790000000,"refresh_token":"r","scope":"streaming user-library-read","pkce":true}"#;
        let tok: Token = serde_json::from_str(json_str).unwrap();
        assert_eq!(tok.access_token, "a");
        assert_eq!(tok.token_type, "Bearer");
        assert_eq!(tok.expires_at, 1790000000.0);
        assert_eq!(tok.refresh_token, "r");
        assert_eq!(tok.scope, "streaming user-library-read");
        assert!(tok.pkce);
    }

    #[test]
    fn token_file_contract_minimal_defaults() {
        let json_str = r#"{"access_token":"a"}"#;
        let tok: Token = serde_json::from_str(json_str).unwrap();
        assert_eq!(tok.access_token, "a");
        assert_eq!(tok.token_type, "Bearer");
        assert_eq!(tok.expires_at, 0.0);
        assert_eq!(tok.refresh_token, "");
        assert_eq!(tok.scope, "");
        assert!(!tok.pkce);
    }

    #[test]
    fn token_file_contract_missing_access_token_and_extra_keys() {
        assert!(serde_json::from_str::<Token>(r#"{"refresh_token":"r"}"#).is_err());
        let extra = r#"{"access_token":"a","unknown_key":"ignored","extra_num":123}"#;
        let tok: Token = serde_json::from_str(extra).unwrap();
        assert_eq!(tok.access_token, "a");
    }

    #[test]
    fn token_file_contract_roundtrip() {
        let tok = Token {
            access_token: "tok123".into(),
            refresh_token: "ref456".into(),
            expires_at: 1234567.89,
            scope: "streaming".into(),
            pkce: true,
            token_type: "Bearer".into(),
        };
        let serialized = serde_json::to_string(&tok).unwrap();
        let parsed: Token = serde_json::from_str(&serialized).unwrap();
        assert_eq!(tok, parsed);
    }

    #[test]
    fn test_retries_sequence_get() {
        let mut r = Retries::default();
        // network error then 429 then 500 on a GET: each retried once (three retries in total)
        assert_eq!(
            after_network_error(&mut r, true),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            after_status(&mut r, 429, 2, true),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            after_status(&mut r, 500, 0, true),
            Some(Duration::from_millis(500))
        );
    }

    #[test]
    fn test_retries_two_network_errors() {
        let mut r = Retries::default();
        assert_eq!(
            after_network_error(&mut r, true),
            Some(Duration::from_millis(500))
        );
        assert_eq!(after_network_error(&mut r, true), None);
    }

    #[test]
    fn test_retries_two_429s() {
        let mut r = Retries::default();
        assert_eq!(
            after_status(&mut r, 429, 2, true),
            Some(Duration::from_secs(2))
        );
        assert_eq!(after_status(&mut r, 429, 2, true), None);
    }

    #[test]
    fn test_retries_two_5xx() {
        let mut r = Retries::default();
        assert_eq!(
            after_status(&mut r, 500, 0, true),
            Some(Duration::from_millis(500))
        );
        assert_eq!(after_status(&mut r, 502, 0, true), None);
    }

    #[test]
    fn test_retries_post_not_idempotent() {
        let mut r = Retries::default();
        // POST: network error, 500: not retried
        assert_eq!(after_network_error(&mut r, false), None);
        assert_eq!(after_status(&mut r, 500, 0, false), None);
        // POST: 429: retried
        assert_eq!(
            after_status(&mut r, 429, 2, false),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn test_retries_429_delays_and_cap() {
        // 429 with retry_after 6 (above 5s cap): not retried
        let mut r1 = Retries::default();
        assert_eq!(after_status(&mut r1, 429, 6, true), None);

        // 429 with retry_after 0: retried after 1 s
        let mut r2 = Retries::default();
        assert_eq!(
            after_status(&mut r2, 429, 0, true),
            Some(Duration::from_secs(1))
        );

        // 429 with retry_after 3: retried after 3 s
        let mut r3 = Retries::default();
        assert_eq!(
            after_status(&mut r3, 429, 3, true),
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn test_retries_unretried_statuses() {
        for &st in &[200, 204, 301, 400, 401, 403, 404] {
            let mut r = Retries::default();
            assert_eq!(after_status(&mut r, st, 0, true), None);
        }
    }

    #[test]
    fn test_retries_5xx_statuses() {
        for &st in &[500, 502, 503, 599] {
            let mut r = Retries::default();
            assert_eq!(
                after_status(&mut r, st, 0, true),
                Some(Duration::from_millis(500))
            );
        }
    }

    #[test]
    fn test_retries_default_and_independent() {
        let r = Retries::default();
        assert_eq!(r.net, 0);
        assert_eq!(r.rate, 0);
        assert_eq!(r.server, 0);

        // Exhausting net doesn't affect rate or server
        let mut r2 = Retries {
            net: MAX_RETRIES_PER_KIND,
            ..Default::default()
        };
        assert_eq!(after_network_error(&mut r2, true), None);
        assert_eq!(
            after_status(&mut r2, 429, 2, true),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            after_status(&mut r2, 500, 0, true),
            Some(Duration::from_millis(500))
        );
    }
}
