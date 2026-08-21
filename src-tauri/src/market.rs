//! cordis.run plugin-market client.
//!
//! The bootstrap webview cannot fetch external hosts under the app CSP, so
//! every catalog request is made here. The wire DTO deliberately remains more
//! permissive than the installation DTO: incomplete, blocked, deprecated, or
//! non-npm entries can be displayed, but only a fully validated nested source
//! can cross the installation boundary.

use reqwest::header::{CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::Manager;
use url::Url;

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
    // Transitional display-only compatibility. Never use this variant (or
    // any old flat npm/version field) as an installation source.
    Text(String),
    Localized {
        #[serde(default)]
        zh: Option<String>,
        #[serde(default)]
        en: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MarketEntry {
    slug: String,
    name: String,
    #[serde(rename = "entryRevision", default)]
    entry_revision: Option<String>,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    description: Option<Description>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default)]
    engines: Option<BTreeMap<String, String>>,
    #[serde(default)]
    stars: Option<u32>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    blocked: Option<bool>,
    #[serde(default)]
    deprecated: Option<bool>,
    // These two fields are Desktop-derived. They are deliberately skipped
    // while deserializing so a server cannot advertise an item as installable.
    #[serde(default, skip_deserializing)]
    installable: bool,
    #[serde(rename = "installReason", default, skip_deserializing)]
    install_reason: Option<String>,
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
    engines: Option<BTreeMap<String, String>>,
    #[serde(default)]
    blocked: Option<bool>,
    #[serde(default)]
    deprecated: Option<bool>,
    #[serde(rename = "publishedAt", default)]
    published_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MarketDetail {
    #[serde(flatten)]
    entry: MarketEntry,
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
    #[serde(rename = "schemaVersion", default)]
    schema_version: Option<u32>,
    #[serde(rename = "catalogRevision", default)]
    catalog_revision: Option<String>,
    #[serde(default)]
    items: Vec<MarketEntry>,
    #[serde(default)]
    count: u32,
    #[serde(default)]
    page: MarketPage,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    cache: Option<MarketCacheMetadata>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MarketCacheMetadata {
    status: &'static str,
    fetched_at_ms: u64,
}

#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    error: ApiError,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiError {
    code: String,
    message: String,
    request_id: Option<String>,
}

/// Fully validated, immutable nested source used by the Desktop installation
/// flow. It intentionally has no legacy flat-field fallback.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketInstallCandidate {
    pub slug: String,
    pub entry_revision: String,
    pub package_name: String,
    pub version: String,
    pub integrity: String,
    pub registry: String,
    pub tarball: String,
}

const DEFAULT_BASE_URL: &str = "https://cordis.run/api/v1";
const SEARCH_TTL: Duration = Duration::from_secs(60);
const DETAIL_TTL: Duration = Duration::from_secs(300);
const STALE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const IMAGE_MAX_BYTES: usize = 2 * 1024 * 1024;
const JSON_MAX_BYTES: usize = 1024 * 1024;
const CACHE_MAX_ENTRIES: usize = 64;
const HOME_CACHE_SCHEMA: u32 = 1;
const HOME_CACHE_LIMIT: u32 = 30;
const HOME_CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const APPROVED_NPM_REGISTRY_HOSTS: &[&str] = &["registry.npmjs.org"];

pub fn is_valid_market_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    !slug.is_empty()
        && slug.len() <= 128
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn base_url_allowed(url: &Url) -> bool {
    // CORDIS_RUN_API is a developer-only fixture override, not a general
    // outbound proxy setting. Keep the production origin and API prefix
    // exact so an inherited environment variable cannot silently widen the
    // network trust boundary (for example with credentials, a custom port,
    // or a same-host non-API route).
    let structurally_canonical = url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(url.path(), "/api/v1" | "/api/v1/");
    let https_cordis = structurally_canonical
        && url.scheme() == "https"
        && url.host_str() == Some("cordis.run")
        && url.port().is_none();
    let debug_loopback = cfg!(debug_assertions)
        && structurally_canonical
        && url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
    https_cordis || debug_loopback
}

fn image_url_allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("cdn.cordis.run")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn canonical_image_content_type(header: &str) -> Option<&'static str> {
    let media_type = header.split(';').next()?.trim();
    if media_type.eq_ignore_ascii_case("image/png") {
        Some("image/png")
    } else if media_type.eq_ignore_ascii_case("image/jpeg") {
        Some("image/jpeg")
    } else if media_type.eq_ignore_ascii_case("image/webp") {
        Some("image/webp")
    } else if media_type.eq_ignore_ascii_case("image/gif") {
        Some("image/gif")
    } else {
        None
    }
}

fn image_magic_matches(content_type: &str, bytes: &[u8]) -> bool {
    match content_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => false,
    }
}

#[derive(Debug, Clone)]
struct Cached {
    at: Instant,
    fetched_at_ms: u64,
    value: Value,
    etag: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheDisposition {
    Fresh,
    Fetched,
    Revalidated,
    Offline,
}

struct CachedJson {
    value: Value,
    etag: Option<String>,
    disposition: CacheDisposition,
    fetched_at_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiskHomeCache {
    schema_version: u32,
    origin: String,
    fetched_at_ms: u64,
    etag: Option<String>,
    catalog_revision: Option<String>,
    response: Value,
}

struct HomeDiskState {
    path: PathBuf,
    cached: Option<DiskHomeCache>,
}

/// Market client state. Managed once and shared across Tauri commands.
pub struct MarketClient {
    base_url: String,
    http: reqwest::Client,
    search_cache: Mutex<HashMap<String, Cached>>,
    detail_cache: Mutex<HashMap<String, Cached>>,
    home_disk: Mutex<Option<HomeDiskState>>,
}

impl MarketClient {
    pub fn new(app: &tauri::AppHandle) -> Result<Self, String> {
        let base_url =
            std::env::var("CORDIS_RUN_API").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let canonical = base_url.trim_end_matches('/') == DEFAULT_BASE_URL;
        let cache_path = if !cfg!(debug_assertions) && canonical {
            Some(
                app.path()
                    .app_data_dir()
                    .map_err(|e| format!("cannot resolve market cache directory: {e}"))?
                    .join("desktop-state/market/home-v1.json"),
            )
        } else {
            None
        };
        Self::with_configuration(base_url, cache_path)
    }

