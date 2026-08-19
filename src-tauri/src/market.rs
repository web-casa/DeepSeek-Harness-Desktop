//! cordis.run plugin-market client. The bootstrap webview cannot fetch
//! external hosts under the app CSP, so every market request is made here.
//! Results are cached in memory; a failed refresh returns the stale cache
//! entry (when one exists) instead of dropping an otherwise usable catalog.

use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Source {
    #[serde(rename = "type", default)]
    source_type: Option<String>,
    #[serde(rename = "packageName", default)]
    package_name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    integrity: Option<String>,
    #[serde(default)]
    registry: Option<String>,
    #[serde(default)]
    tarball: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum Description {
    Text(String),
    Localized {
        #[serde(default)]
        zh: Option<String>,
        #[serde(default)]
        en: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MarketItem {
    slug: String,
    name: String,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    description: Option<Description>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default)]
    stars: Option<u32>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    blocked: Option<bool>,
    #[serde(default)]
    deprecated: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MarketVersion {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default)]
    engines: Option<serde_json::Value>,
    #[serde(default)]
    blocked: Option<bool>,
    #[serde(default)]
    deprecated: Option<bool>,
    #[serde(rename = "publishedAt", default)]
    published_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MarketDetail {
    slug: String,
    name: String,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    description: Option<Description>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default)]
    stars: Option<u32>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    blocked: Option<bool>,
    #[serde(default)]
    deprecated: Option<bool>,
    #[serde(default)]
    screenshots: Vec<String>,
    #[serde(default)]
    versions: Vec<MarketVersion>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct MarketPage {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(rename = "hasMore", default)]
    has_more: bool,
    #[serde(default)]
    limit: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MarketSearchResponse {
    #[serde(default)]
    items: Vec<MarketItem>,
    #[serde(default)]
    count: u32,
    #[serde(default)]
    page: MarketPage,
}

const DEFAULT_BASE_URL: &str = "https://cordis.run/api/v1";
const SEARCH_TTL: Duration = Duration::from_secs(60);
const DETAIL_TTL: Duration = Duration::from_secs(300);
const IMAGE_MAX_BYTES: usize = 2 * 1024 * 1024;
const JSON_MAX_BYTES: usize = 1024 * 1024;
const CACHE_MAX_ENTRIES: usize = 64;

pub fn is_valid_market_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    !slug.is_empty()
        && slug.len() <= 128
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn base_url_allowed(url: &reqwest::Url) -> bool {
    let https_cordis = url.scheme() == "https" && url.host_str() == Some("cordis.run");
    let debug_loopback = cfg!(debug_assertions)
        && url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
    https_cordis || debug_loopback
}

#[derive(Debug, Clone)]
struct Cached {
    at: Instant,
    value: Value,
}

/// Market client state. Managed once and shared across Tauri commands.
pub struct MarketClient {
    base_url: String,
    http: reqwest::Client,
    search_cache: Mutex<HashMap<String, Cached>>,
    detail_cache: Mutex<HashMap<String, Cached>>,
}

