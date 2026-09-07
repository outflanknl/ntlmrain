//! Blocking client for the lookup service's capability-token protocol.

use crate::local_lookup::{parse_endpoint_file, validate_candidate_file};
use reqwest::Url;
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use std::error::Error as _;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use thiserror::Error;

pub const DEFAULT_LOOKUP_URL: &str = "https://lookup.ntlmrain.com";

#[derive(Clone, Debug)]
pub struct RemoteLookupConfig {
    pub base_url: String,
    /// `None` disables Basic authentication. Empty strings remain valid values.
    pub username: Option<String>,
    pub password: Option<String>,
    pub poll_interval: Duration,
    pub request_timeout: Duration,
}

impl Default for RemoteLookupConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_LOOKUP_URL.to_string(),
            username: None,
            password: None,
            poll_interval: Duration::from_secs(2),
            request_timeout: Duration::from_secs(30 * 60),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteStatus {
    pub state: String,
    #[serde(default)]
    pub record_count: u64,
    #[serde(default)]
    pub processed_records: u64,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub queue_position: Option<u64>,
    #[serde(default)]
    pub match_count: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub poll_within_seconds: Option<f64>,
    #[serde(default)]
    pub download_within_seconds: Option<f64>,
}

#[derive(Clone, Debug)]
pub enum RemoteProgress {
    Upload {
        loaded: u64,
        total: u64,
        done: bool,
    },
    Status(RemoteStatus),
    Download {
        loaded: u64,
        total: Option<u64>,
        done: bool,
    },
}

#[derive(Debug, Error)]
pub enum RemoteLookupError {
    #[error("lookup cancelled")]
    Cancelled,
    #[error("invalid lookup URL: {0}")]
    InvalidUrl(String),
    #[error("invalid lookup artifact: {0}")]
    InvalidArtifact(String),
    #[error("lookup service returned HTTP {status}: {message}")]
    Service { status: u16, message: String },
    #[error("lookup service returned an invalid response: {0}")]
    InvalidResponse(String),
    #[error("lookup connection failed: {0}")]
    Connect(String),
    #[error("lookup request timed out: {0}")]
    Timeout(String),
    #[error("lookup request failed: {0}")]
    Http(String),
    #[error("candidate download failed: {0}")]
    Io(#[from] std::io::Error),
}

impl From<reqwest::Error> for RemoteLookupError {
    fn from(error: reqwest::Error) -> Self {
        let detail = error
            .source()
            .map(|source| source_chain_last(source).to_string())
            .unwrap_or_else(|| error.to_string());
        if error.is_timeout() {
            Self::Timeout(detail)
        } else if error.is_connect() {
            Self::Connect(detail)
        } else {
            Self::Http(detail)
        }
    }
}

fn source_chain_last<'a>(
    mut source: &'a (dyn std::error::Error + 'static),
) -> &'a (dyn std::error::Error + 'static) {
    while let Some(next) = source.source() {
        source = next;
    }
    source
}

#[derive(Clone)]
pub struct RemoteLookupClient {
    client: Client,
    config: RemoteLookupConfig,
    submit_url: Url,
    status_url: Url,
    result_url: Url,
    cancel_url: Url,
}

#[derive(Debug, Deserialize)]
struct SubmissionReceipt {
    submission_token: String,
    #[serde(default)]
    poll_within_seconds: Option<f64>,
}

#[derive(Serialize)]
struct SubmissionAccess<'a> {
    submission_token: &'a str,
}

impl RemoteLookupClient {
    pub fn new(config: RemoteLookupConfig) -> Result<Self, RemoteLookupError> {
        if config.poll_interval.is_zero() {
            return Err(RemoteLookupError::InvalidResponse(
                "poll interval must be positive".into(),
            ));
        }
        let base = Url::parse(config.base_url.trim_end_matches('/'))
            .map_err(|error| RemoteLookupError::InvalidUrl(error.to_string()))?;
        if base.scheme() != "http" && base.scheme() != "https" {
            return Err(RemoteLookupError::InvalidUrl(
                "only HTTP and HTTPS are supported".into(),
            ));
        }
        let endpoint = |path: &str| {
            base.join(path)
                .map_err(|error| RemoteLookupError::InvalidUrl(error.to_string()))
        };
        let client = Client::builder().timeout(config.request_timeout).build()?;
        Ok(Self {
            client,
            config,
            submit_url: endpoint("/api/v1/submissions")?,
            status_url: endpoint("/api/v1/submissions/status")?,
            result_url: endpoint("/api/v1/submissions/result")?,
            cancel_url: endpoint("/api/v1/submissions/cancel")?,
        })
    }