    #[cfg(test)]
    fn with_base_url(base_url: String) -> Result<Self, String> {
        Self::with_configuration(base_url, None)
    }

    fn with_configuration(base_url: String, cache_path: Option<PathBuf>) -> Result<Self, String> {
        let parsed =
            Url::parse(&base_url).map_err(|e| format!("invalid CORDIS_RUN_API URL: {e}"))?;
        if !base_url_allowed(&parsed) {
            return Err(
                "CORDIS_RUN_API must be https://cordis.run/api/v1 (debug builds may use http://127.0.0.1:<port>/api/v1)"
                    .to_string(),
            );
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("dsh-desktop/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("market client init failed: {e}"))?;
        let normalized_origin = base_url.trim_end_matches('/').to_string();
        let home_disk = cache_path.map(|path| HomeDiskState {
            cached: load_disk_home_cache(&path, &normalized_origin),
            path,
        });
        Ok(MarketClient {
            base_url,
            http,
            search_cache: Mutex::new(HashMap::new()),
            detail_cache: Mutex::new(HashMap::new()),
            home_disk: Mutex::new(home_disk),
        })
    }

    #[cfg(test)]
    fn with_home_cache(base_url: String, path: PathBuf) -> Result<Self, String> {
        Self::with_configuration(base_url, Some(path))
    }

    fn cache_fresh(
        cache: &Mutex<HashMap<String, Cached>>,
        key: &str,
        ttl: Duration,
    ) -> Option<Cached> {
        let cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.get(key).and_then(|entry| {
            if entry.at.elapsed() < ttl {
                Some(entry.clone())
            } else {
                None
            }
        })
    }

    fn cache_any(cache: &Mutex<HashMap<String, Cached>>, key: &str) -> Option<Cached> {
        let cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache
            .get(key)
            .filter(|entry| entry.at.elapsed() < STALE_MAX_AGE)
            .cloned()
    }

    fn cache_put(
        cache: &Mutex<HashMap<String, Cached>>,
        key: &str,
        value: Value,
        etag: Option<String>,
    ) {
        let mut cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.retain(|_, entry| entry.at.elapsed() < STALE_MAX_AGE);
        if cache.len() >= CACHE_MAX_ENTRIES && !cache.contains_key(key) {
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
                fetched_at_ms: unix_time_ms(),
                value,
                etag,
            },
        );
    }

    fn cache_revalidated(cache: &Mutex<HashMap<String, Cached>>, key: &str) -> Option<Cached> {
        let mut cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = cache.get_mut(key)?;
        if entry.at.elapsed() >= STALE_MAX_AGE {
            return None;
        }
        entry.at = Instant::now();
        entry.fetched_at_ms = unix_time_ms();
        Some(entry.clone())
    }

    /// Fetch JSON with a conditional request once a cached representation is
    /// stale. 304 refreshes the cache timestamp without attempting to parse a
    /// body. HTTP API errors deliberately never fall back to stale data:
    /// otherwise a deleted detail could be mistaken for a live catalog entry.
    async fn cached_json(
        &self,
        cache: &Mutex<HashMap<String, Cached>>,
        key: &str,
        ttl: Duration,
        url: Url,
        label: &str,
        force_revalidate: bool,
    ) -> Result<CachedJson, String> {
        if !force_revalidate {
            if let Some(fresh) = Self::cache_fresh(cache, key, ttl) {
                return Ok(CachedJson {
                    value: fresh.value,
                    etag: fresh.etag,
                    disposition: CacheDisposition::Fresh,
                    fetched_at_ms: fresh.fetched_at_ms,
                });
            }
        }

        let previous = Self::cache_any(cache, key);
        let mut request = self.http.get(url);
        if let Some(etag) = previous.as_ref().and_then(|entry| entry.etag.as_deref()) {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                // Catalog browsing may remain available while offline, but an
                // install/activation decision must be based on a live (or
                // conditionally revalidated 304) detail response. Returning
                // a stale candidate here would let a revoked revision cross
                // the confirmation boundary after a network failure.
                if !force_revalidate && is_transport_failure(&error) {
                    if let Some(stale) = previous {
                        return Ok(CachedJson {
                            value: stale.value,
                            etag: stale.etag,
                            disposition: CacheDisposition::Offline,
                            fetched_at_ms: stale.fetched_at_ms,
                        });
                    }
                }
                return Err(format!("{label} request failed: {error}"));
            }
        };
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            let cached = Self::cache_revalidated(cache, key)
                .ok_or_else(|| format!("{label} returned HTTP 304 without a cached response"))?;
            return Ok(CachedJson {
                value: cached.value,
                etag: cached.etag,
                disposition: CacheDisposition::Revalidated,
                fetched_at_ms: cached.fetched_at_ms,
            });
        }

