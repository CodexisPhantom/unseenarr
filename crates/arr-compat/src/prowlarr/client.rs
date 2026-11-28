//! Read-only Prowlarr v1 HTTP client for importing indexers
//! (`docs/08-integrations.md`, "Prowlarr as import source", and
//! `docs/14-compatibility.md`, section C).
//!
//! Every request sends HTTP `GET` with `X-Api-Key` through [`OutboundClient`],
//! enforcing the outbound URL and DNS check. The client exposes no write
//! methods.

use std::fmt;

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::de::DeserializeOwned;
use unseenarr_core::http::{OutboundClient, UrlError, check_url};
use url::Url;

use super::dto::IndexerResource;

const AGENT: &str = concat!("Unseenarr/", env!("CARGO_PKG_VERSION"));
const API_KEY_HEADER: &str = "X-Api-Key";

/// Why a read-only Prowlarr v1 API call failed.
#[derive(Debug, PartialEq, Eq)]
pub enum ProwlarrError {
    /// The URL failed the outbound check.
    Url(UrlError),
    /// Prowlarr refused the API key (HTTP 401 or 403).
    Unauthorized,
    /// The endpoint was not found (HTTP 404).
    NotFound(String),
    /// Prowlarr answered with an unexpected HTTP status.
    Status { status: u16, detail: String },
    /// Prowlarr answered with a body that did not match the expected JSON shape.
    InvalidResponse(String),
    /// Prowlarr could not be reached or the transfer failed.
    Unavailable(String),
}

impl fmt::Display for ProwlarrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(error) => write!(f, "{error}"),
            Self::Unauthorized => f.write_str("Prowlarr refused the API key"),
            Self::NotFound(endpoint) => {
                write!(f, "Prowlarr endpoint `{endpoint}` was not found (HTTP 404)")
            }
            Self::Status { status, detail } if detail.is_empty() => {
                write!(f, "Prowlarr answered HTTP {status}")
            }
            Self::Status { status, detail } => {
                write!(f, "Prowlarr answered HTTP {status}: {detail}")
            }
            Self::InvalidResponse(reason) => write!(f, "unexpected Prowlarr answer: {reason}"),
            Self::Unavailable(reason) => write!(f, "Prowlarr is not available: {reason}"),
        }
    }
}

impl std::error::Error for ProwlarrError {}

impl From<UrlError> for ProwlarrError {
    fn from(error: UrlError) -> Self {
        Self::Url(error)
    }
}

/// Normalises a user-entered Prowlarr URL to `<origin>[<urlBase>]`, dropping a
/// trailing `/api/v1` suffix, after running the outbound URL check.
fn base(base_url: &str) -> Result<Url, UrlError> {
    let mut url = Url::parse(base_url.trim()).map_err(|_| UrlError::Invalid)?;
    check_url(&url)?;
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_owned();
    let lower = path.to_ascii_lowercase();
    let prefix = match lower.strip_suffix("/api/v1") {
        Some(stripped) => path[..stripped.len()].to_owned(),
        None => path,
    };
    url.set_path(if prefix.is_empty() { "/" } else { &prefix });
    Ok(url)
}

/// Normalises a user-entered Prowlarr URL to `<origin>[<urlBase>]/api/v1/<endpoint>`
/// after running the outbound URL check.
fn endpoint_url(base_url: &str, endpoint: &str) -> Result<String, UrlError> {
    let mut url = base(base_url)?;
    let path = url.path().trim_end_matches('/').to_owned();
    let endpoint = endpoint.trim_start_matches('/');
    if path == "/" || path.is_empty() {
        url.set_path(&format!("/api/v1/{endpoint}"));
    } else {
        url.set_path(&format!("{path}/api/v1/{endpoint}"));
    }
    Ok(url.into())
}

/// The per-indexer Torznab/Newznab feed of one Prowlarr indexer:
/// `<origin>[<urlBase>]/<indexerId>/api`.
pub fn feed_url(base_url: &str, indexer_id: i32) -> Result<String, UrlError> {
    let mut url = base(base_url)?;
    let path = url.path().trim_end_matches('/').to_owned();
    if path == "/" || path.is_empty() {
        url.set_path(&format!("/{indexer_id}/api"));
    } else {
        url.set_path(&format!("{path}/{indexer_id}/api"));
    }
    Ok(url.into())
}

/// The first words of an error response body, collapsed to a single line.
fn snippet(body: &str) -> String {
    body.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}

fn unavailable(error: reqwest::Error) -> ProwlarrError {
    ProwlarrError::Unavailable(error.without_url().to_string())
}