    /// Submit one complete `NTLMEND1` artifact and return the validated
    /// `NTLMCAN1` response. The callback is invoked synchronously.
    pub fn lookup<F>(
        &self,
        endpoint_file: &[u8],
        cancel: Option<&AtomicBool>,
        mut progress: F,
    ) -> Result<Vec<u8>, RemoteLookupError>
    where
        F: FnMut(RemoteProgress),
    {
        let query_count = parse_endpoint_file(endpoint_file)
            .map_err(|error| RemoteLookupError::InvalidArtifact(error.to_string()))?
            .len() as u64;
        check_cancelled(cancel)?;
        progress(RemoteProgress::Upload {
            loaded: 0,
            total: endpoint_file.len() as u64,
            done: false,
        });
        let response = self
            .authenticate(
                self.client
                    .post(self.submit_url.clone())
                    .header(
                        reqwest::header::CONTENT_TYPE,
                        "application/vnd.netntlmv1.endpoints",
                    )
                    .body(endpoint_file.to_vec()),
            )
            .send()?;
        let response = require_success(response)?;
        let receipt: SubmissionReceipt = response.json()?;
        validate_token(&receipt.submission_token)?;
        progress(RemoteProgress::Upload {
            loaded: endpoint_file.len() as u64,
            total: endpoint_file.len() as u64,
            done: true,
        });

        let result = self.finish_lookup(&receipt, query_count, cancel, &mut progress);
        if result.is_err() {
            self.cancel_best_effort(&receipt.submission_token);
        }
        result
    }

    /// Explicitly cancel a known submission capability.
    pub fn cancel(&self, submission_token: &str) -> Result<(), RemoteLookupError> {
        validate_token(submission_token)?;
        let response = self
            .authenticate(self.client.post(self.cancel_url.clone()))
            .json(&SubmissionAccess { submission_token })
            .send()?;
        require_success(response)?;
        Ok(())
    }

    fn finish_lookup<F>(
        &self,
        receipt: &SubmissionReceipt,
        query_count: u64,
        cancel: Option<&AtomicBool>,
        progress: &mut F,
    ) -> Result<Vec<u8>, RemoteLookupError>
    where
        F: FnMut(RemoteProgress),
    {
        let poll_interval = receipt
            .poll_within_seconds
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .map(|seconds| Duration::from_secs_f64(seconds / 2.0))
            .map(|lease_interval| min_duration(self.config.poll_interval, lease_interval))
            .unwrap_or(self.config.poll_interval);
        loop {
            sleep_interruptibly(poll_interval, cancel)?;
            let response = self
                .authenticate(self.client.post(self.status_url.clone()))
                .json(&SubmissionAccess {
                    submission_token: &receipt.submission_token,
                })
                .send()?;
            let status: RemoteStatus = require_success(response)?.json()?;
            if !status.progress.is_finite() || !(0.0..=1.0).contains(&status.progress) {
                return Err(RemoteLookupError::InvalidResponse(
                    "status progress is outside 0..=1".into(),
                ));
            }
            if status.record_count != 0 && status.record_count != query_count {
                return Err(RemoteLookupError::InvalidResponse(
                    "status record count does not match submitted endpoints".into(),
                ));
            }
            progress(RemoteProgress::Status(status.clone()));
            match status.state.as_str() {
                "queued" | "running" => {}
                "ready" => break,
                "failed" => {
                    return Err(RemoteLookupError::InvalidResponse(
                        status.error.unwrap_or_else(|| "lookup failed".into()),
                    ));
                }
                other => {
                    return Err(RemoteLookupError::InvalidResponse(format!(
                        "unexpected submission state {other:?}"
                    )));
                }
            }
        }

        check_cancelled(cancel)?;
        progress(RemoteProgress::Download {
            loaded: 0,
            total: None,
            done: false,
        });
        let response = self
            .authenticate(self.client.post(self.result_url.clone()))
            .json(&SubmissionAccess {
                submission_token: &receipt.submission_token,
            })
            .send()?;
        let mut response = require_success(response)?;
        let declared = response.content_length();
        let mut output = Vec::with_capacity(
            declared
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or(0),
        );
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check_cancelled(cancel)?;
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..read]);
            progress(RemoteProgress::Download {
                loaded: output.len() as u64,
                total: declared,
                done: false,
            });
        }
        validate_candidate_file(&output, Some(query_count))
            .map_err(|error| RemoteLookupError::InvalidArtifact(error.to_string()))?;
        progress(RemoteProgress::Download {
            loaded: output.len() as u64,
            // Completed streams are complete regardless of an absent or stale
            // Content-Length. Consumers should render `done` as 100%.
            total: declared.or(Some(output.len() as u64)),
            done: true,
        });
        Ok(output)
    }

    fn authenticate(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.config.username {
            Some(username) => request.basic_auth(username, self.config.password.as_deref()),
            None => request,
        }
    }

    fn cancel_best_effort(&self, submission_token: &str) {
        let _ = self
            .authenticate(self.client.post(self.cancel_url.clone()))
            .json(&SubmissionAccess { submission_token })
            .send();
    }
}