        let status = response.status();
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = read_limited_json_body(response).await?;
        if !status.is_success() {
            return Err(format_api_error(label, status, &body));
        }
        let json = serde_json::from_slice::<Value>(&body)
            .map_err(|e| format!("{label} response was not JSON: {e}"))?;
        Self::cache_put(cache, key, json.clone(), etag.clone());
        Ok(CachedJson {
            value: json,
            etag,
            disposition: CacheDisposition::Fetched,
            fetched_at_ms: unix_time_ms(),
        })
    }

    fn seed_home_from_disk(&self, key: &str) {
        let envelope = self
            .home_disk
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .and_then(|state| state.cached.clone());
        let Some(envelope) = envelope else {
            return;
        };
        let mut cache = self
            .search_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.contains_key(key) {
            return;
        }
        let stale_at = Instant::now()
            .checked_sub(SEARCH_TTL)
            .unwrap_or_else(Instant::now);
        cache.insert(
            key.to_string(),
            Cached {
                at: stale_at,
                fetched_at_ms: envelope.fetched_at_ms,
                value: envelope.response,
                etag: envelope.etag,
            },
        );
    }

    fn persist_home(&self, raw: &Value, etag: Option<String>, catalog_revision: Option<String>) {
        let mut disk = self
            .home_disk
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(state) = disk.as_mut() else {
            return;
        };
        let envelope = DiskHomeCache {
            schema_version: HOME_CACHE_SCHEMA,
            origin: self.base_url.trim_end_matches('/').to_string(),
            fetched_at_ms: unix_time_ms(),
            etag,
            catalog_revision,
            response: raw.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&envelope) else {
            return;
        };
        if crate::secure_fs::atomic_write(&state.path, &bytes, JSON_MAX_BYTES).is_ok() {
            state.cached = Some(envelope);
        }
    }

    pub async fn search(
        &self,
        query: &str,
        category: Option<&str>,
        limit: Option<u32>,
        cursor: Option<&str>,
        dsh_version: &str,
    ) -> Result<Value, String> {
        // The Desktop catalog is intentionally never an all-platform endpoint.
        // The backend command hardcodes this too; retaining it here prevents a
        // future caller from accidentally widening the request.
        let platform = "desktop";
        let limit = limit.unwrap_or(50).clamp(1, 100);
        let cache_key = format!(
            "{:?}",
            (
                query,
                category.unwrap_or(""),
                limit,
                cursor.unwrap_or(""),
                platform,
                dsh_version
            )
        );
        let is_home = query.is_empty()
            && category.is_none_or(str::is_empty)
            && cursor.is_none_or(str::is_empty)
            && limit == HOME_CACHE_LIMIT;
        if is_home {
            self.seed_home_from_disk(&cache_key);
        }
        let url = format!("{}/plugins", self.base_url.trim_end_matches('/'));
        let mut url = Url::parse(&url).map_err(|e| format!("bad market base URL: {e}"))?;
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("platform", platform)
            .append_pair("limit", &limit.to_string());
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            // Cursor values are opaque: only hand the exact server value back.
            url.query_pairs_mut().append_pair("cursor", cursor);
        }
        if let Some(category) = category.filter(|category| !category.is_empty()) {
            url.query_pairs_mut().append_pair("category", category);
        }

        let outcome = self
            .cached_json(
                &self.search_cache,
                &cache_key,
                SEARCH_TTL,
                url,
                "market search",
                false,
            )
            .await?;
        let raw = outcome.value;
        let mut parsed: MarketSearchResponse = serde_json::from_value(raw.clone())
            .map_err(|e| format!("market search response did not match the v4 DTO: {e}"))?;
        if is_home
            && matches!(
                outcome.disposition,
                CacheDisposition::Fetched | CacheDisposition::Revalidated
            )
        {
            self.persist_home(&raw, outcome.etag.clone(), parsed.catalog_revision.clone());
        }
        for item in &mut parsed.items {
            annotate_installability(item, dsh_version);
        }
        // Defensive only: the server already filters by platform. Keep count
        // and page untouched because they are the server's pagination truth.
        parsed
            .items
            .retain(|item| item.platforms.iter().any(|value| value == platform));
        if outcome.disposition == CacheDisposition::Offline {
            for item in &mut parsed.items {
                item.installable = false;
                item.install_reason = Some(
                    "offline cached catalog entries are display-only; reconnect to install"
                        .to_string(),
                );
            }
            parsed.page.cursor = None;
            parsed.page.has_more = false;
            parsed.cache = Some(MarketCacheMetadata {
                status: "offline",
                fetched_at_ms: outcome.fetched_at_ms,
            });
        }
        serde_json::to_value(parsed)
            .map_err(|e| format!("market search response serialization failed: {e}"))
    }

    async fn fetch_detail(
        &self,
        slug: &str,
        force_revalidate: bool,
    ) -> Result<MarketDetail, String> {
        if !is_valid_market_slug(slug) {
            return Err("invalid market slug".to_string());
        }
        let url = format!("{}/plugins/{slug}", self.base_url.trim_end_matches('/'));
        let url = Url::parse(&url).map_err(|e| format!("bad market base URL: {e}"))?;
        let raw = self
            .cached_json(
                &self.detail_cache,
                slug,
                DETAIL_TTL,
                url,
                "market detail",
                force_revalidate,
            )
            .await?
            .value;
        serde_json::from_value(raw)
            .map_err(|e| format!("market detail response did not match the v4 DTO: {e}"))
    }

    pub async fn detail(&self, slug: &str, dsh_version: &str) -> Result<Value, String> {
        let mut detail = self.fetch_detail(slug, false).await?;
        annotate_installability(&mut detail.entry, dsh_version);
        serde_json::to_value(detail)
            .map_err(|e| format!("market detail response serialization failed: {e}"))
    }