/// Read-only client for a running Prowlarr v1 instance.
///
/// Accepts a base URL with or without a URL base, trailing slash, or `/api/v1`
/// suffix (for example `http://prowlarr:9696`, `http://prowlarr:9696/prowlarr/`,
/// or `http://prowlarr:9696/prowlarr/api/v1`).
#[derive(Clone)]
pub struct ProwlarrClient {
    http: OutboundClient,
    base_url: String,
    api_key: String,
}

// No derive: the API key must not reach a log line.
impl fmt::Debug for ProwlarrClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProwlarrClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl ProwlarrClient {
    /// Creates a read-only client targeting `base_url` with `api_key`.
    pub fn new(http: OutboundClient, base_url: &str, api_key: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim().to_owned(),
            api_key: api_key.to_owned(),
        }
    }

    async fn get<T: DeserializeOwned>(&self, endpoint: &str) -> Result<T, ProwlarrError> {
        let url = endpoint_url(&self.base_url, endpoint)?;
        let response = self
            .http
            .get(&url)?
            .header(API_KEY_HEADER, &self.api_key)
            .header(USER_AGENT, AGENT)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(unavailable)?;

        match response.status() {
            status if status.is_success() => {
                let body = response.bytes().await.map_err(unavailable)?;
                serde_json::from_slice(&body)
                    .map_err(|error| ProwlarrError::InvalidResponse(format!("{endpoint}: {error}")))
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(ProwlarrError::Unauthorized),
            StatusCode::NOT_FOUND => Err(ProwlarrError::NotFound(endpoint.to_owned())),
            status => {
                let body = response.text().await.unwrap_or_default();
                Err(ProwlarrError::Status {
                    status: status.as_u16(),
                    detail: snippet(&body),
                })
            }
        }
    }

    /// `GET /api/v1/indexer`
    pub async fn indexers(&self) -> Result<Vec<IndexerResource>, ProwlarrError> {
        self.get("indexer").await
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::prowlarr::dto::IndexerResource;

    const INDEXERS: &str = include_str!("../../../../fixtures/prowlarr/indexer.json");

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn json_ok(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "application/json")
    }

    fn client(base_url: &str) -> ProwlarrClient {
        ProwlarrClient::new(OutboundClient::new().unwrap(), base_url, KEY)
    }

    #[tokio::test]
    async fn indexers_parse_the_v1_answer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/indexer"))
            .and(header("X-Api-Key", KEY))
            .and(header("User-Agent", AGENT))
            .and(header("Accept", "application/json"))
            .respond_with(json_ok(INDEXERS))
            .expect(1)
            .mount(&server)
            .await;

        let indexers: Vec<IndexerResource> = client(&server.uri()).indexers().await.unwrap();
        assert_eq!(indexers.len(), 3);
        assert_eq!(indexers[0].name, "Example Tracker");
        assert_eq!(indexers[0].protocol, "torrent");
        assert_eq!(indexers[0].categories(), vec![2000, 5000]);
        let api_key = indexers[0].field("apiKey").unwrap();
        assert_eq!(
            api_key.value.as_ref().and_then(serde_json::Value::as_str),
            Some("********")
        );
    }

    #[tokio::test]
    async fn a_url_base_and_api_suffix_are_normalised() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/prowlarr/api/v1/indexer"))
            .and(header("X-Api-Key", KEY))
            .respond_with(json_ok(INDEXERS))
            .expect(1)
            .mount(&server)
            .await;

        let base = format!("{}/prowlarr/api/v1", server.uri());
        let indexers = client(&base).indexers().await.unwrap();
        assert_eq!(indexers.len(), 3);
    }

    #[tokio::test]
    async fn a_refused_key_is_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/indexer"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        assert_eq!(
            client(&server.uri()).indexers().await.unwrap_err(),
            ProwlarrError::Unauthorized
        );
    }

    #[tokio::test]
    async fn a_broken_answer_is_an_invalid_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/indexer"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        assert!(matches!(
            client(&server.uri()).indexers().await.unwrap_err(),
            ProwlarrError::InvalidResponse(_)
        ));
    }

    #[test]
    fn feed_url_points_at_the_indexer_endpoint() {
        assert_eq!(
            feed_url("http://prowlarr:9696", 3).unwrap(),
            "http://prowlarr:9696/3/api"
        );
        assert_eq!(
            feed_url("http://prowlarr:9696/prowlarr/", 3).unwrap(),
            "http://prowlarr:9696/prowlarr/3/api"
        );
        assert_eq!(
            feed_url("http://prowlarr:9696/prowlarr/api/v1", 3).unwrap(),
            "http://prowlarr:9696/prowlarr/3/api"
        );
    }
}