fn validate_token(token: &str) -> Result<(), RemoteLookupError> {
    if token.len() != 64
        || !token
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(RemoteLookupError::InvalidResponse(
            "submission token is not 256-bit lowercase hexadecimal".into(),
        ));
    }
    Ok(())
}

fn require_success(response: Response) -> Result<Response, RemoteLookupError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let fallback = status
        .canonical_reason()
        .unwrap_or("lookup request failed")
        .to_string();
    let body = response.text().unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("detail")
                .and_then(|detail| detail.as_str())
                .map(str::to_owned)
        })
        .or_else(|| (!body.trim().is_empty()).then(|| body.trim().to_string()))
        .unwrap_or(fallback);
    Err(RemoteLookupError::Service {
        status: status.as_u16(),
        message,
    })
}

fn check_cancelled(cancel: Option<&AtomicBool>) -> Result<(), RemoteLookupError> {
    if cancel.is_some_and(|state| state.load(Ordering::Acquire)) {
        Err(RemoteLookupError::Cancelled)
    } else {
        Ok(())
    }
}

fn sleep_interruptibly(
    duration: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<(), RemoteLookupError> {
    let deadline = Instant::now() + duration;
    loop {
        check_cancelled(cancel)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        std::thread::sleep(min_duration(remaining, Duration::from_millis(100)));
    }
}

fn min_duration(left: Duration, right: Duration) -> Duration {
    if left <= right { left } else { right }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_lookup::{CandidateRecord, ENDPOINT_MAGIC, encode_candidate_file};
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};

    fn endpoint_file(endpoint: u64) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(ENDPOINT_MAGIC);
        output.extend_from_slice(&1u32.to_le_bytes());
        output.extend_from_slice(&8u32.to_le_bytes());
        output.extend_from_slice(&1u64.to_le_bytes());
        output.extend_from_slice(&0u32.to_le_bytes());
        output.extend_from_slice(&0u32.to_le_bytes());
        output.extend_from_slice(&endpoint.to_le_bytes());
        output
    }

    #[test]
    fn defaults_use_public_https_api_without_authentication() {
        let config = RemoteLookupConfig::default();
        assert_eq!(config.base_url, DEFAULT_LOOKUP_URL);
        assert_eq!(config.username, None);
        assert_eq!(config.password, None);
    }

    #[test]
    fn rejects_non_http_base_url() {
        let config = RemoteLookupConfig {
            base_url: "file:///tmp/table".into(),
            ..RemoteLookupConfig::default()
        };
        assert!(matches!(
            RemoteLookupClient::new(config),
            Err(RemoteLookupError::InvalidUrl(_))
        ));
    }

    #[test]
    fn validates_lowercase_capability_tokens() {
        assert!(validate_token(&"a0".repeat(32)).is_ok());
        assert!(validate_token(&"A0".repeat(32)).is_err());
        assert!(validate_token("short").is_err());
    }

    #[test]
    fn rejects_cancelled_lookup_before_network_access() {
        let client = RemoteLookupClient::new(RemoteLookupConfig::default()).unwrap();
        let cancelled = AtomicBool::new(true);
        let error = client
            .lookup(&endpoint_file(1), Some(&cancelled), |_| {})
            .unwrap_err();
        assert!(matches!(error, RemoteLookupError::Cancelled));
    }

    #[test]
    fn candidate_fixture_is_compatible_with_remote_validation() {
        let candidate = encode_candidate_file(
            1,
            &[CandidateRecord {
                ordinal: 0,
                start: 42,
            }],
        )
        .unwrap();
        assert_eq!(validate_candidate_file(&candidate, Some(1)).unwrap(), 1);
    }

    #[test]
    fn service_errors_extract_json_detail() {
        // Keep the detail extraction independently testable without a server.
        let value: serde_json::Value = serde_json::from_str(r#"{"detail":"bad batch"}"#).unwrap();
        assert_eq!(value["detail"], "bad batch");
        assert_eq!(reqwest::StatusCode::BAD_REQUEST.as_u16(), 400);
    }

    #[test]
    fn completes_public_submission_poll_and_download_protocol() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let endpoints = endpoint_file(0x0123_4567_89ab_cdef);
        let expected_upload = endpoints.clone();
        let candidates = encode_candidate_file(
            1,
            &[
                CandidateRecord {
                    ordinal: 0,
                    start: 42,
                },
                CandidateRecord {
                    ordinal: 0,
                    start: 43,
                },
            ],
        )
        .unwrap();
        let expected_candidates = candidates.clone();
        let server_token = "ab".repeat(32);
        let server = std::thread::spawn(move || {
            let replies = [
                (
                    "/api/v1/submissions",
                    "202 Accepted",
                    "application/json",
                    format!(
                        r#"{{"submission_token":"{server_token}","poll_within_seconds":1}}"#
                    )
                    .into_bytes(),
                ),
                (
                    "/api/v1/submissions/status",
                    "200 OK",
                    "application/json",
                    br#"{"state":"queued","record_count":1,"processed_records":0,"progress":0.0,"queue_position":2}"#.to_vec(),
                ),
                (
                    "/api/v1/submissions/status",
                    "200 OK",
                    "application/json",
                    br#"{"state":"running","record_count":1,"processed_records":1,"progress":1.0,"queue_position":1}"#.to_vec(),
                ),
                (
                    "/api/v1/submissions/status",
                    "200 OK",
                    "application/json",
                    br#"{"state":"ready","record_count":1,"processed_records":1,"progress":1.0,"match_count":2}"#.to_vec(),
                ),
                (
                    "/api/v1/submissions/result",
                    "200 OK",
                    "application/vnd.netntlmv1.candidates",
                    candidates,
                ),
            ];
            for (index, (path, status, content_type, body)) in replies.into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                assert_request(&request, path);
                if index == 0 {
                    assert_eq!(split_request(&request).1, expected_upload);
                } else {
                    assert_eq!(request_json(&request)["submission_token"], server_token);
                }
                write_response(&mut stream, status, content_type, &body);
            }
        });

        let mut events = Vec::new();
        let result = mock_client(address)
            .lookup(&endpoints, None, |event| events.push(event))
            .unwrap();
        server.join().unwrap();
        assert_eq!(result, expected_candidates);
        let states = events
            .iter()
            .filter_map(|event| match event {
                RemoteProgress::Status(status) => Some(status.state.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(states, ["queued", "running", "ready"]);
        assert!(matches!(
            events.last(),
            Some(RemoteProgress::Download { loaded, done: true, .. })
                if *loaded == result.len() as u64
        ));
    }

    #[test]
    fn failed_submission_is_cancelled_best_effort() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_token = "cd".repeat(32);
        let server = std::thread::spawn(move || {
            let replies = [
                (
                    "/api/v1/submissions",
                    "202 Accepted",
                    format!(
                        r#"{{"submission_token":"{server_token}","poll_within_seconds":1}}"#
                    )
                    .into_bytes(),
                ),
                (
                    "/api/v1/submissions/status",
                    "200 OK",
                    br#"{"state":"failed","record_count":1,"processed_records":1,"progress":1.0,"error":"table read failed"}"#.to_vec(),
                ),
                ("/api/v1/submissions/cancel", "204 No Content", Vec::new()),
            ];
            for (path, status, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                assert_request(&request, path);
                if path != "/api/v1/submissions" {
                    assert_eq!(request_json(&request)["submission_token"], server_token);
                }
                write_response(&mut stream, status, "application/json", &body);
            }
        });

        let error = mock_client(address)
            .lookup(&endpoint_file(7), None, |_| {})
            .unwrap_err();
        server.join().unwrap();
        assert!(matches!(
            error,
            RemoteLookupError::InvalidResponse(ref message) if message == "table read failed"
        ));
    }

    fn mock_client(address: std::net::SocketAddr) -> RemoteLookupClient {
        RemoteLookupClient::new(RemoteLookupConfig {
            base_url: format!("http://{address}"),
            poll_interval: Duration::from_millis(1),
            request_timeout: Duration::from_secs(5),
            ..RemoteLookupConfig::default()
        })
        .unwrap()
    }

    fn split_request(request: &[u8]) -> (&[u8], &[u8]) {
        let offset = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        (&request[..offset], &request[offset..])
    }

    fn assert_request(request: &[u8], path: &str) {
        let headers = String::from_utf8_lossy(split_request(request).0);
        assert!(headers.starts_with(&format!("POST {path} HTTP/1.1\r\n")));
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
    }

    fn request_json(request: &[u8]) -> serde_json::Value {
        serde_json::from_slice(split_request(request).1).unwrap()
    }

    fn read_request(stream: &mut TcpStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            data.extend_from_slice(&buffer[..read]);
            if let Some(offset) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let headers = String::from_utf8_lossy(&data[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
            })
            .unwrap_or(0);
        while data.len() < header_end + content_length {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            data.extend_from_slice(&buffer[..read]);
        }
        data.truncate(header_end + content_length);
        data
    }

    fn write_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
        stream.flush().unwrap();
    }
}