impl MarketClient {
    pub fn new() -> Result<Self, String> {
        let base_url =
            std::env::var("CORDIS_RUN_API").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let base_url = {
            let parsed = reqwest::Url::parse(&base_url)
                .map_err(|e| format!("invalid CORDIS_RUN_API URL: {e}"))?;
            if !base_url_allowed(&parsed) {
                return Err("CORDIS_RUN_API must be https://cordis.run (debug builds may use http://127.0.0.1)".to_string());
            }
            base_url
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("dsh-desktop/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("market client init failed: {e}"))?;
        Ok(MarketClient {
            base_url,
            http,
            search_cache: Mutex::new(HashMap::new()),
            detail_cache: Mutex::new(HashMap::new()),
        })
    }

    fn cache_get(
        cache: &Mutex<HashMap<String, Cached>>,
        key: &str,
        ttl: Duration,
    ) -> Option<Value> {
        let cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.get(key).and_then(|entry| {
            if entry.at.elapsed() < ttl {
                Some(entry.value.clone())
            } else {
                None
            }
        })
    }

    fn cache_put(cache: &Mutex<HashMap<String, Cached>>, key: &str, value: Value) {
        let mut cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.retain(|_, entry| entry.at.elapsed() < Duration::from_secs(24 * 60 * 60));
        if cache.len() >= CACHE_MAX_ENTRIES {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            key.to_string(),
            Cached {
                at: Instant::now(),
                value,
            },
        );
    }

    pub async fn search(
        &self,
        query: &str,
        category: Option<&str>,
        limit: Option<u32>,
        cursor: Option<&str>,
        platform: &str,
    ) -> Result<Value, String> {
        let limit = limit.unwrap_or(30);
        let cache_key = format!(
            "{:?}",
            (
                query,
                category.unwrap_or(""),
                limit,
                cursor.unwrap_or(""),
                platform
            )
        );
        if let Some(fresh) = Self::cache_get(&self.search_cache, &cache_key, SEARCH_TTL) {
            return Ok(fresh);
        }

        let url = format!("{}/plugins", self.base_url.trim_end_matches('/'));
        let mut url = reqwest::Url::parse(&url).map_err(|e| format!("bad market base URL: {e}"))?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("platform", platform)
            .append_pair("limit", &limit.to_string());
        if let Some(cursor) = cursor {
            if !cursor.is_empty() {
                url.query_pairs_mut().append_pair("cursor", cursor);
            }
        }
        if let Some(category) = category {
            url.query_pairs_mut().append_pair("category", category);
        }

        let fetch = async {
            let response = self
                .http
                .get(url)
                .send()
                .await
                .map_err(|e| format!("market search request failed: {e}"))?;
            if !response.status().is_success() {
                return Err(format!("market search failed: HTTP {}", response.status()));
            }
            let body = read_limited_json_body(response).await?;
            let mut parsed: MarketSearchResponse = serde_json::from_slice(&body)
                .map_err(|e| format!("market search response was not JSON: {e}"))?;
            // Defensive only: the server already filters by `platform`.
            // Keep `count` and `page` exactly as sent — they are the server's
            // cursor-pagination truth. If the server ignored the filter, the
            // worst case is an item/count mismatch, not a wrong-platform
            // install (the UI still checks platforms before installing).
            parsed
                .items
                .retain(|item| item.platforms.iter().any(|p| p == platform));
            let json = serde_json::to_value(parsed)
                .map_err(|e| format!("market search response serialization failed: {e}"))?;
            MarketClient::cache_put(&self.search_cache, &cache_key, json.clone());
            Ok(json)
        };

        match fetch.await {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Some(stale) = Self::cache_get(
                    &self.search_cache,
                    &cache_key,
                    Duration::from_secs(24 * 60 * 60),
                ) {
                    Ok(stale)
                } else {
                    Err(error)
                }
            }
        }
    }

    pub async fn detail(&self, slug: &str) -> Result<Value, String> {
        if !is_valid_market_slug(slug) {
            return Err("invalid market slug".to_string());
        }
        let cache_key = slug.to_string();
        if let Some(fresh) = Self::cache_get(&self.detail_cache, &cache_key, DETAIL_TTL) {
            return Ok(fresh);
        }

        let url = format!("{}/plugins/{slug}", self.base_url.trim_end_matches('/'));

        let fetch = async {
            let response = self
                .http
                .get(url)
                .send()
                .await
                .map_err(|e| format!("market detail request failed: {e}"))?;
            if !response.status().is_success() {
                return Err(format!("market detail failed: HTTP {}", response.status()));
            }
            let body = read_limited_json_body(response).await?;
            let detail: MarketDetail = serde_json::from_slice(&body)
                .map_err(|e| format!("market detail response was not JSON: {e}"))?;
            let json = serde_json::to_value(detail)
                .map_err(|e| format!("market detail response serialization failed: {e}"))?;
            MarketClient::cache_put(&self.detail_cache, &cache_key, json.clone());
            Ok(json)
        };

        match fetch.await {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Some(stale) = Self::cache_get(
                    &self.detail_cache,
                    &cache_key,
                    Duration::from_secs(24 * 60 * 60),
                ) {
                    Ok(stale)
                } else {
                    Err(error)
                }
            }
        }
    }