    /// Revalidate the detail even if its normal cache TTL has not elapsed.
    /// A 304 still proves the previously cached entry is the current revision.
    pub async fn prepare_install(
        &self,
        slug: &str,
        dsh_version: &str,
    ) -> Result<MarketInstallCandidate, String> {
        let detail = self.fetch_detail(slug, true).await?;
        candidate_from_entry(&detail.entry, dsh_version)
    }

    pub async fn image(&self, url: &str) -> Result<Value, String> {
        let parsed = Url::parse(url).map_err(|e| format!("invalid image URL: {e}"))?;
        if !image_url_allowed(&parsed) {
            return Err("market images must use https://cdn.cordis.run".to_string());
        }
        let response = self
            .http
            .get(parsed)
            .send()
            .await
            .map_err(|e| format!("image request failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("image request failed: HTTP {}", response.status()));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(canonical_image_content_type)
            .ok_or_else(|| {
                "market image must be PNG, JPEG, WebP, or GIF (SVG is not accepted)".to_string()
            })?;
        let bytes = read_limited_image_body(response).await?;
        if !image_magic_matches(content_type, &bytes) {
            return Err("market image bytes do not match the declared raster type".to_string());
        }
        let data_url = format!("data:{content_type};base64,{}", base64_encode(&bytes));
        Ok(serde_json::json!({ "dataUrl": data_url }))
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn is_transport_failure(error: &reqwest::Error) -> bool {
    error.is_connect()
        || error.is_timeout()
        || (error.is_request() && !error.is_builder() && !error.is_redirect())
}

fn load_disk_home_cache(path: &std::path::Path, expected_origin: &str) -> Option<DiskHomeCache> {
    let bytes = crate::secure_fs::read_bounded(path, JSON_MAX_BYTES as u64)
        .ok()
        .flatten()?;
    let envelope: DiskHomeCache = serde_json::from_slice(&bytes).ok()?;
    let now = unix_time_ms();
    let max_age_ms = HOME_CACHE_MAX_AGE.as_millis().min(u128::from(u64::MAX)) as u64;
    if envelope.schema_version != HOME_CACHE_SCHEMA
        || envelope.origin != expected_origin
        || envelope.fetched_at_ms > now
        || now.saturating_sub(envelope.fetched_at_ms) > max_age_ms
    {
        return None;
    }
    let parsed: MarketSearchResponse = serde_json::from_value(envelope.response.clone()).ok()?;
    if parsed.page.limit != HOME_CACHE_LIMIT
        || parsed.catalog_revision != envelope.catalog_revision
        || parsed.cache.is_some()
    {
        return None;
    }
    Some(envelope)
}

fn format_api_error(label: &str, status: reqwest::StatusCode, body: &[u8]) -> String {
    match serde_json::from_slice::<ApiErrorBody>(body) {
        Ok(parsed) => {
            let request = parsed
                .error
                .request_id
                .filter(|id| !id.is_empty())
                .map(|id| format!(" (requestId: {id})"))
                .unwrap_or_default();
            format!(
                "{label} failed: {} {}: {}{request}",
                status, parsed.error.code, parsed.error.message
            )
        }
        Err(_) => format!("{label} failed: HTTP {status}"),
    }
}

fn required_wire_string<'a>(value: &'a Option<String>, field: &str) -> Result<&'a str, String> {
    let value = value
        .as_deref()
        .ok_or_else(|| format!("market entry is missing {field}"))?;
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(format!("market entry has invalid {field}"));
    }
    Ok(value)
}

fn secure_https_url(raw: &str, field: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|e| format!("market entry has invalid {field}: {e}"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(format!("market entry has unsafe {field} URL"));
    }
    Ok(url)
}

fn is_valid_sha512_integrity(value: &str) -> bool {
    let Some(encoded) = value.strip_prefix("sha512-") else {
        return false;
    };
    if encoded.len() != 88 || !encoded.is_ascii() {
        return false;
    }
    let bytes = encoded.as_bytes();
    let padding = if bytes.ends_with(b"==") {
        2
    } else if bytes.ends_with(b"=") {
        1
    } else {
        0
    };
    if padding > 2 || bytes[..bytes.len() - padding].contains(&b'=') {
        return false;
    }
    if !bytes[..bytes.len() - padding]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
    {
        return false;
    }
    // SHA-512 output is 64 bytes, which always has two trailing "=" in
    // canonical standard base64. Reject unusual encodings rather than trying
    // to normalize them before they reach the lockfile verifier.
    padding == 2 && (encoded.len() / 4) * 3 - padding == 64
}

fn parse_dsh_requirement(raw: &str) -> Result<VersionReq, String> {
    VersionReq::parse(raw)
        .or_else(|_| {
            let normalized = raw.split_whitespace().collect::<Vec<_>>().join(", ");
            VersionReq::parse(&normalized)
        })
        .map_err(|e| format!("market entry has invalid engines.dsh: {e}"))
}

/// Store distributions have an additional, locally pinned review boundary.
/// Keep it in the candidate gate so the catalog UI, the confirmation preview,
/// and the mutation command all agree that an unreviewed package is not
/// installable. The command repeats this check as defense in depth because
/// IPC callers must never rely on UI-derived state.
fn distribution_allows_package(package_name: &str, store_build: bool) -> bool {
    !store_build || crate::curated_plugins::is_allowed(package_name)
}

fn candidate_from_entry(
    entry: &MarketEntry,
    dsh_version: &str,
) -> Result<MarketInstallCandidate, String> {
    if !is_valid_market_slug(&entry.slug) {
        return Err("market entry has invalid slug".to_string());
    }
    if entry.blocked != Some(false) {
        return Err("this market entry is blocked and cannot be installed".to_string());
    }
    if entry.deprecated != Some(false) {
        return Err("this market entry is deprecated and cannot be installed".to_string());
    }
    if !entry.platforms.iter().any(|platform| platform == "desktop") {
        return Err("this market entry does not support desktop".to_string());
    }

    let entry_revision = required_wire_string(&entry.entry_revision, "entryRevision")?.to_owned();
    let source = entry
        .source
        .as_ref()
        .ok_or_else(|| "market entry is missing nested source".to_string())?;
    if source.source_type.as_deref() != Some("npm") {
        return Err("this market entry is not an npm source".to_string());
    }
    let package_name = required_wire_string(&source.package_name, "source.packageName")?;
    if !crate::plugins::is_valid_package_name(package_name) {
        return Err("market entry has invalid source.packageName".to_string());
    }
    if !distribution_allows_package(package_name, crate::build_info::STORE_BUILD) {
        return Err(
            "this market entry is not on the Microsoft Store reviewed plugin list".to_string(),
        );
    }
    let version = required_wire_string(&source.version, "source.version")?;
    Version::parse(version).map_err(|e| format!("market entry has invalid source.version: {e}"))?;
    let integrity = required_wire_string(&source.integrity, "source.integrity")?;
    if !is_valid_sha512_integrity(integrity) {
        return Err("market entry has invalid source.integrity".to_string());
    }
    let registry_raw = required_wire_string(&source.registry, "source.registry")?;
    let registry = secure_https_url(registry_raw, "source.registry")?;
    let registry_host = registry
        .host_str()
        .ok_or_else(|| "market entry has invalid source.registry host".to_string())?;
    if registry.path() != "/" || registry.query().is_some() {
        return Err("market entry has unsafe source.registry URL".to_string());
    }
    if !APPROVED_NPM_REGISTRY_HOSTS.contains(&registry_host) {
        return Err("market entry uses an unapproved npm registry host".to_string());
    }
    let tarball = required_wire_string(&source.tarball, "source.tarball")?;
    let tarball_url = secure_https_url(tarball, "source.tarball")?;
    if tarball_url.host_str() != Some(registry_host) {
        return Err("market entry tarball host does not match its registry".to_string());
    }
    if tarball_url.query().is_some() {
        return Err("market entry has unsafe source.tarball URL".to_string());
    }

    let dsh_range = entry
        .engines
        .as_ref()
        .and_then(|engines| engines.get("dsh"))
        .ok_or_else(|| "market entry is missing engines.dsh".to_string())?;
    let requirement = parse_dsh_requirement(dsh_range)?;
    let current = Version::parse(dsh_version)
        .map_err(|e| format!("Desktop DSH version is unavailable or invalid: {e}"))?;
    if !requirement.matches(&current) {
        return Err(format!(
            "this market entry requires DSH {dsh_range}, but Desktop runs {dsh_version}"
        ));
    }

    Ok(MarketInstallCandidate {
        slug: entry.slug.clone(),
        entry_revision,
        package_name: package_name.to_owned(),
        version: version.to_owned(),
        integrity: integrity.to_owned(),
        registry: registry_raw.to_owned(),
        tarball: tarball.to_owned(),
    })
}

fn annotate_installability(entry: &mut MarketEntry, dsh_version: &str) {
    match candidate_from_entry(entry, dsh_version) {
        Ok(_) => {
            entry.installable = true;
            entry.install_reason = None;
        }
        Err(error) => {
            entry.installable = false;
            entry.install_reason = Some(error);
        }
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
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    const TEST_SERVER_IO_TIMEOUT: Duration = Duration::from_secs(5);

    const VALID_INTEGRITY: &str =
        "sha512-CQpnWPrDwmP1+SMHXZhtLtJv90yiyVfluGsX5iNCVkrhQtU3TQHsUWPG9wkdk9Lgd5yNpAg9jQEo90CBaXgWMA==";

    fn valid_entry() -> MarketEntry {
        serde_json::from_value(serde_json::json!({
            "slug": "fixture-plugin",
            "name": "Fixture Plugin",
            "entryRevision": "revision-1",
            "description": {"zh": "中文说明", "en": "English description"},
            "source": {
                "type": "npm",
                "packageName": "dsh-cc-tui",
                "version": "1.0.0",
                "integrity": VALID_INTEGRITY,
                "registry": "https://registry.npmjs.org",
                "tarball": "https://registry.npmjs.org/dsh-cc-tui/-/dsh-cc-tui-1.0.0.tgz"
            },
            "platforms": ["desktop"],
            "engines": {"dsh": ">=0.1.0-rc.6 <0.2.0"},
            "blocked": false,
            "deprecated": false
        }))
        .expect("valid fixture entry")
    }

    fn home_response_body() -> String {
        serde_json::json!({
            "schemaVersion": 4,
            "catalogRevision": "catalog-1",
            "items": [serde_json::to_value(valid_entry()).expect("serialize entry")],
            "count": 1,
            "page": {"cursor": "opaque-next", "hasMore": true, "limit": HOME_CACHE_LIMIT}
        })
        .to_string()
    }

    fn cache_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "dshd-market-cache-{name}-{}",
                crate::secure_fs::random_suffix().expect("cache test id")
            ))
            .join("home-v1.json")
    }

    fn response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
        let mut out = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("\r\n");
        out.push_str(body);
        out
    }

    fn accept_test_connection(listener: &TcpListener) -> TcpStream {
        let deadline = Instant::now() + TEST_SERVER_IO_TIMEOUT;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    // Accepted sockets inherit the listener's nonblocking
                    // mode on Windows. Restore blocking I/O so the bounded
                    // read/write timeouts below behave consistently.
                    stream
                        .set_nonblocking(false)
                        .expect("set fixture stream blocking");
                    stream
                        .set_read_timeout(Some(TEST_SERVER_IO_TIMEOUT))
                        .expect("set fixture read timeout");
                    stream
                        .set_write_timeout(Some(TEST_SERVER_IO_TIMEOUT))
                        .expect("set fixture write timeout");
                    return stream;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "fixture server timed out waiting for a request"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("fixture server accept failed: {error}"),
            }
        }
    }

    fn test_server(responses: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        listener
            .set_nonblocking(true)
            .expect("set fixture listener nonblocking");
        let port = listener.local_addr().expect("fixture address").port();
        let worker = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let mut stream = accept_test_connection(&listener);
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    let count = stream.read(&mut chunk).expect("read request");
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8(bytes).expect("request is UTF-8"));
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            requests
        });
        (format!("http://127.0.0.1:{port}/api/v1"), worker)
    }

    fn test_server_disconnects_after_first_response(
        first_response: String,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        listener
            .set_nonblocking(true)
            .expect("set fixture listener nonblocking");
        let port = listener.local_addr().expect("fixture address").port();
        let worker = std::thread::spawn(move || {
            let mut first = accept_test_connection(&listener);
            let mut bytes = [0_u8; 1024];
            let _ = first.read(&mut bytes).expect("read first request");
            first
                .write_all(first_response.as_bytes())
                .expect("write first response");
            drop(first);

            // Accept the conditional revalidation request and close the
            // socket without a response. This models an interrupted network
            // path, not an HTTP API error (which already must not be stale).
            let mut second = accept_test_connection(&listener);
            let _ = second.read(&mut bytes).expect("read second request");
        });
        (format!("http://127.0.0.1:{port}/api/v1"), worker)
    }

    #[test]
    fn parses_nested_source_and_bilingual_description() {
        let entry = valid_entry();
        assert_eq!(
            entry
                .source
                .as_ref()
                .and_then(|source| source.package_name.as_deref()),
            Some("dsh-cc-tui")
        );
        match entry.description.as_ref() {
            Some(Description::Localized { zh, en }) => {
                assert_eq!(zh.as_deref(), Some("中文说明"));
                assert_eq!(en.as_deref(), Some("English description"));
            }
            _ => panic!("localized description should deserialize"),
        }
    }

    #[test]
    fn candidate_accepts_only_complete_nested_npm_source() {
        let entry = valid_entry();
        let candidate =
            candidate_from_entry(&entry, "0.1.0-rc.7").expect("fixture should be installable");
        assert_eq!(candidate.entry_revision, "revision-1");
        assert_eq!(candidate.package_name, "dsh-cc-tui");

        let mut blocked = valid_entry();
        blocked.blocked = Some(true);
        assert!(candidate_from_entry(&blocked, "0.1.0-rc.7").is_err());

        let mut deprecated = valid_entry();
        deprecated.deprecated = Some(true);
        assert!(candidate_from_entry(&deprecated, "0.1.0-rc.7").is_err());

        let legacy: MarketEntry = serde_json::from_value(serde_json::json!({
            "slug": "legacy",
            "name": "Legacy",
            "npm": "legacy-package",
            "version": "1.0.0",
            "platforms": ["desktop"],
            "blocked": false,
            "deprecated": false
        }))
        .expect("legacy display shape can deserialize");
        assert!(candidate_from_entry(&legacy, "0.1.0-rc.7").is_err());
    }

    #[test]
    fn blocked_entry_is_displayable_but_never_produces_install_candidate() {
        let mut blocked = valid_entry();
        blocked.blocked = Some(true);
        annotate_installability(&mut blocked, "0.1.0-rc.7");
        assert!(!blocked.installable);
        assert!(blocked
            .install_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("blocked")));
        assert!(candidate_from_entry(&blocked, "0.1.0-rc.7").is_err());
    }

    #[test]
    fn candidate_rejects_bad_source_host_or_engine() {
        let mut entry = valid_entry();
        entry.source.as_mut().expect("source").tarball =
            Some("https://evil.example/fixture-plugin.tgz".to_string());
        assert!(candidate_from_entry(&entry, "0.1.0-rc.7").is_err());

        let mut incompatible = valid_entry();
        incompatible.engines = Some(BTreeMap::from([("dsh".to_string(), ">=0.2.0".to_string())]));
        assert!(candidate_from_entry(&incompatible, "0.1.0-rc.7").is_err());
        assert!(is_valid_sha512_integrity(VALID_INTEGRITY));
        assert!(!is_valid_sha512_integrity("sha512-AAAA"));
    }

    #[test]
    fn store_distribution_only_allows_the_reviewed_snapshot() {
        assert!(distribution_allows_package("dsh-cc-tui", true));
        assert!(!distribution_allows_package("fixture-plugin", true));
        assert!(distribution_allows_package("fixture-plugin", false));
    }

    #[test]
    fn search_uses_desktop_cursor_category_and_etag_revalidation() {
        let body = serde_json::json!({
            "items": [serde_json::to_value(valid_entry()).expect("serialize entry")],
            "count": 1,
            "page": {"cursor": "fixture:next", "hasMore": true, "limit": 1}
        })
        .to_string();
        let (base, worker) = test_server(vec![
            response(
                "200 OK",
                &[
                    ("Content-Type", "application/json"),
                    ("ETag", "\"fixture-v1\""),
                ],
                &body,
            ),
            response("304 Not Modified", &[("ETag", "\"fixture-v1\"")], ""),
        ]);
        let client = MarketClient::with_base_url(base).expect("client");
        let first = tauri::async_runtime::block_on(client.search(
            "",
            Some("agent"),
            Some(1),
            Some("fixture:0"),
            "0.1.0-rc.7",
        ))
        .expect("first search");
        assert_eq!(first["count"], 1);
        assert_eq!(first["page"]["cursor"], "fixture:next");
        assert_eq!(first["items"][0]["installable"], true);
        {
            let mut cache = client.search_cache.lock().expect("cache");
            for entry in cache.values_mut() {
                // Keep a margin beyond the boundary so hosted-runner clock
                // granularity cannot leave this test on the exact TTL edge.
                // The second request must always exercise revalidation.
                entry.at = Instant::now() - (SEARCH_TTL + Duration::from_secs(1));
            }
        }
        let second = tauri::async_runtime::block_on(client.search(
            "",
            Some("agent"),
            Some(1),
            Some("fixture:0"),
            "0.1.0-rc.7",
        ))
        .expect("304 should reuse cache");
        assert_eq!(second["items"][0]["slug"], "fixture-plugin");
        let requests = worker.join().expect("fixture worker");
        let first_request = requests[0].to_ascii_lowercase();
        assert!(first_request.contains("platform=desktop"));
        assert!(first_request.contains("category=agent"));
        assert!(first_request.contains("cursor=fixture%3a0"));
        assert!(requests[1]
            .to_ascii_lowercase()
            .contains("if-none-match: \"fixture-v1\""));
    }

    #[test]
    fn disk_home_cache_revalidates_with_etag_across_restart() {
        let body = home_response_body();
        let (base, worker) = test_server(vec![
            response(
                "200 OK",
                &[
                    ("Content-Type", "application/json"),
                    ("ETag", "\"home-v1\""),
                ],
                &body,
            ),
            response("304 Not Modified", &[("ETag", "\"home-v1\"")], ""),
        ]);
        let path = cache_path("revalidate");
        let first =
            MarketClient::with_home_cache(base.clone(), path.clone()).expect("first client");
        let first_result = tauri::async_runtime::block_on(first.search(
            "",
            None,
            Some(HOME_CACHE_LIMIT),
            None,
            "0.1.0-rc.7",
        ));
        assert!(first_result.is_ok());
        assert!(path.is_file());
        drop(first);

        let second = MarketClient::with_home_cache(base, path.clone()).expect("second client");
        let second_result = tauri::async_runtime::block_on(second.search(
            "",
            None,
            Some(HOME_CACHE_LIMIT),
            None,
            "0.1.0-rc.7",
        ))
        .expect("304 should reuse persistent cache");
        assert_eq!(second_result["items"][0]["slug"], "fixture-plugin");
        assert!(second_result.get("cache").is_none());
        let requests = worker.join().expect("fixture worker");
        assert!(requests[1]
            .to_ascii_lowercase()
            .contains("if-none-match: \"home-v1\""));
        std::fs::remove_dir_all(path.parent().expect("cache parent")).expect("remove cache");
    }

    #[test]
    fn disk_home_transport_failure_is_display_only_and_has_no_pagination() {
        let body = home_response_body();
        let (base, worker) = test_server_disconnects_after_first_response(response(
            "200 OK",
            &[
                ("Content-Type", "application/json"),
                ("ETag", "\"home-v1\""),
            ],
            &body,
        ));
        let path = cache_path("offline");
        let first =
            MarketClient::with_home_cache(base.clone(), path.clone()).expect("first client");
        tauri::async_runtime::block_on(first.search(
            "",
            None,
            Some(HOME_CACHE_LIMIT),
            None,
            "0.1.0-rc.7",
        ))
        .expect("populate disk cache");
        drop(first);

        let second = MarketClient::with_home_cache(base, path.clone()).expect("second client");
        let offline = tauri::async_runtime::block_on(second.search(
            "",
            None,
            Some(HOME_CACHE_LIMIT),
            None,
            "0.1.0-rc.7",
        ))
        .expect("transport failure should use disk cache");
        assert_eq!(offline["cache"]["status"], "offline");
        assert_eq!(offline["items"][0]["installable"], false);
        assert!(offline["items"][0]["installReason"]
            .as_str()
            .is_some_and(|reason| reason.contains("display-only")));
        assert_eq!(offline["page"]["hasMore"], false);
        assert!(offline["page"]["cursor"].is_null());
        worker.join().expect("fixture worker");
        std::fs::remove_dir_all(path.parent().expect("cache parent")).expect("remove cache");
    }

    #[test]
    fn corrupt_expired_or_wrong_origin_disk_cache_is_ignored() {
        let path = cache_path("invalid");
        let parent = path.parent().expect("cache parent");
        crate::secure_fs::ensure_private_dir(parent).expect("cache dir");
        std::fs::write(&path, b"not-json").expect("corrupt cache");
        assert!(load_disk_home_cache(&path, "https://cordis.run/api/v1").is_none());

        let response: Value = serde_json::from_str(&home_response_body()).expect("home response");
        let expired = DiskHomeCache {
            schema_version: HOME_CACHE_SCHEMA,
            origin: "https://cordis.run/api/v1".to_string(),
            fetched_at_ms: unix_time_ms()
                .saturating_sub(HOME_CACHE_MAX_AGE.as_millis() as u64)
                .saturating_sub(1),
            etag: Some("\"old\"".to_string()),
            catalog_revision: Some("catalog-1".to_string()),
            response: response.clone(),
        };
        std::fs::write(&path, serde_json::to_vec(&expired).expect("expired JSON"))
            .expect("expired cache");
        assert!(load_disk_home_cache(&path, "https://cordis.run/api/v1").is_none());

        let wrong_origin = DiskHomeCache {
            fetched_at_ms: unix_time_ms(),
            origin: "https://evil.example/api/v1".to_string(),
            ..expired
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&wrong_origin).expect("wrong-origin JSON"),
        )
        .expect("wrong-origin cache");
        assert!(load_disk_home_cache(&path, "https://cordis.run/api/v1").is_none());
        std::fs::remove_dir_all(parent).expect("remove cache");
    }

    #[test]
    fn detail_404_is_reported_as_json_api_error() {
        let (base, worker) = test_server(vec![response(
            "404 Not Found",
            &[("Content-Type", "application/json")],
            r#"{"error":{"code":"NOT_FOUND","message":"no such slug","requestId":"req-1"}}"#,
        )]);
        let client = MarketClient::with_base_url(base).expect("client");
        let error = tauri::async_runtime::block_on(client.detail("missing", "0.1.0-rc.7"))
            .expect_err("detail should fail");
        assert!(error.contains("NOT_FOUND: no such slug"));
        assert!(error.contains("requestId: req-1"));
        let _ = worker.join().expect("fixture worker");
    }

    #[test]
    fn install_prepare_does_not_fall_back_to_stale_detail_after_network_failure() {
        let body = serde_json::json!({
            "slug": "fixture-plugin",
            "name": "Fixture Plugin",
            "entryRevision": "revision-1",
            "source": {
                "type": "npm",
                "packageName": "fixture-plugin",
                "version": "1.0.0",
                "integrity": VALID_INTEGRITY,
                "registry": "https://registry.npmjs.org",
                "tarball": "https://registry.npmjs.org/fixture-plugin/-/fixture-plugin-1.0.0.tgz"
            },
            "platforms": ["desktop"],
            "engines": {"dsh": ">=0.1.0-rc.6 <0.2.0"},
            "blocked": false,
            "deprecated": false
        })
        .to_string();
        let (base, worker) = test_server_disconnects_after_first_response(response(
            "200 OK",
            &[
                ("Content-Type", "application/json"),
                ("ETag", "\"fixture-v1\""),
            ],
            &body,
        ));
        let client = MarketClient::with_base_url(base).expect("client");
        let detail = tauri::async_runtime::block_on(client.detail("fixture-plugin", "0.1.0-rc.7"));
        assert!(detail.is_ok(), "first detail request should populate cache");
        let error =
            tauri::async_runtime::block_on(client.prepare_install("fixture-plugin", "0.1.0-rc.7"))
                .expect_err("install preparation must require live revalidation");
        assert!(error.contains("market detail request failed"));
        worker.join().expect("fixture worker");
    }

    #[test]
    fn image_urls_are_cdn_only() {
        assert!(image_url_allowed(
            &Url::parse("https://cdn.cordis.run/screenshots/x/1.webp").expect("url")
        ));
        assert!(!image_url_allowed(
            &Url::parse("https://cordis.run/screenshots/x/1.webp").expect("url")
        ));
        assert!(!image_url_allowed(
            &Url::parse("http://127.0.0.1/fixture.png").expect("url")
        ));
    }

    #[test]
    fn market_image_mime_types_are_an_exact_raster_allowlist() {
        for (header, expected) in [
            ("image/png", "image/png"),
            ("IMAGE/JPEG", "image/jpeg"),
            ("image/webp; charset=binary", "image/webp"),
            (" image/gif ", "image/gif"),
        ] {
            assert_eq!(canonical_image_content_type(header), Some(expected));
        }
        for rejected in [
            "image/svg+xml",
            "image/avif",
            "image/x-icon",
            "image/pngevil",
            "text/html",
            "",
        ] {
            assert_eq!(canonical_image_content_type(rejected), None, "{rejected}");
        }

        assert!(image_magic_matches("image/png", b"\x89PNG\r\n\x1a\nrest"));
        assert!(image_magic_matches("image/jpeg", b"\xff\xd8\xff\xe0rest"));
        assert!(image_magic_matches("image/gif", b"GIF89arest"));
        assert!(image_magic_matches(
            "image/webp",
            b"RIFF\x04\x00\x00\x00WEBPrest"
        ));
        assert!(!image_magic_matches(
            "image/png",
            b"<svg xmlns='http://www.w3.org/2000/svg'>"
        ));
    }

    #[test]
    fn base_url_is_limited_to_the_canonical_api_boundary() {
        assert!(base_url_allowed(
            &Url::parse("https://cordis.run/api/v1").expect("production API URL")
        ));
        assert!(base_url_allowed(
            &Url::parse("https://cordis.run/api/v1/").expect("production API URL")
        ));
        assert!(base_url_allowed(
            &Url::parse("http://127.0.0.1:3210/api/v1").expect("fixture API URL")
        ));
        assert!(base_url_allowed(
            &Url::parse("http://localhost:3210/api/v1/").expect("fixture API URL")
        ));

        for raw in [
            "https://cordis.run/",
            "https://cordis.run/api/v1?fixture=1",
            "https://cordis.run/api/v1#fragment",
            "https://user:pass@cordis.run/api/v1",
            "https://cordis.run:8443/api/v1",
            "https://evil.example/api/v1",
            "http://cordis.run/api/v1",
            "http://127.0.0.1:3210/not-api",
        ] {
            assert!(
                !base_url_allowed(&Url::parse(raw).expect("test URL")),
                "should reject {raw}"
            );
        }
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