    pub async fn image(&self, url: &str) -> Result<Value, String> {
        let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid image URL: {e}"))?;
        if !base_url_allowed(&parsed) {
            return Err("market images must use an allowed market host".to_string());
        }
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("image request failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("image request failed: HTTP {}", response.status()));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        if !content_type.starts_with("image/") {
            return Err("market image response is not an image".to_string());
        }
        let bytes = read_limited_image_body(response).await?;
        let data_url = format!("data:{content_type};base64,{}", base64_encode(&bytes));
        Ok(serde_json::json!({ "dataUrl": data_url }))
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

async fn read_limited_json_body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    use futures_util::StreamExt;
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("market read failed: {e}"))?;
        if body.len().saturating_add(chunk.len()) > JSON_MAX_BYTES {
            return Err("market response exceeds 1 MiB".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn read_limited_image_body(response: reqwest::Response) -> Result<Vec<u8>, String> {
    use futures_util::StreamExt;
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("image read failed: {e}"))?;
        if body.len().saturating_add(chunk.len()) > IMAGE_MAX_BYTES {
            return Err("image exceeds 2 MiB".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn base_url_defaults_to_cordis_run() {
        // Constructor reads env; the default path is covered by base_url builder.
        let base = std::env::var("CORDIS_RUN_API").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        assert_eq!(base, DEFAULT_BASE_URL);
    }

    #[test]
    fn parses_cursor_page_and_defensively_filters_items() {
        let body = r#"{
            "items": [
                {
                    "slug":"a",
                    "name":"A",
                    "source":{"type":"npm","packageName":"a-pkg","version":"1.0.0"},
                    "description":{"zh":"甲","en":"A"},
                    "platforms":["web","desktop"]
                },
                {
                    "slug":"b",
                    "name":"B",
                    "source":{"type":"npm","packageName":"b-pkg","version":"2.0.0"},
                    "platforms":["web"]
                },
                {
                    "slug":"c",
                    "name":"C",
                    "source":{"type":"npm","packageName":"c-pkg","version":"3.0.0"},
                    "platforms":["desktop"]
                }
            ],
            "count": 3,
            "page": {"cursor":"opaque","hasMore":true,"limit":50}
        }"#;
        let mut parsed: MarketSearchResponse =
            serde_json::from_str(body).expect("fixture should parse");
        assert_eq!(parsed.page.limit, 50);
        assert!(parsed.page.has_more);
        assert_eq!(parsed.page.cursor.as_deref(), Some("opaque"));
        parsed
            .items
            .retain(|item| item.platforms.iter().any(|p| p == "desktop"));
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(parsed.items[0].slug, "a");
        assert_eq!(
            parsed.items[0]
                .source
                .as_ref()
                .and_then(|s| s.package_name.as_deref()),
            Some("a-pkg")
        );
        assert_eq!(parsed.items[1].slug, "c");
        // Server pagination fields are preserved untouched.
        assert_eq!(parsed.count, 3);
        assert_eq!(parsed.page.limit, 50);
        assert!(parsed.page.has_more);
        assert_eq!(parsed.page.cursor.as_deref(), Some("opaque"));
        match parsed.items[0].description.as_ref() {
            Some(Description::Localized { zh, en }) => {
                assert_eq!(zh.as_deref(), Some("甲"));
                assert_eq!(en.as_deref(), Some("A"));
            }
            _ => panic!("description should parse as localized text"),
        }
    }

    #[test]
    fn detail_defaults_screenshots_to_empty_and_parses_source() {
        let detail: MarketDetail = serde_json::from_str(
            r#"{"slug":"x","name":"X","platforms":["desktop"],"source":{"type":"npm","packageName":"x-pkg","version":"1.0.0"}}"#,
        )
        .expect("detail fixture should parse");
        assert!(detail.screenshots.is_empty());
        assert_eq!(detail.platforms, vec!["desktop".to_string()]);
        assert_eq!(
            detail
                .source
                .as_ref()
                .and_then(|s| s.package_name.as_deref()),
            Some("x-pkg")
        );
    }

    #[test]
    fn loopback_allowed_only_in_debug_builds() {
        let release_cordis =
            base_url_allowed(&reqwest::Url::parse("https://cordis.run/api/v1").expect("url"));
        assert!(release_cordis);
        if cfg!(debug_assertions) {
            let loopback = base_url_allowed(
                &reqwest::Url::parse("http://127.0.0.1:8787/api/v1").expect("url"),
            );
            assert!(loopback);
        }
        assert!(!base_url_allowed(
            &reqwest::Url::parse("https://evil.example/api/v1").expect("url")
        ));
    }

    #[test]
    fn slug_validator_rejects_path_and_query_chars() {
        assert!(is_valid_market_slug("is-odd"));
        assert!(is_valid_market_slug("code"));
        assert!(!is_valid_market_slug("../other"));
        assert!(!is_valid_market_slug("a?b"));
        assert!(!is_valid_market_slug("a#b"));
        assert!(!is_valid_market_slug("A"));
    }

    #[test]
    fn base64_encodes_and_pads() {
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }
}
