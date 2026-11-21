//! Settings handling, ported from `searx/settings_defaults.py`,
//! `searx/settings_loader.py` and `searx/settings.yml`.
//!
//! Differences to upstream are deliberate and documented:
//!
//! * the built-in defaults ship as a TOML file (`settings.toml`); YAML engines
//!   such as the generic `xpath` engine are not ported,
//! * `use_default_settings` can be `true` (merge user engines into the
//!   defaults, the upstream default) or `false` (replace the engine list),
//! * unknown engine specific keys are kept in `EngineConfig::extra` so that a
//!   config written for SearXNG keeps loading.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::locales;

/// Default seconds the server drains in-flight requests after a stop signal.
fn default_shutdown_grace() -> u64 {
    10
}

/// Largest page number the HTTP API accepts.
///
/// `search.max_page = 0` means "no configured limit", but every engine
/// multiplies the page number into an offset, so the entry point clamps to
/// this value. Kept here so diagnostics and the web layer share one number.
pub const MAX_PAGENO: u32 = 1000;

/// The built-in defaults, embedded at compile time.
pub const DEFAULT_SETTINGS: &str = include_str!("../settings.toml");

/// Formats the `/search` endpoint can render (upstream `OUTPUT_FORMATS`).
pub static OUTPUT_FORMATS: &[&str] = &["html", "csv", "json", "rss"];

/// Accept a list of categories or one comma separated string.
///
/// Upstream's YAML accepts a scalar here, and a list that was accidentally
/// written as a string must stay one category ("general"), not fall apart into
/// its characters. ``["general"]`` and ``"general"`` therefore mean the same.
fn string_or_seq<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrSeq {
        String(String),
        Seq(Vec<String>),
    }

    Ok(match StringOrSeq::deserialize(deserializer)? {
        StringOrSeq::String(text) => text
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect(),
        StringOrSeq::Seq(items) => items,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not read settings file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid settings file: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid settings: {0}")]
    Validation(String),
}

/// Top level settings object.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    /// When true the engines of the defaults and of the user file are merged.
    pub use_default_settings: Option<bool>,
    pub general: General,
    pub server: Server,
    pub search: Search,
    pub ui: Ui,
    pub outgoing: Outgoing,
    pub engines: Vec<EngineConfig>,
    pub plugins: Plugins,
    /// Response cache backend (upstream `cache` and `valkey`).
    #[serde(default)]
    pub cache: CacheSettings,
    /// Deep research: plan queries, search, fetch, synthesise a cited report.
    #[serde(default)]
    pub research: ResearchSettings,
    /// Page fetching for MCP `read_page` and HTTP `include_raw_content`.
    #[serde(default)]
    pub fetch: FetchSettings,
    /// Model Context Protocol exposure.
    pub mcp: McpSettings,
    /// Categories shown as tabs (upstream `categories_as_tabs`).
    #[serde(default, deserialize_with = "string_or_seq")]
    pub categories_as_tabs: Vec<String>,
    /// Accepted for compatibility with SearXNG configs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doi_resolvers: Option<toml::Table>,
}

/// Deep research configuration. `enabled` requires an OpenAI-compatible
/// `base_url` and `api_key`; without them the /research endpoint still returns
/// a thorough evidence bundle (no synthesised report).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ResearchSettings {
    pub enabled: bool,
    /// OpenAI-compatible base URL, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Sub-queries generated per question.
    pub max_queries: usize,
    /// Sources kept after deduplication.
    pub max_sources: usize,
    /// LLM request timeout in seconds.
    pub timeout: u64,
    /// Whole-request deadline of `/research`, in seconds.
    ///
    /// Sub-queries and page fetches run one after another, so the LLM timeout
    /// alone does not bound the endpoint.
    pub deadline: u64,
    /// How many `/research` requests may run at the same time.
    pub max_concurrency: usize,
}

impl Default for ResearchSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            model: "gpt-4o-mini".into(),
            max_queries: 5,
            max_sources: 10,
            timeout: 60,
            deadline: 120,
            max_concurrency: 2,
        }
    }
}

/// Page fetching backend. `direct` keeps the built-in fetcher that validates
/// every destination locally. `deepseek` sends the URL to the DeepSeek Fetch
/// API, which returns Markdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FetchProvider {
    #[default]
    Direct,
    DeepSeek,
}

/// Page fetching configuration for MCP `read_page` and HTTP raw content.
/// Model Context Protocol exposure (`[mcp]`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct McpSettings {
    /// Serve MCP over Streamable HTTP at `/mcp` on the HTTP listener.
    ///
    /// Off by default: the endpoint hands a remote agent the same fetch
    /// capability as `read_page`, so a public instance opts in knowingly.
    pub http: bool,
    /// Host authorities accepted for the MCP endpoint. Empty keeps the
    /// transport default (loopback only), which is what a local instance
    /// wants; a reverse proxy has to name its public host here.
    pub allowed_hosts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FetchSettings {
    pub provider: FetchProvider,
    /// Request timeout for one page fetch, in seconds.
    pub timeout: f64,
    pub deepseek: DeepSeekFetchSettings,
}

impl Default for FetchSettings {
    fn default() -> Self {
        Self {
            provider: FetchProvider::Direct,
            timeout: 20.0,
            deepseek: DeepSeekFetchSettings::default(),
        }
    }
}

/// Credentials of the DeepSeek Fetch API.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DeepSeekFetchSettings {
    pub base_url: String,
    pub api_key: String,
}

impl Default for DeepSeekFetchSettings {
    fn default() -> Self {
        Self {
            base_url: "https://api.deepseek.com".into(),
            api_key: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct General {
    pub debug: bool,
    pub instance_name: String,
    pub contact_url: String,
    pub donation_url: String,
    pub privacypolicy_url: String,
    pub enable_metrics: bool,
    pub public_instance: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            debug: false,
            instance_name: "RustXNG".into(),
            contact_url: String::new(),
            donation_url: String::new(),
            privacypolicy_url: String::new(),
            enable_metrics: true,
            public_instance: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Server {
    pub bind_address: String,
    pub port: u16,
    /// Exit after this many seconds without a request (0 keeps running).
    /// Handy for sidecars that a launcher forgets to stop.
    #[serde(default)]
    pub idle_timeout: u64,
    /// Seconds the server keeps draining after SIGTERM/SIGINT before it stops.
    ///
    /// Containers and service managers kill a process that outlives their own
    /// grace period, so the drain has to end on its own.
    #[serde(default = "default_shutdown_grace")]
    pub shutdown_grace: u64,
    pub base_url: Option<String>,
    pub secret_key: String,
    /// `server.limiter`: the legacy boolean or a `[server.limiter]` table.
    pub limiter: LimiterSettings,
    /// Length of the legacy limiter window in seconds.
    ///
    /// Only used when `server.limiter = true` (the flat form); the table form
    /// uses the upstream `ip_limit` windows.
    pub limiter_window: u64,
    /// Requests allowed per client and window in the legacy flat form.
    pub limiter_max: u32,
    /// Upstream's `server.public_instance`: enables the stricter limiter.
    pub public_instance: bool,
    pub image_proxy: bool,
    /// HTTP method of the search form: `GET` or `POST`.
    pub method: String,
    pub http_protocol_version: Option<String>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1".into(),
            port: 29483,
            idle_timeout: 0,
            shutdown_grace: default_shutdown_grace(),
            base_url: None,
            secret_key: String::new(),
            limiter: LimiterSettings::default(),
            limiter_window: 60,
            limiter_max: 30,
            public_instance: false,
            image_proxy: false,
            method: "POST".into(),
            http_protocol_version: None,
        }
    }
}

impl Server {
    /// Whether the limiter is switched on, in either the flat or table form.
    pub fn limiter_enabled(&self) -> bool {
        self.limiter.is_enabled()
    }

    /// The `[server.limiter]` table, if the table form is used.
    /// Seconds spent draining in-flight requests on shutdown (default 10).
    pub fn shutdown_grace(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.shutdown_grace)
    }

    pub fn limiter_config(&self) -> Option<&LimiterConfig> {
        self.limiter.config()
    }
}

/// `server.limiter`: a boolean (upstream's `server.limiter`) or a table:
///
/// ```text
/// [server.limiter]
/// enabled = true
/// trusted_proxies = ["127.0.0.0/8"]
///
/// [server.limiter.botdetection]
/// ipv4_prefix = 32
/// ipv6_prefix = 48
///
/// [server.limiter.botdetection.ip_limit]
/// filter_link_local = false
/// link_token = true
///
/// [server.limiter.botdetection.ip_lists]
/// pass_ip = ["192.168.0.0/16"]
/// block_ip = []
/// ```
///
/// The `[botdetection]` keys mirror `limiter.toml` upstream.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum LimiterSettings {
    /// `server.limiter = true`: enabled with the legacy flat window/limit.
    Enabled(bool),
    /// `[server.limiter]` with the botdetection configuration.
    Config(Box<LimiterConfig>),
}

impl Default for LimiterSettings {
    fn default() -> Self {
        Self::Enabled(false)
    }
}

impl LimiterSettings {
    pub fn is_enabled(&self) -> bool {
        match self {
            Self::Enabled(enabled) => *enabled,
            Self::Config(config) => config.enabled,
        }
    }

    pub fn config(&self) -> Option<&LimiterConfig> {
        match self {
            Self::Enabled(_) => None,
            Self::Config(config) => Some(config),
        }
    }
}

/// The `[server.limiter]` table.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LimiterConfig {
    /// Enabling the table form enables the limiter unless set to `false`.
    pub enabled: bool,
    /// Trusted proxies; merged with `botdetection.trusted_proxies`.
    pub trusted_proxies: Vec<String>,
    pub botdetection: BotDetection,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            trusted_proxies: Vec::new(),
            botdetection: BotDetection::default(),
        }
    }
}

/// `[server.limiter.botdetection]`, mirroring upstream's `limiter.toml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct BotDetection {
    /// Number of leading bits that define an IPv4 client network.
    pub ipv4_prefix: u8,
    /// Number of leading bits that define an IPv6 client network.
    pub ipv6_prefix: u8,
    /// Addresses of reverse proxies whose forwarding headers are trusted.
    pub trusted_proxies: Vec<String>,
    pub ip_limit: IpLimitConfig,
    pub ip_lists: IpListsConfig,
}

impl Default for BotDetection {
    fn default() -> Self {
        Self {
            ipv4_prefix: 32,
            ipv6_prefix: 48,
            trusted_proxies: vec!["127.0.0.0/8".into(), "::1".into()],
            ip_limit: IpLimitConfig::default(),
            ip_lists: IpListsConfig::default(),
        }
    }
}

/// `[server.limiter.botdetection.ip_limit]`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct IpLimitConfig {
    /// Also rate limit link-local client networks.
    pub filter_link_local: bool,
    /// Challenge clients without a valid limiter token earlier.
    pub link_token: bool,
}

/// `[server.limiter.botdetection.ip_lists]`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct IpListsConfig {
    pub block_ip: Vec<String>,
    pub pass_ip: Vec<String>,
    /// Pass the hardcoded check.searx.space addresses.
    pub pass_searxng_org: bool,
}

impl Default for IpListsConfig {
    fn default() -> Self {
        Self {
            block_ip: Vec::new(),
            pass_ip: Vec::new(),
            pass_searxng_org: true,
        }
    }
}

fn all_locale_tags() -> Vec<String> {
    locales::LOCALE_TAGS
        .iter()
        .map(|tag| (*tag).to_string())
        .collect()
}

/// `[search.circuit]`: the statistical circuit breaker in front of the engines.
///
/// Hard blocks (access denied, CAPTCHA, 429) suspend an engine outright
/// through `search.suspended_times`. The circuit is the softer layer: it looks
/// at the last `window` outcomes and opens when too many of them failed, so an
/// engine that times out or returns unparsable pages half of the time leaves
/// the rotation for a while instead of costing every search its deadline.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct CircuitSettings {
    /// Enable the breaker. Disabled leaves every engine in the rotation.
    pub enabled: bool,
    /// Number of recent outcomes the failure rate is measured over.
    pub window: usize,
    /// Failure share of the window that opens the circuit (0 < ratio <= 1).
    pub failure_ratio: f64,
    /// Outcomes needed before the ratio is trusted.
    pub minimum_samples: usize,
    /// First open period in seconds. Every failed half-open trial doubles it.
    pub open_seconds: u64,
    /// Ceiling of the doubling open period in seconds.
    pub max_open_seconds: u64,
}

impl CircuitSettings {
    /// Upper bound of `window`; larger values are clamped when used.
    pub const MAX_WINDOW: usize = 1_000;
    /// Upper bound of both open periods. Matches the suspension ceiling.
    pub const MAX_OPEN_SECONDS: u64 = 365 * 24 * 60 * 60;

    /// Window size that is always usable, even when validation was skipped.
    pub fn effective_window(&self) -> usize {
        self.window.clamp(1, Self::MAX_WINDOW)
    }

    /// Failure ratio that is always usable. A non-finite value never opens
    /// early; it behaves like "only open when everything failed".
    pub fn effective_failure_ratio(&self) -> f64 {
        if self.failure_ratio.is_finite() {
            self.failure_ratio.clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// Minimum sample count, never above the window it is compared against.
    pub fn effective_minimum_samples(&self) -> usize {
        self.minimum_samples.clamp(1, self.effective_window())
    }

    /// First open period in seconds, at least one.
    pub fn effective_open_seconds(&self) -> u64 {
        self.open_seconds.clamp(1, Self::MAX_OPEN_SECONDS)
    }

    /// Ceiling of the open period, never below the first step.
    pub fn effective_max_open_seconds(&self) -> u64 {
        self.max_open_seconds
            .clamp(self.effective_open_seconds(), Self::MAX_OPEN_SECONDS)
    }
}

impl Default for CircuitSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            window: 20,
            failure_ratio: 0.6,
            minimum_samples: 10,
            open_seconds: 30,
            max_open_seconds: 600,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Search {
    pub safe_search: u8,
    /// Autocomplete backend chain, `` or `none` disables autocompletion.
    pub autocomplete: String,
    pub autocomplete_min: usize,
    pub default_lang: String,
    pub languages: Vec<String>,
    pub ban_time_on_fail: u64,
    pub max_ban_time_on_fail: u64,
    /// Consecutive empty answers before an engine is rested.
    pub empty_result_limit: u64,
    /// How long a rested engine stays out of the rotation.
    pub empty_result_cooldown: u64,
    pub suspended_times: BTreeMap<String, u64>,
    pub formats: Vec<String>,
    pub max_page: u32,
    pub timeout_limit: f64,
    /// How the engines of one search are started: all at once or tier by tier.
    pub strategy: SearchStrategy,
    /// Engines started per tier in the staged strategy.
    pub staged_tier: usize,
    /// The staged strategy stops starting new tiers at this many results.
    pub staged_min_results: usize,
    /// Seconds a parallel search keeps waiting once an engine runs past this
    /// point without having answered. 0 keeps the old behaviour of waiting
    /// for every engine up to the search deadline.
    ///
    /// One slow provider otherwise holds the whole request open to
    /// `timeout_limit`, which is where the three-second tail came from.
    pub early_stop_grace: f64,
    /// How many of the top results get their page text when the request asks
    /// for raw content. Larger means richer answers, slower fetches.
    pub raw_content_limit: usize,
    /// Strength of the URL/title query-term signal blended into the ranking.
    /// 0 keeps pure upstream position fusion; higher values lift the page that
    /// actually matches the query the way a neural ranker does.
    pub relevance_weight: f64,
    /// Weight of the multi-source agreement bonus: a result several engines
    /// returned ranks above an equally placed result only one engine returned.
    /// 0 disables the term.
    pub consensus_weight: f64,
    /// Weight of the title/snippet query-term match bonus. `relevance_weight`
    /// also sees URL slugs and body depth; this term looks at the title and
    /// snippet alone. 0 disables it.
    pub text_match_weight: f64,
    /// Merge AMP, mobile and print copies of one page, and results whose title
    /// and body are nearly identical. The rules are deliberately strict, so a
    /// false positive (merging two different pages) is unlikely.
    pub dedup_fuzzy: bool,
    /// OpenAI-compatible embeddings endpoint for semantic re-ranking
    /// (e.g. the bundled tools/embed_server.py, or any cloud embeddings API).
    /// Empty disables semantic re-ranking.
    pub embedding_url: String,
    pub embedding_model: String,
    /// EXPERIMENTAL: directory of the local BM25 index. Empty (the default)
    /// keeps the feature off. When a directory is set its hits are merged
    /// into the upstream results.
    pub local_index_path: String,
    /// Set false to keep the index on disk but out of the ranking.
    pub local_index_merge: bool,
    /// EXPERIMENTAL: directory of an index built by `rustxng-wikidump` (the
    /// opening of every Wikipedia article). Empty keeps it off. It is matched
    /// loosely (one content word is enough, ranking decides) and merged like
    /// any other source; it follows `local_index_merge`.
    pub wikipedia_index_path: String,
    /// Add the official website (Wikidata P856) of the top Wikipedia articles
    /// a general search found. One request to the Wikidata Query Service.
    pub official_sites: bool,
    /// Weight of those official-site results in the merge.
    pub official_sites_weight: f64,
    /// How long the merge waits for that lookup once the engines are done,
    /// in milliseconds. The Query Service usually answers in about half a
    /// second but sometimes takes seconds; a later answer is dropped.
    pub official_sites_wait_ms: u64,
    /// When not empty, only engines named here (display name or engine id,
    /// case-insensitive) are enabled and every other entry is off. An engine
    /// listed here runs even if it ships disabled. This is how a reviewed,
    /// "clean" provider set is pinned (see `settings.clean.toml`).
    pub engine_allowlist: Vec<String>,
    /// Longest page text returned per result by include_raw_content.
    pub raw_content_chars: usize,
    /// Optional command for JavaScript page rendering.
    pub page_renderer_command: Vec<String>,
    /// What happens when the page renderer fails: "static" keeps the text
    /// extracted from the original HTML, "none" discards it (empty body).
    pub page_renderer_fallback: PageRendererFallback,
    /// Decoded page body bytes read at most for raw content and MCP page
    /// reads. Values are clamped between 1 byte and 32 MiB; the default
    /// 4 MiB keeps the previous behaviour.
    pub raw_content_max_bytes: usize,
    /// How many extra result pages of the paging engines to merge into the
    /// answer. 0 (the default) only uses page 1; 1 adds page 2, and so on.
    pub deep_pages: u32,
    /// Run a site-scoped second pass when the named site is missing from the
    /// first pass (finds the reference page that a tutorial page outranks).
    pub site_scoped_retry: bool,
    /// How strongly canonical-site authority is blended into the ranking.
    /// Naming a site (arxiv, mdn, python, ...) lifts its canonical host and
    /// pushes its mirror copies down, which keywords and embeddings cannot see.
    pub authority_weight: f64,
    /// How strongly the cosine similarity is blended into the ranking.
    pub embedding_weight: f64,
    /// How many of the top results are re-ranked by embedding similarity.
    pub embedding_topk: usize,
    /// The statistical circuit breaker in front of the engines.
    pub circuit: CircuitSettings,
}

/// Behaviour of the JavaScript page renderer when rendering fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PageRendererFallback {
    /// Keep the text extracted from the original static HTML (default).
    #[default]
    Static,
    /// Discard the page text and report an empty body (previous behaviour).
    None,
}

/// Search execution strategy, upstream's default is "parallel".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchStrategy {
    /// Start every selected engine at once (SearXNG compatible).
    #[default]
    Parallel,
    /// Start engines tier by tier and stop once enough results were collected.
    Staged,
}

impl Default for Search {
    fn default() -> Self {
        // First suspension of a hard block. Repeats escalate in
        // `EngineStates`: 1 h -> 6 h -> 24 h -> 7 days, reset by a success.
        let mut suspended_times = BTreeMap::new();
        suspended_times.insert("SearxEngineAccessDenied".into(), 3_600);
        suspended_times.insert("SearxEngineCaptcha".into(), 3_600);
        // A 429 usually clears in minutes; an hour is far too punishing for
        // a good engine such as Yahoo! JAPAN (upstream defaults to 3600).
        suspended_times.insert("SearxEngineTooManyRequests".into(), 300);
        // A timeout is cut short by the search deadline and the bounded
        // parallel wait, so resting is not needed by default: measured with
        // the grace window in place, resting an engine here only produced
        // empty result sets without making the request faster. Operators who
        // want it can set `SearxEngineTimeout`.
        suspended_times.insert("SearxEngineTimeout".into(), 0);
        suspended_times.insert("cf_SearxEngineCaptcha".into(), 3_600);
        suspended_times.insert("cf_SearxEngineAccessDenied".into(), 3_600);
        suspended_times.insert("recaptcha_SearxEngineCaptcha".into(), 3_600);
        Self {
            safe_search: 0,
            autocomplete: "duckduckgo".into(),
            autocomplete_min: 4,
            default_lang: String::new(),
            languages: all_locale_tags(),
            ban_time_on_fail: 5,
            max_ban_time_on_fail: 120,
            empty_result_limit: 8,
            empty_result_cooldown: 300,
            suspended_times,
            formats: OUTPUT_FORMATS.iter().map(|f| (*f).to_string()).collect(),
            max_page: 0,
            timeout_limit: 3.0,
            strategy: SearchStrategy::Parallel,
            staged_tier: 8,
            staged_min_results: 10,
            early_stop_grace: 0.75,
            raw_content_limit: 5,
            relevance_weight: 4.0,
            consensus_weight: 0.5,
            text_match_weight: 0.1,
            dedup_fuzzy: true,
            embedding_url: String::new(),
            embedding_model: "BAAI/bge-small-en-v1.5".into(),
            local_index_path: String::new(),
            local_index_merge: true,
            wikipedia_index_path: String::new(),
            official_sites: true,
            official_sites_weight: 1.0,
            official_sites_wait_ms: 400,
            engine_allowlist: Vec::new(),
            raw_content_chars: 8_000,
            page_renderer_command: Vec::new(),
            page_renderer_fallback: PageRendererFallback::default(),
            raw_content_max_bytes: crate::fetch::MAX_BODY_BYTES,
            deep_pages: 0,
            site_scoped_retry: false,
            authority_weight: 8.0,
            embedding_weight: 2.5,
            embedding_topk: 25,
            circuit: CircuitSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Ui {
    pub default_theme: String,
    pub default_locale: String,
    pub center_alignment: bool,
    pub results_on_new_tab: bool,
    pub infinite_scroll: bool,
    pub search_on_category_select: bool,
    pub hotkeys: String,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            default_theme: "simple".into(),
            default_locale: String::new(),
            center_alignment: false,
            results_on_new_tab: false,
            infinite_scroll: false,
            search_on_category_select: true,
            hotkeys: "default".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Outgoing {
    pub request_timeout: f64,
    pub max_request_timeout: f64,
    pub useragent_suffix: String,
    pub pool_connections: usize,
    pub pool_maxsize: usize,
    pub enable_http2: bool,
    pub verify: bool,
    /// Additional trusted CA certificates in a PEM bundle.
    pub ca_bundle: Option<String>,
    pub retries: u32,
    pub max_redirects: usize,
    pub keepalive_expiry: f64,
    pub proxies: BTreeMap<String, String>,
    pub using_tor_proxy: bool,
    /// Announce ourselves honestly: every upstream request carries the fixed
    /// `rustxng/<version> (+repo; contact)` User-Agent instead of a rotating
    /// browser look-alike. API providers (Wikimedia, crates.io, Crossref, ...)
    /// ask for an identifiable client with a contact.
    pub identified_ua: bool,
    /// Contact (mail address or URL) added to the identified User-Agent.
    pub contact: String,
    /// Minimum pause between two requests to the same host, in milliseconds.
    /// 0 disables it. API etiquette is often "at most one request a second".
    pub host_spacing_ms: u64,
    /// Per-host exceptions to `host_spacing_ms`, keyed by host or domain
    /// (`"wikipedia.org"` covers every edition). The most specific key wins.
    pub host_spacing_overrides: BTreeMap<String, u64>,
}

impl Default for Outgoing {
    fn default() -> Self {
        Self {
            request_timeout: 4.0,
            max_request_timeout: 20.0,
            useragent_suffix: String::new(),
            pool_connections: 100,
            pool_maxsize: 20,
            enable_http2: true,
            verify: true,
            ca_bundle: None,
            retries: 1,
            max_redirects: 20,
            keepalive_expiry: 5.0,
            proxies: BTreeMap::new(),
            using_tor_proxy: false,
            identified_ua: false,
            contact: String::new(),
            host_spacing_ms: 0,
            host_spacing_overrides: BTreeMap::new(),
        }
    }
}

/// Response cache configuration (upstream `cache` and `valkey` sections).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CacheSettings {
    /// Backend: "memory" (default) or "valkey".
    pub backend: String,
    /// Connection URL of the valkey backend, e.g. redis://127.0.0.1:6379/0.
    pub url: String,
    /// Entry lifetime in seconds (upstream cache.ttl).
    pub ttl: u64,
    /// Upper bound of the in-memory cache in bytes (upstream cache.max_bytes).
    pub max_bytes: usize,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            backend: "memory".to_string(),
            url: "redis://127.0.0.1:6379/0".to_string(),
            ttl: 300,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Plugins {
    /// Plugins enabled for every request (upstream `enabled_plugins`).
    pub enabled: Vec<String>,
    /// Plugins disabled for every request (upstream `disabled_plugins`).
    pub disabled: Vec<String>,
}

/// One entry of the `engines` list.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EngineConfig {
    /// Display name, used by bangs and preferences.
    pub name: String,
    /// Engine implementation id (`searx/engines/<engine>.py` upstream). May
    /// be left out of a user entry that overrides a default engine by name.
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub shortcut: Option<String>,
    /// One category or a list; a string is split on commas.
    #[serde(default, deserialize_with = "string_or_seq")]
    pub categories: Vec<String>,
    /// `None` means "not specified"; an explicit `false` re-enables an engine
    /// that ships disabled in the defaults.
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub inactive: bool,
    #[serde(default)]
    pub weight: Option<f64>,
    #[serde(default)]
    pub timeout: Option<f64>,
    #[serde(default)]
    pub paging: Option<bool>,
    #[serde(default)]
    pub language_support: Option<bool>,
    #[serde(default)]
    pub time_range_support: Option<bool>,
    #[serde(default)]
    pub safesearch: Option<bool>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub search_url: Option<String>,
    /// Route this engine through its own proxy (upstream per-engine proxies).
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Free-form, engine specific options (kept for SearXNG compatibility).
    #[serde(flatten, default)]
    pub extra: toml::Table,
}

impl EngineConfig {
    /// Build an engine config programmatically (used by the built-in defaults
    /// and by tests).
    pub fn new(name: &str, engine: &str, shortcut: &str, categories: &[&str]) -> Self {
        Self {
            name: name.into(),
            engine: engine.into(),
            shortcut: Some(shortcut.into()),
            categories: categories.iter().map(|c| (*c).to_string()).collect(),
            proxy: None,
            disabled: None,
            inactive: false,
            weight: None,
            timeout: None,
            paging: None,
            language_support: None,
            time_range_support: None,
            safesearch: None,
            base_url: None,
            search_url: None,
            api_key: None,
            extra: toml::Table::new(),
        }
    }

    pub fn opt_str(&self, key: &str) -> Option<&str> {
        self.extra.get(key).and_then(|value| value.as_str())
    }

    pub fn opt_bool(&self, key: &str) -> Option<bool> {
        self.extra.get(key).and_then(|value| value.as_bool())
    }

    pub fn opt_i64(&self, key: &str) -> Option<i64> {
        self.extra.get(key).and_then(|value| value.as_integer())
    }

    pub fn opt_f64(&self, key: &str) -> Option<f64> {
        self.extra
            .get(key)
            .and_then(|value| value.as_float())
            .or_else(|| {
                self.extra
                    .get(key)
                    .and_then(|value| value.as_integer())
                    .map(|v| v as f64)
            })
    }

    pub fn opt_str_list(&self, key: &str) -> Vec<String> {
        self.extra
            .get(key)
            .and_then(|value| value.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Convert a YAML value into a TOML value (mappings become tables).
fn yaml_to_toml(value: &yaml_serde::Value) -> Option<toml::Value> {
    Some(match value {
        yaml_serde::Value::Null => return None,
        yaml_serde::Value::Bool(flag) => toml::Value::Boolean(*flag),
        yaml_serde::Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                toml::Value::Integer(integer)
            } else {
                toml::Value::Float(number.as_f64()?)
            }
        }
        yaml_serde::Value::String(text) => toml::Value::String(text.clone()),
        yaml_serde::Value::Sequence(items) => {
            toml::Value::Array(items.iter().filter_map(yaml_to_toml).collect())
        }
        yaml_serde::Value::Mapping(mapping) => {
            let mut table = toml::Table::new();
            for (key, value) in mapping {
                if let Some(key) = yaml_key(key) {
                    if let Some(value) = yaml_to_toml(value) {
                        table.insert(key, value);
                    }
                }
            }
            toml::Value::Table(table)
        }
        yaml_serde::Value::Tagged(tagged) => return yaml_to_toml(&tagged.value),
    })
}

fn yaml_key(value: &yaml_serde::Value) -> Option<String> {
    match value {
        yaml_serde::Value::String(text) => Some(text.clone()),
        yaml_serde::Value::Bool(flag) => Some(flag.to_string()),
        yaml_serde::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

impl Settings {
    /// Initialise the proxy signing key once per Searcher. Never retain the
    /// historical sample key, including when loaded from an older config.
    pub(crate) fn ensure_secret_key(&mut self) -> Result<(), String> {
        if self.server.secret_key.trim().is_empty() || self.server.secret_key == "ultrasecretkey" {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes)
                .map_err(|error| format!("cannot generate a proxy signing key: {error}"))?;
            self.server.secret_key = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        }
        Ok(())
    }

    /// Parse a SearXNG `settings.yml` (YAML) document and merge it onto the
    /// built-in defaults. Upstream's configuration format is YAML, so a
    /// drop-in `settings.yml` is converted to TOML values first (the merge
    /// logic works on those). Unlike [`Settings::load`] this does not apply the
    /// `SEARXNG_*` overrides and does not validate.
    pub fn from_yaml_str(text: &str) -> Result<Settings, SettingsError> {
        Self::merge_values(Self::defaults_value(), Self::user_value(text, true)?)
    }

    /// Upstream's `server.public_instance`, also accepted under `[general]`
    /// for configurations written for older versions of this port.
    pub fn is_public_instance(&self) -> bool {
        self.general.public_instance || self.server.public_instance
    }

    /// The built-in defaults (no user file involved).
    pub fn defaults() -> Settings {
        toml::from_str(DEFAULT_SETTINGS).expect("built-in settings.toml must be valid")
    }

    /// The built-in defaults as raw TOML values, the base of every merge.
    ///
    /// `search.suspended_times` ships only in `Search::default()`, so it is
    /// seeded here. Without the seed a user table would replace the shipped
    /// entries instead of extending them. A table in the defaults file wins
    /// over the Rust entries, so the seed merges instead of replacing.
    fn defaults_value() -> toml::Value {
        let mut value: toml::Value =
            toml::from_str(DEFAULT_SETTINGS).expect("built-in settings.toml must be valid");
        if let Some(toml::Value::Table(search)) = value.get_mut("search") {
            let mut suspended = toml::Value::try_from(Search::default().suspended_times)
                .expect("suspended_times is a table of integers");
            if let Some(shipped) = search.remove("suspended_times") {
                suspended = deep_merge(suspended, shipped);
            }
            search.insert("suspended_times".into(), suspended);
        }
        value
    }

    /// Parse a user document into raw TOML values. YAML is converted first so
    /// both formats share one merge path.
    fn user_value(text: &str, is_yaml: bool) -> Result<toml::Value, SettingsError> {
        if is_yaml {
            let yaml: yaml_serde::Value = yaml_serde::from_str(text)
                .map_err(|error| SettingsError::Validation(error.to_string()))?;
            yaml_to_toml(&yaml)
                .ok_or_else(|| SettingsError::Validation("empty YAML document".into()))
        } else {
            toml::from_str::<toml::Value>(text).map_err(SettingsError::Parse)
        }
    }

    /// Load settings from `path`, merged onto the built-in defaults, then apply
    /// the `SEARXNG_*` environment overrides.
    pub fn load(path: Option<&Path>) -> Result<Settings, SettingsError> {
        Self::load_profile(path, Profile::Standard)
    }

    /// Load settings for a profile: the built-in defaults, then the profile's
    /// overlay (built into the binary), then the file at `path`, then the
    /// `SEARXNG_*` environment overrides. A later layer wins.
    pub fn load_profile(path: Option<&Path>, profile: Profile) -> Result<Settings, SettingsError> {
        let overlay = profile
            .overlay()
            .map(|text| toml::from_str::<toml::Value>(text).map_err(SettingsError::Parse))
            .transpose()?;
        let mut settings = match path {
            Some(path) => {
                let raw = std::fs::read_to_string(path).map_err(|source| SettingsError::Read {
                    path: path.to_path_buf(),
                    source,
                })?;
                // Upstream configurations are YAML; TOML is also accepted.
                let is_yaml = matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("yml" | "yaml")
                );
                let user = Self::user_value(&raw, is_yaml)?;
                let user = match overlay {
                    Some(overlay) => layer(overlay, user),
                    None => user,
                };
                Self::merge_values(Self::defaults_value(), user).map_err(|error| {
                    // The merged deserialize works on values without spans, so a
                    // TOML type error loses its line and column. Re-parsing the
                    // user file alone recovers the spanned message. The two
                    // parsers disagree on TOML datetimes, so only swap in the
                    // re-parsed error when it reports the same problem.
                    if is_yaml {
                        return error;
                    }
                    if let SettingsError::Parse(original) = &error {
                        if let Err(spanned) = toml::from_str::<Settings>(&raw) {
                            if spanned.message() == original.message() {
                                return SettingsError::Parse(spanned);
                            }
                        }
                    }
                    error
                })?
            }
            None => match overlay {
                Some(overlay) => Self::merge_values(Self::defaults_value(), overlay)?,
                None => Settings::defaults(),
            },
        };
        settings.apply_env();
        settings.validate()?;
        Ok(settings)
    }

    /// Resolve the settings path from `SEARXNG_SETTINGS_PATH` (upstream
    /// behaviour) or `--config`.
    pub fn path_from_env() -> Option<PathBuf> {
        std::env::var("SEARXNG_SETTINGS_PATH")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    /// Merge a raw user document onto the raw built-in defaults, then
    /// deserialize once.
    ///
    /// Merging raw values is what keeps "not specified" apart from an explicit
    /// `false`, `0` or `[]`: deserializing the user file first fills every
    /// missing key with the Rust `Default` and loses that difference.
    ///
    /// The engine list is the one exception. It merges by engine name unless
    /// `use_default_settings` is `false`, so it is taken out of the raw
    /// document and merged on the typed form afterwards.
    fn merge_values(base: toml::Value, mut user: toml::Value) -> Result<Settings, SettingsError> {
        let user_engines = strip_engines(&mut user);
        let merged = deep_merge(base, user);
        let mut settings: Settings = merged.try_into().map_err(SettingsError::Parse)?;

        if settings.use_default_settings == Some(false) {
            settings.engines.clear();
        }

        if let Some(engines) = user_engines {
            let engines: Vec<EngineConfig> = engines.try_into().map_err(SettingsError::Parse)?;
            settings.engines = if settings.use_default_settings.unwrap_or(true) {
                merge_engines_by_name(settings.engines, engines)
            } else {
                engines
            };
        }
        Ok(settings)
    }

    fn apply_env(&mut self) {
        if let Ok(value) = std::env::var("SEARXNG_BIND_ADDRESS") {
            if !value.is_empty() {
                self.server.bind_address = value;
            }
        }
        if let Ok(value) = std::env::var("SEARXNG_PORT") {
            if let Ok(port) = value.parse::<u16>() {
                self.server.port = port;
            }
        }
        if let Ok(value) = std::env::var("SEARXNG_SECRET") {
            if !value.is_empty() {
                self.server.secret_key = value;
            }
        }
        if let Ok(value) = std::env::var("SEARXNG_BASE_URL") {
            if !value.is_empty() {
                self.server.base_url = Some(value);
            }
        }
        if let Ok(value) = std::env::var("SEARXNG_DEBUG") {
            self.general.debug = matches!(value.as_str(), "1" | "true" | "on");
        }
        if let Ok(value) = std::env::var("SEARXNG_ENABLE_METRICS") {
            self.general.enable_metrics = !matches!(value.as_str(), "0" | "false" | "off");
        }
        if let Ok(value) = std::env::var("SEARXNG_SEARCH_DEFAULT_LANG") {
            if !value.is_empty() {
                self.search.default_lang = value;
            }
        }
    }

    /// Validate the settings the way upstream does before starting.
    pub fn validate(&self) -> Result<(), SettingsError> {
        if let Some(engine) = self
            .engines
            .iter()
            .find(|engine| engine.engine.trim().is_empty())
        {
            return Err(SettingsError::Validation(format!(
                "engine {:?} needs `engine` (only entries that override a default engine by name may leave it out)",
                engine.name
            )));
        }
        if self.server.port == 0 {
            return Err(SettingsError::Validation(
                "server.port must not be 0".into(),
            ));
        }
        if self.server.method != "GET" && self.server.method != "POST" {
            return Err(SettingsError::Validation(format!(
                "server.method must be GET or POST, got {:?}",
                self.server.method
            )));
        }
        if let Some(config) = self.server.limiter_config() {
            if config.botdetection.ipv4_prefix > 32 {
                return Err(SettingsError::Validation(format!(
                    "server.limiter.botdetection.ipv4_prefix must be 0-32, got {}",
                    config.botdetection.ipv4_prefix
                )));
            }
            if config.botdetection.ipv6_prefix > 128 {
                return Err(SettingsError::Validation(format!(
                    "server.limiter.botdetection.ipv6_prefix must be 0-128, got {}",
                    config.botdetection.ipv6_prefix
                )));
            }
        }
        if self.search.safe_search > 2 {
            return Err(SettingsError::Validation(
                "search.safe_search must be 0, 1 or 2".into(),
            ));
        }
        if !self.fetch.timeout.is_finite()
            || self.fetch.timeout <= 0.0
            || std::time::Duration::try_from_secs_f64(self.fetch.timeout).is_err()
        {
            return Err(SettingsError::Validation(
                "fetch.timeout must be a finite positive duration".into(),
            ));
        }
        if !self.search.timeout_limit.is_finite()
            || self.search.timeout_limit <= 0.0
            || std::time::Duration::try_from_secs_f64(self.search.timeout_limit).is_err()
        {
            return Err(SettingsError::Validation(
                "search.timeout_limit must be a finite positive duration".into(),
            ));
        }
        if self.outgoing.request_timeout <= 0.0 || self.outgoing.max_request_timeout <= 0.0 {
            return Err(SettingsError::Validation(
                "outgoing timeouts must be positive".into(),
            ));
        }
        if self.outgoing.max_request_timeout < self.outgoing.request_timeout {
            return Err(SettingsError::Validation(
                "outgoing.max_request_timeout must be >= outgoing.request_timeout".into(),
            ));
        }
        for format in &self.search.formats {
            if !OUTPUT_FORMATS.contains(&format.as_str()) {
                return Err(SettingsError::Validation(format!(
                    "unknown search format {format:?} (allowed: {})",
                    OUTPUT_FORMATS.join(", ")
                )));
            }
        }
        self.validate_cache()?;
        self.validate_ranking()?;
        self.validate_suspension()?;
        self.validate_circuit()?;
        self.validate_research()?;
        self.validate_base_url()?;
        let mut names: BTreeMap<&str, &str> = BTreeMap::new();
        let mut shortcuts: BTreeMap<&str, &str> = BTreeMap::new();
        for engine in &self.engines {
            if engine.name.trim().is_empty() {
                return Err(SettingsError::Validation(
                    "engine entries need a name".into(),
                ));
            }
            if engine.engine.trim().is_empty() {
                return Err(SettingsError::Validation(format!(
                    "engine {:?} needs an engine type",
                    engine.name
                )));
            }
            // The historical one-character split turned "general" into 7
            // categories. A real category is never a single character, so a
            // list that consists only of them is a string split by mistake.
            if !engine.categories.is_empty()
                && engine
                    .categories
                    .iter()
                    .all(|category| category.chars().count() == 1)
            {
                return Err(SettingsError::Validation(format!(
                    "engine {:?}: categories {:?} look like a string split into characters; write [\"general\"]",
                    engine.name, engine.categories
                )));
            }
            if let Some(previous) = names.insert(engine.name.as_str(), engine.engine.as_str()) {
                return Err(SettingsError::Validation(format!(
                    "duplicate engine name {:?} ({previous} and {})",
                    engine.name, engine.engine
                )));
            }
            if let Some(shortcut) = engine.shortcut.as_deref() {
                if let Some(previous) = shortcuts.insert(shortcut, engine.name.as_str()) {
                    return Err(SettingsError::Validation(format!(
                        "duplicate engine shortcut {shortcut:?} ({previous} and {})",
                        engine.name
                    )));
                }
            }
        }
        Ok(())
    }

    /// The ranking coefficients are added to the position-fusion score, so a
    /// non-finite or negative value would make comparisons meaningless.
    fn validate_ranking(&self) -> Result<(), SettingsError> {
        for (name, value) in [
            ("search.consensus_weight", self.search.consensus_weight),
            ("search.text_match_weight", self.search.text_match_weight),
        ] {
            if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                return Err(SettingsError::Validation(format!(
                    "{name} must be a finite number between 0 and 100, got {value}"
                )));
            }
        }
        Ok(())
    }

    /// Suspension windows become `Instant + Duration`; a value large enough to
    /// overflow panics while the engine-state lock is held.
    fn validate_suspension(&self) -> Result<(), SettingsError> {
        const MAX_SUSPENSION_SECONDS: u64 = 365 * 24 * 60 * 60;
        for (reason, seconds) in &self.search.suspended_times {
            if *seconds > MAX_SUSPENSION_SECONDS {
                return Err(SettingsError::Validation(format!(
                    "search.suspended_times.{reason} must be at most {MAX_SUSPENSION_SECONDS} seconds"
                )));
            }
        }
        if self.search.empty_result_cooldown > MAX_SUSPENSION_SECONDS {
            return Err(SettingsError::Validation(format!(
                "search.empty_result_cooldown must be at most {MAX_SUSPENSION_SECONDS} seconds"
            )));
        }
        Ok(())
    }

    /// The circuit breaker must be usable as configured. Values outside the
    /// ranges the breaker supports would either never open it or leave an
    /// engine out of the rotation with no way back, so they are errors rather
    /// than silently clamped.
    fn validate_circuit(&self) -> Result<(), SettingsError> {
        let circuit = &self.search.circuit;
        if circuit.window == 0 || circuit.window > CircuitSettings::MAX_WINDOW {
            return Err(SettingsError::Validation(format!(
                "search.circuit.window must be 1-{}, got {}",
                CircuitSettings::MAX_WINDOW,
                circuit.window
            )));
        }
        if !circuit.failure_ratio.is_finite()
            || circuit.failure_ratio <= 0.0
            || circuit.failure_ratio > 1.0
        {
            return Err(SettingsError::Validation(format!(
                "search.circuit.failure_ratio must be in (0, 1], got {}",
                circuit.failure_ratio
            )));
        }
        if circuit.minimum_samples == 0 {
            return Err(SettingsError::Validation(
                "search.circuit.minimum_samples must be greater than 0".into(),
            ));
        }
        if circuit.minimum_samples > circuit.window {
            // A threshold the window can never reach would be dead config.
            return Err(SettingsError::Validation(format!(
                "search.circuit.minimum_samples ({}) must not exceed search.circuit.window ({})",
                circuit.minimum_samples, circuit.window
            )));
        }
        if circuit.open_seconds == 0 || circuit.open_seconds > CircuitSettings::MAX_OPEN_SECONDS {
            return Err(SettingsError::Validation(format!(
                "search.circuit.open_seconds must be 1-{} seconds, got {}",
                CircuitSettings::MAX_OPEN_SECONDS,
                circuit.open_seconds
            )));
        }
        if circuit.max_open_seconds < circuit.open_seconds
            || circuit.max_open_seconds > CircuitSettings::MAX_OPEN_SECONDS
        {
            return Err(SettingsError::Validation(format!(
                "search.circuit.max_open_seconds must be {}-{} seconds, got {}",
                circuit.open_seconds,
                CircuitSettings::MAX_OPEN_SECONDS,
                circuit.max_open_seconds
            )));
        }
        Ok(())
    }

    /// The cache backend is selected by name and must be usable as configured.
    fn validate_cache(&self) -> Result<(), SettingsError> {
        let backend = self.cache.backend.trim().to_ascii_lowercase();
        match backend.as_str() {
            "memory" => {
                if self.cache.max_bytes == 0 {
                    return Err(SettingsError::Validation(
                        "cache.max_bytes must be greater than 0".into(),
                    ));
                }
            }
            "valkey" => {
                if self.cache.ttl == 0 {
                    return Err(SettingsError::Validation(
                        "cache.ttl must be greater than 0 for the valkey backend".into(),
                    ));
                }
                let url = self.cache.url.trim();
                if !(url.starts_with("redis://") || url.starts_with("valkey://")) {
                    return Err(SettingsError::Validation(format!(
                        "cache.url must start with redis:// or valkey://, got {url:?}"
                    )));
                }
            }
            other => {
                return Err(SettingsError::Validation(format!(
                    "cache.backend must be memory or valkey, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// `/research` needs positive limits; zero would silently be clamped.
    fn validate_research(&self) -> Result<(), SettingsError> {
        for (name, value) in [
            ("research.max_queries", self.research.max_queries),
            ("research.max_sources", self.research.max_sources),
            ("research.max_concurrency", self.research.max_concurrency),
        ] {
            if value == 0 {
                return Err(SettingsError::Validation(format!(
                    "{name} must be greater than 0"
                )));
            }
        }
        if self.research.deadline == 0 {
            return Err(SettingsError::Validation(
                "research.deadline must be greater than 0".into(),
            ));
        }
        Ok(())
    }

    /// `server.base_url` feeds feeds and the OpenSearch description, so a
    /// value that is not an absolute http(s) URL would produce broken links.
    fn validate_base_url(&self) -> Result<(), SettingsError> {
        let Some(base) = self
            .server
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(());
        };
        let parsed = url::Url::parse(base).map_err(|error| {
            SettingsError::Validation(format!("server.base_url is not a URL: {error}"))
        })?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(SettingsError::Validation(format!(
                "server.base_url must be an absolute http(s) URL, got {base:?}"
            )));
        }
        Ok(())
    }

    /// Valid settings that are still likely to surprise an operator.
    ///
    /// `--check-config` prints these next to the readiness report and the
    /// server logs them once at startup. They are not errors: the instance
    /// runs, but the behaviour may differ from what was intended.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.search.max_page == 0 {
            warnings.push(format!(
                "search.max_page = 0 (no configured limit): the HTTP API still rejects page numbers above {MAX_PAGENO}"
            ));
        }
        for (key, value) in [
            ("search.consensus_weight", self.search.consensus_weight),
            ("search.text_match_weight", self.search.text_match_weight),
        ] {
            if value > 20.0 {
                warnings.push(format!(
                    "{key} = {value} is high enough to override position fusion"
                ));
            }
        }
        let loopback = matches!(
            self.server.bind_address.as_str(),
            "127.0.0.1" | "localhost" | "::1"
        );
        if !loopback && !self.server.limiter.is_enabled() {
            warnings.push(
                "server.bind_address is not loopback while server.limiter is disabled".to_string(),
            );
        }
        if !self.general.enable_metrics {
            warnings.push("general.enable_metrics = false: /metrics answers 404".to_string());
        }
        if !self.search.circuit.enabled {
            warnings.push(
                "search.circuit.enabled = false: engines that keep failing stay in the rotation"
                    .to_string(),
            );
        }
        if self.research.enabled && self.research.api_key.trim().is_empty() {
            warnings.push(
                "research.enabled = true without research.api_key: /research returns evidence only"
                    .to_string(),
            );
        }
        // These keys are parsed so that a SearXNG configuration keeps
        // loading, but this port never reads them. A value that differs from
        // the built-in default is called out instead of silently doing nothing.
        let unimplemented =
            |key: &str| format!("{key} is accepted for compatibility but not implemented");
        if self.general.debug {
            warnings.push(unimplemented("general.debug"));
        }
        if self
            .server
            .http_protocol_version
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            warnings.push(unimplemented("server.http_protocol_version"));
        }
        let outgoing = Outgoing::default();
        if self.outgoing.pool_connections != outgoing.pool_connections {
            warnings.push(unimplemented("outgoing.pool_connections"));
        }
        if self.outgoing.keepalive_expiry != outgoing.keepalive_expiry {
            warnings.push(unimplemented("outgoing.keepalive_expiry"));
        }
        if self.ui.search_on_category_select != Ui::default().search_on_category_select {
            warnings.push(unimplemented("ui.search_on_category_select"));
        }
        if self.ui.hotkeys != Ui::default().hotkeys {
            warnings.push(unimplemented("ui.hotkeys"));
        }
        if self.doi_resolvers.is_some() {
            warnings.push(unimplemented("doi_resolvers"));
        }
        warnings
    }

    /// Engines that are enabled by default, in config order.
    pub fn enabled_engines(&self) -> impl Iterator<Item = &EngineConfig> {
        let allowlist: Vec<String> = self
            .search
            .engine_allowlist
            .iter()
            .map(|name| name.trim().to_lowercase())
            .collect();
        self.engines.iter().filter(move |engine| {
            if engine.inactive {
                return false;
            }
            if allowlist.is_empty() {
                return engine.disabled != Some(true);
            }
            allowlist.contains(&engine.name.to_lowercase())
                || allowlist.contains(&engine.engine.to_lowercase())
        })
    }

    pub fn engine(&self, name: &str) -> Option<&EngineConfig> {
        self.engines.iter().find(|engine| engine.name == name)
    }

    /// Default language list limited to what the UI offers.
    pub fn languages(&self) -> Vec<String> {
        self.search.languages.clone()
    }

    /// The base URL used for absolute links in feeds/opensearch.
    pub fn base_url(&self) -> String {
        self.server.base_url.clone().unwrap_or_else(|| {
            let host = if self.server.bind_address == "0.0.0.0" {
                "localhost"
            } else {
                self.server.bind_address.as_str()
            };
            format!("http://{host}:{}", self.server.port)
        })
    }
}

/// Take the `engines` array out of a raw document. Engine lists merge by name
/// on the typed form, never by value.
/// A set of built-in settings to start from (`--profile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// `settings.toml`: reviewed keyless default entries, subject to their recorded conditions.
    #[default]
    Standard,
    /// `settings.full.toml`: opt-in sources that may require additional permissions.
    /// Private or personal use does not override any provider's terms.
    Full,
    /// `settings.clean.toml`: search only locally prebuilt crawl and dump indexes.
    Clean,
}

impl Profile {
    pub const NAMES: &'static str = "standard, full, clean";

    /// Parse a profile name (`default` and `normal` mean `standard`).
    pub fn parse(name: &str) -> Result<Profile, String> {
        match name.trim().to_ascii_lowercase().as_str() {
            "standard" | "default" | "normal" => Ok(Profile::Standard),
            "full" => Ok(Profile::Full),
            "clean" => Ok(Profile::Clean),
            other => Err(format!(
                "unknown profile {other:?} (choose one of: {})",
                Profile::NAMES
            )),
        }
    }

    /// The profile named by `RUSTXNG_PROFILE`, if set.
    pub fn from_env() -> Result<Option<Profile>, String> {
        match std::env::var("RUSTXNG_PROFILE") {
            Ok(value) if !value.trim().is_empty() => Profile::parse(&value).map(Some),
            _ => Ok(None),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Profile::Standard => "standard",
            Profile::Full => "full",
            Profile::Clean => "clean",
        }
    }

    /// The settings this profile lays over the defaults.
    fn overlay(self) -> Option<&'static str> {
        match self {
            Profile::Standard => None,
            Profile::Full => Some(include_str!("../settings.full.toml")),
            Profile::Clean => Some(include_str!("../settings.clean.toml")),
        }
    }
}

/// Lay `user` over `profile`: tables merge key by key and the engine lists
/// are concatenated, so a user entry still overrides the profile's by name.
fn layer(mut profile: toml::Value, mut user: toml::Value) -> toml::Value {
    let engines = match (strip_engines(&mut profile), strip_engines(&mut user)) {
        (Some(toml::Value::Array(mut first)), Some(toml::Value::Array(second))) => {
            first.extend(second);
            Some(toml::Value::Array(first))
        }
        (first, second) => second.or(first),
    };
    let mut merged = deep_merge(profile, user);
    if let (Some(engines), toml::Value::Table(table)) = (engines, &mut merged) {
        table.insert("engines".into(), engines);
    }
    merged
}

fn strip_engines(value: &mut toml::Value) -> Option<toml::Value> {
    match value {
        toml::Value::Table(table) => table.remove("engines"),
        _ => None,
    }
}

fn deep_merge(base: toml::Value, user: toml::Value) -> toml::Value {
    match (base, user) {
        (toml::Value::Table(mut base), toml::Value::Table(user)) => {
            for (key, value) in user {
                let merged = match base.remove(&key) {
                    Some(existing) => deep_merge(existing, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            toml::Value::Table(base)
        }
        (_, user) => user,
    }
}

/// Merge user engine entries into the default list by engine name.
fn merge_engines_by_name(
    defaults: Vec<EngineConfig>,
    user: Vec<EngineConfig>,
) -> Vec<EngineConfig> {
    let mut merged = defaults;
    for engine in user {
        match merged.iter_mut().find(|entry| entry.name == engine.name) {
            Some(existing) => {
                // User entries win field by field; unset fields keep the default.
                if engine.shortcut.is_some() {
                    existing.shortcut = engine.shortcut.clone();
                }
                if !engine.categories.is_empty() {
                    existing.categories = engine.categories.clone();
                }
                if let Some(disabled) = engine.disabled {
                    existing.disabled = Some(disabled);
                }
                if engine.inactive {
                    existing.inactive = true;
                }
                if engine.weight.is_some() {
                    existing.weight = engine.weight;
                }
                if engine.timeout.is_some() {
                    existing.timeout = engine.timeout;
                }
                if engine.paging.is_some() {
                    existing.paging = engine.paging;
                }
                if engine.language_support.is_some() {
                    existing.language_support = engine.language_support;
                }
                if engine.time_range_support.is_some() {
                    existing.time_range_support = engine.time_range_support;
                }
                if engine.safesearch.is_some() {
                    existing.safesearch = engine.safesearch;
                }
                if engine.base_url.is_some() {
                    existing.base_url = engine.base_url.clone();
                }
                if engine.search_url.is_some() {
                    existing.search_url = engine.search_url.clone();
                }
                if engine.proxy.is_some() {
                    existing.proxy = engine.proxy.clone();
                }
                if engine.api_key.is_some() {
                    existing.api_key = engine.api_key.clone();
                }
                for (key, value) in engine.extra {
                    existing.extra.insert(key, value);
                }
            }
            None => merged.push(engine),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_keys_are_random_for_empty_or_legacy_defaults_and_custom_keys_survive() {
        let mut first = Settings::defaults();
        first.ensure_secret_key().unwrap();
        let mut second = Settings::defaults();
        second.server.secret_key = "ultrasecretkey".into();
        second.ensure_secret_key().unwrap();
        assert_eq!(first.server.secret_key.len(), 64);
        assert!(
            first
                .server
                .secret_key
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        );
        assert_eq!(second.server.secret_key.len(), 64);
        assert_ne!(first.server.secret_key, second.server.secret_key);
        let original = first.server.secret_key.clone();
        first.ensure_secret_key().unwrap();
        assert_eq!(first.server.secret_key, original);
        second.server.secret_key = "operator-managed-key".into();
        second.ensure_secret_key().unwrap();
        assert_eq!(second.server.secret_key, "operator-managed-key");
    }

    /// Merge `text` the way `Settings::load` does, but without the `SEARXNG_*`
    /// overrides, so an ambient environment variable cannot flip an assertion.
    fn merged(text: &str, is_yaml: bool) -> Settings {
        let user = Settings::user_value(text, is_yaml).expect("user settings parse");
        Settings::merge_values(Settings::defaults_value(), user).expect("settings merge")
    }

    fn merged_toml(text: &str) -> Settings {
        merged(text, false)
    }

    fn merged_yaml(text: &str) -> Settings {
        merged(text, true)
    }

    /// Load `text` through the real file path and `Settings::load`, the entry
    /// point the server uses.
    fn load_file(file_name: &str, text: &str) -> Settings {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join(file_name);
        std::fs::write(&path, text).expect("write the settings file");
        Settings::load(Some(&path)).expect("settings load")
    }

    #[test]
    fn a_partial_file_loads_through_the_cli_entry_point() {
        let settings = load_file("settings.toml", "[server]\nport = 9999\n");

        assert_eq!(settings.cache.max_bytes, 33_554_432);
        assert_eq!(settings.ui.default_locale, "en");
        assert!(settings.engine("wikipedia").is_some());
    }

    #[test]
    fn a_yaml_file_loads_through_the_cli_entry_point() {
        let settings = load_file("settings.yml", "server:\n  port: 9999\n");

        assert_eq!(settings.cache.max_bytes, 33_554_432);
        assert_eq!(settings.ui.default_locale, "en");
    }

    #[test]
    fn a_partial_user_file_keeps_the_shipped_defaults() {
        let defaults = Settings::defaults();
        let settings = merged_toml("[server]\nport = 9999\n");

        assert_eq!(settings.server.port, 9999);
        assert_eq!(settings.cache.max_bytes, 33_554_432);
        assert_eq!(settings.search.default_lang, "auto");
        assert_eq!(settings.ui.default_locale, "en");
        assert_eq!(
            settings.general.instance_name,
            defaults.general.instance_name
        );
        assert_eq!(settings.search.languages, defaults.search.languages);
        assert_eq!(
            settings.outgoing.request_timeout,
            defaults.outgoing.request_timeout
        );
    }

    #[test]
    fn an_empty_user_file_keeps_every_default() {
        let defaults = Settings::defaults();
        let settings = merged_toml("");

        assert_eq!(settings.server.port, defaults.server.port);
        assert_eq!(settings.cache.max_bytes, defaults.cache.max_bytes);
        assert_eq!(settings.ui.default_locale, defaults.ui.default_locale);
        assert_eq!(settings.engines.len(), defaults.engines.len());
    }

    #[test]
    fn a_partial_yaml_file_keeps_the_shipped_defaults() {
        let settings = merged_yaml("server:\n  port: 9999\n");

        assert_eq!(settings.server.port, 9999);
        assert_eq!(settings.ui.default_locale, "en");
        assert_eq!(settings.search.default_lang, "auto");
    }

    #[test]
    fn a_partial_suspension_map_extends_the_shipped_entries() {
        let settings = merged_toml(
            r#"
[search.suspended_times]
SearxEngineTooManyRequests = 60
"#,
        );

        let suspended = &settings.search.suspended_times;
        assert_eq!(suspended.get("SearxEngineTooManyRequests"), Some(&60));
        assert_eq!(suspended.get("SearxEngineCaptcha"), Some(&3_600));
        assert_eq!(suspended.get("cf_SearxEngineCaptcha"), Some(&3_600));
    }

    #[test]
    fn a_nested_limiter_table_keeps_the_botdetection_defaults() {
        let settings = merged_toml(
            r#"
[server.limiter]
enabled = true

[server.limiter.botdetection.ip_limit]
filter_link_local = true
"#,
        );

        assert!(settings.server.limiter_enabled());
        let limiter = settings.server.limiter_config().expect("table form");
        assert_eq!(limiter.botdetection.ipv4_prefix, 32);
        assert_eq!(
            limiter.botdetection.trusted_proxies,
            vec!["127.0.0.0/8".to_string(), "::1".to_string()]
        );
        assert!(limiter.botdetection.ip_limit.filter_link_local);
        assert!(!limiter.botdetection.ip_limit.link_token);
        assert!(limiter.botdetection.ip_lists.pass_searxng_org);
    }

    #[test]
    fn engine_specific_options_survive_the_merge() {
        let settings = merged_toml(
            r#"
[[engines]]
name = "my xpath"
engine = "xpath"
shortcut = "mx"
categories = ["general"]
content_xpath = "//p"
"#,
        );

        // shipped engine options (the flattened `extra` table) stay
        let shipped = settings.engine("stack overflow").expect("stack overflow");
        assert_eq!(shipped.opt_str("api_site"), Some("stackoverflow"));
        // and a user engine keeps its own options
        let added = settings.engine("my xpath").expect("my xpath");
        assert_eq!(added.opt_str("content_xpath"), Some("//p"));
    }

    #[test]
    fn a_user_tab_list_is_kept() {
        let settings = merged_toml("categories_as_tabs = [\"general\"]\n");

        assert_eq!(settings.categories_as_tabs, vec!["general".to_string()]);
    }

    #[test]
    fn a_scalar_category_is_a_single_category() {
        // TOML
        let toml_settings = merged_toml(
            r#"
categories_as_tabs = "general"

[[engines]]
name = "scalar categories"
engine = "wikipedia"
categories = "general"
"#,
        );
        assert_eq!(toml_settings.categories_as_tabs, ["general"]);
        assert_eq!(
            toml_settings
                .engine("scalar categories")
                .expect("engine kept")
                .categories,
            ["general"]
        );

        // YAML, the upstream format
        let yaml_settings = merged_yaml(
            r#"
categories_as_tabs: general
engines:
  - name: scalar categories
    engine: wikipedia
    categories: general
"#,
        );
        assert_eq!(yaml_settings.categories_as_tabs, ["general"]);
        assert_eq!(
            yaml_settings
                .engine("scalar categories")
                .expect("engine kept")
                .categories,
            ["general"]
        );
    }

    #[test]
    fn a_comma_separated_category_scalar_is_split() {
        let settings = merged_toml(
            r#"
[[engines]]
name = "comma categories"
engine = "wikipedia"
categories = "general, images"
"#,
        );
        assert_eq!(
            settings
                .engine("comma categories")
                .expect("engine kept")
                .categories,
            ["general", "images"]
        );
    }

    #[test]
    fn a_one_character_category_list_names_the_engine() {
        let mut settings = Settings::defaults();
        let name = settings.engines[0].name.clone();
        settings.engines[0].categories = "general"
            .chars()
            .map(|character| character.to_string())
            .collect();

        let error = settings.validate().expect_err("a split string is rejected");
        assert!(error.to_string().contains(&name), "{error}");
        assert!(
            error.to_string().contains("split into characters"),
            "{error}"
        );

        // A real list, including a one-word category, stays valid.
        settings.engines[0].categories = vec!["general".into()];
        settings
            .validate()
            .expect("a normal category list is valid");
    }

    #[test]
    fn from_yaml_str_merges_onto_the_defaults() {
        let settings = Settings::from_yaml_str(
            r#"
server:
  port: 9999
engines:
  - name: "google"
    engine: google
    disabled: true
"#,
        )
        .expect("valid yaml");

        assert_eq!(settings.server.port, 9999);
        assert_eq!(settings.cache.max_bytes, 33_554_432);
        assert_eq!(settings.ui.default_locale, "en");
        assert_eq!(
            settings.engine("google").expect("google").disabled,
            Some(true)
        );
        assert!(settings.engine("wikipedia").is_some());
    }

    #[test]
    fn a_wrong_value_type_is_a_parse_error_that_names_the_line() {
        let user = Settings::user_value("[server]\nport = \"8888\"\n", false).expect("valid toml");
        let error = Settings::merge_values(Settings::defaults_value(), user)
            .expect_err("a string port is rejected");
        assert!(matches!(error, SettingsError::Parse(_)), "{error}");

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("settings.toml");
        std::fs::write(&path, "[server]\nport = \"8888\"\n").expect("write the settings file");
        let error = Settings::load(Some(&path)).expect_err("a string port is rejected");
        assert!(error.to_string().contains("line 2"), "{error}");
    }

    #[test]
    fn an_empty_yaml_file_is_rejected_as_validation() {
        let error = Settings::user_value("", true).expect_err("an empty document");
        assert!(matches!(error, SettingsError::Validation(_)), "{error}");
    }

    #[test]
    fn an_explicit_empty_format_list_is_kept() {
        assert!(!Settings::defaults().search.formats.is_empty());

        let settings = merged_toml("[search]\nformats = []\n");
        assert!(
            settings.search.formats.is_empty(),
            "explicit [] disables the extra formats"
        );
        assert_eq!(settings.search.default_lang, "auto");
        assert_eq!(settings.ui.default_locale, "en");
    }

    #[test]
    fn an_explicit_false_is_kept() {
        assert!(Settings::defaults().general.enable_metrics);

        let settings = merged_toml("[general]\nenable_metrics = false\n");
        assert!(!settings.general.enable_metrics);
        assert_eq!(settings.ui.default_locale, "en");
    }

    #[test]
    fn an_explicit_zero_is_kept() {
        assert_eq!(Settings::defaults().outgoing.retries, 1);

        let settings = merged_toml("[outgoing]\nretries = 0\n");
        assert_eq!(settings.outgoing.retries, 0);
        assert_eq!(settings.cache.max_bytes, 33_554_432);
    }

    #[test]
    fn a_partial_file_still_merges_engines_by_name() {
        let settings = merged_toml(
            r#"
[server]
port = 9999

[[engines]]
name = "google"
engine = "google"
disabled = true
"#,
        );

        assert_eq!(
            settings.engine("google").expect("google").disabled,
            Some(true)
        );
        assert!(
            settings.engine("wikipedia").is_some(),
            "shipped engines stay"
        );
        assert_eq!(settings.ui.default_locale, "en");
    }

    #[test]
    fn use_default_settings_false_replaces_the_engine_list() {
        let settings = merged_toml(
            r#"
use_default_settings = false

[[engines]]
name = "only"
engine = "wikipedia"
shortcut = "only"
"#,
        );

        assert_eq!(settings.engines.len(), 1);
        assert_eq!(settings.engines[0].name, "only");
        assert_eq!(settings.server.port, 29483);
    }

    #[test]
    fn disabling_default_engines_never_restores_them_for_empty_lists() {
        for text in [
            "use_default_settings = false\nengines = []\n",
            "use_default_settings = false\n",
        ] {
            assert!(merged_toml(text).engines.is_empty());
        }
        for text in [
            "use_default_settings: false\nengines: []\n",
            "use_default_settings: false\n",
        ] {
            assert!(merged_yaml(text).engines.is_empty());
        }
        assert!(!merged_toml("engines = []\n").engines.is_empty());
    }

    #[test]
    fn restricted_html_providers_are_not_enabled_by_default() {
        let settings = Settings::defaults();
        for name in ["yahoo japan", "mojeek"] {
            assert_eq!(settings.engine(name).unwrap().disabled, Some(true));
            assert!(!settings.enabled_engines().any(|engine| engine.name == name));
        }
        let partial = merged_toml("[server]\nport = 9999\n");
        assert_eq!(partial.engine("yahoo japan").unwrap().disabled, Some(true));
        assert_eq!(partial.engine("mojeek").unwrap().disabled, Some(true));
    }

    #[test]
    fn an_allowlist_enables_exactly_the_named_engines() {
        let settings = merged_toml(
            r#"
[search]
engine_allowlist = ["Wikipedia", "crates", "mojeek"]
"#,
        );
        let names: Vec<&str> = settings
            .enabled_engines()
            .map(|engine| engine.name.as_str())
            .collect();
        assert!(names.contains(&"wikipedia"), "{names:?}");
        // Matched by engine id too, and an allowed engine runs even if it
        // ships disabled.
        assert!(names.contains(&"crates.io"), "{names:?}");
        assert!(names.contains(&"mojeek"), "{names:?}");
        assert!(!names.contains(&"duckduckgo"), "{names:?}");
        assert_eq!(names.len(), 3, "{names:?}");
        // No allowlist keeps the shipped behaviour.
        assert!(Settings::defaults().enabled_engines().count() > names.len());
    }

    #[test]
    fn a_user_engine_entry_can_set_its_own_proxy() {
        let settings = merged_toml(
            r#"
[[engines]]
name = "mwmbl"
engine = "mwmbl"
proxy = "http://127.0.0.1:18888"
"#,
        );
        let engine = settings.engine("mwmbl").expect("mwmbl kept");
        assert_eq!(engine.proxy.as_deref(), Some("http://127.0.0.1:18888"));
        // engines without the key stay direct
        let other = settings.engine("swisscows").expect("swisscows kept");
        assert!(other.proxy.is_none());
    }

    #[test]
    fn a_user_can_enable_an_engine_that_ships_disabled() {
        // exaapi ships disabled in the defaults
        let shipped = Settings::defaults();
        assert_eq!(
            shipped.engine("exaapi").expect("exaapi exists").disabled,
            Some(true)
        );

        let settings = merged_toml(
            r#"
[[engines]]
name = "exaapi"
engine = "exaapi"
shortcut = "exa"
categories = ["general", "web"]
disabled = false
api_key = "test-key"
"#,
        );
        let enabled = settings.engine("exaapi").expect("exaapi kept");
        assert_eq!(enabled.disabled, Some(false), "explicit false must win");
        assert_eq!(enabled.api_key.as_deref(), Some("test-key"));
    }

    #[test]
    fn builtin_defaults_are_valid() {
        let settings = Settings::defaults();
        settings.validate().expect("defaults must validate");
        assert!(!settings.engines.is_empty());
        assert_eq!(settings.server.port, 29483);
    }

    #[test]
    fn builtin_default_port_is_29483() {
        assert_eq!(Server::default().port, 29483);
    }

    #[test]
    fn an_explicit_legacy_port_8888_in_user_config_still_wins() {
        let merged = merged_toml(
            r#"
use_default_settings = true
[server]
port = 8888
"#,
        );
        assert_eq!(merged.server.port, 8888);
        merged.validate().expect("legacy port must validate");
    }

    #[test]
    fn upstream_yaml_settings_are_accepted() {
        let yaml = r#"
use_default_settings: true
search:
  safe_search: 2
  default_lang: "ja"
server:
  port: 8880
engines:
  - name: "wikipedia (ja)"
    engine: wikipedia
    shortcut: wja
    categories: ["general"]
    base_url: "https://ja.wikipedia.org"
"#;
        let settings = merged_yaml(yaml);

        assert_eq!(settings.server.port, 8880);
        assert_eq!(settings.search.safe_search, 2);
        assert_eq!(settings.search.default_lang, "ja");
        assert!(settings.engine("wikipedia (ja)").is_some());
        assert_eq!(settings.ui.default_locale, "en");
    }

    #[test]
    fn yaml_settings_preserve_scalar_types_tags_and_aliases() {
        let settings = merged_yaml(
            r#"
general:
  instance_name: !label "RustXNG 日本語"
server:
  port: !!int "29484"
  secret_key: 000123
outgoing:
  request_timeout: !!float "1.25"
engines:
  - name: yaml fixture one
    engine: wikipedia
    base_url: &endpoint https://example.test/wiki
    categories: &categories [general, null, images]
    disabled: TRUE
    weight: &weight 0.75
    timeout: null
    negative_hex: -0x2a
    binary: 0b101
    description: |
      first line
      日本語の本文
  - name: yaml fixture two
    engine: wikipedia
    base_url: *endpoint
    categories: *categories
    weight: *weight
"#,
        );

        assert_eq!(settings.general.instance_name, "RustXNG 日本語");
        assert_eq!(settings.server.port, 29484);
        assert_eq!(settings.server.secret_key, "000123");
        assert_eq!(settings.outgoing.request_timeout, 1.25);
        let first = settings.engine("yaml fixture one").unwrap();
        let second = settings.engine("yaml fixture two").unwrap();
        assert_eq!(first.base_url.as_deref(), Some("https://example.test/wiki"));
        assert_eq!(second.base_url, first.base_url);
        assert_eq!(first.categories, ["general", "images"]);
        assert_eq!(second.categories, first.categories);
        assert_eq!(first.disabled, Some(true));
        assert_eq!(first.weight, Some(0.75));
        assert_eq!(second.weight, first.weight);
        assert_eq!(first.timeout, None);
        assert_eq!(first.opt_i64("negative_hex"), Some(-42));
        assert_eq!(first.opt_i64("binary"), Some(5));
        assert_eq!(
            first.opt_str("description"),
            Some("first line\n日本語の本文\n")
        );
    }

    #[test]
    fn yaml_mapping_keys_and_nulls_keep_their_conversion_rules() {
        let value = Settings::user_value(
            "options:\n  true: enabled\n  0x10: sixteen\n  1.5: decimal\n  omitted: null\n",
            true,
        )
        .unwrap();
        let options = value["options"].as_table().unwrap();
        assert_eq!(options["true"].as_str(), Some("enabled"));
        assert_eq!(options["16"].as_str(), Some("sixteen"));
        assert_eq!(options["1.5"].as_str(), Some("decimal"));
        assert!(!options.contains_key("omitted"));
    }

    #[test]
    fn malformed_yaml_and_unsafe_expansion_are_rejected() {
        for text in [
            "server: {port: 29483, port: 29484}",
            "server: {port: !!int invalid}",
            "server: *missing",
            "value: &recursive [*recursive]",
            "server: {port: 29483}\n---\nserver: {port: 29484}",
        ] {
            assert!(Settings::user_value(text, true).is_err(), "{text}");
        }
        let nested = format!("{}0{}", "[".repeat(129), "]".repeat(129));
        assert!(Settings::user_value(&nested, true).is_err());

        let mut aliases = String::from("a0: &a0 [payload]\n");
        for level in 1..8 {
            let repeated = vec![format!("*a{}", level - 1); 10].join(", ");
            aliases.push_str(&format!("a{level}: &a{level} [{repeated}]\n"));
        }
        assert!(Settings::user_value(&aliases, true).is_err());
    }

    #[test]
    fn the_cache_backend_must_be_usable_as_configured() {
        let mut settings = Settings::defaults();
        settings.cache.backend = "redis".into();
        assert!(settings.validate().is_err(), "unknown backend");
        settings.cache.backend = "valkey".into();
        settings.cache.url = "http://127.0.0.1:6379".into();
        assert!(settings.validate().is_err(), "valkey needs a redis URL");
        settings.cache.url = "redis://127.0.0.1:6379/0".into();
        settings.validate().expect("a redis URL is accepted");
        settings.cache.ttl = 0;
        assert!(settings.validate().is_err(), "ttl must be positive");
        settings.cache.ttl = 300;
        settings.cache.backend = "memory".into();
        settings.cache.max_bytes = 0;
        assert!(settings.validate().is_err(), "memory needs a size budget");
    }

    #[test]
    fn suspension_windows_stay_within_a_year() {
        let mut settings = Settings::defaults();
        settings
            .search
            .suspended_times
            .insert("SearxEngineAccessDenied".into(), 365 * 24 * 60 * 60);
        settings.validate().expect("a year is allowed");
        settings
            .search
            .suspended_times
            .insert("SearxEngineAccessDenied".into(), 365 * 24 * 60 * 60 + 1);
        assert!(
            settings.validate().is_err(),
            "a window larger than a year is rejected"
        );

        let mut settings = Settings::defaults();
        settings.search.empty_result_cooldown = 366 * 24 * 60 * 60;
        assert!(settings.validate().is_err(), "the cooldown is bounded too");
    }

    #[test]
    fn ranking_weights_are_finite_and_bounded() {
        let mut settings = Settings::defaults();
        settings.validate().expect("the shipped weights are valid");
        for value in [-1.0, 100.5, f64::NAN, f64::INFINITY] {
            settings.search.consensus_weight = value;
            assert!(
                settings.validate().is_err(),
                "consensus_weight = {value} must be rejected"
            );
        }
        settings.search.consensus_weight = 20.0;
        settings.search.text_match_weight = 100.0;
        settings.validate().expect("the upper bound is allowed");
        assert!(
            !settings
                .warnings()
                .iter()
                .any(|warning| warning.contains("consensus_weight")),
            "the default order of magnitude is not surprising"
        );
        settings.search.consensus_weight = 25.0;
        assert!(
            settings
                .warnings()
                .iter()
                .any(|warning| warning.contains("consensus_weight")),
            "a weight that can override fusion is explained"
        );
    }

    #[test]
    fn the_circuit_breaker_is_validated_and_clamped() {
        let mut settings = Settings::defaults();
        assert_eq!(settings.search.circuit, CircuitSettings::default());
        assert_eq!(settings.search.circuit, Search::default().circuit);
        settings
            .validate()
            .expect("the shipped circuit settings are valid");

        settings.search.circuit.window = 0;
        assert!(settings.validate().is_err(), "a zero window has no samples");
        settings.search.circuit = CircuitSettings::default();

        settings.search.circuit.failure_ratio = 0.0;
        assert!(
            settings.validate().is_err(),
            "a zero ratio would open on a clean window"
        );
        settings.search.circuit.failure_ratio = 1.5;
        assert!(settings.validate().is_err(), "a ratio is a share of one");
        settings.search.circuit.failure_ratio = f64::NAN;
        assert!(
            settings.validate().is_err(),
            "a non-finite ratio is rejected"
        );
        settings.search.circuit = CircuitSettings::default();

        settings.search.circuit.minimum_samples = 0;
        assert!(settings.validate().is_err());
        settings.search.circuit.minimum_samples = 21;
        assert!(
            settings.validate().is_err(),
            "a threshold the window cannot reach is dead config"
        );
        settings.search.circuit = CircuitSettings::default();

        settings.search.circuit.open_seconds = 0;
        assert!(settings.validate().is_err());
        settings.search.circuit.open_seconds = 30;
        settings.search.circuit.max_open_seconds = 29;
        assert!(
            settings.validate().is_err(),
            "the ceiling cannot be below the first step"
        );
        settings.search.circuit.max_open_seconds = CircuitSettings::MAX_OPEN_SECONDS + 1;
        assert!(settings.validate().is_err(), "the ladder is bounded");

        // Hand-built values are clamped instead of panicking, so a library
        // caller that skipped validation cannot divide by zero.
        let clamped = CircuitSettings {
            window: 0,
            minimum_samples: 0,
            failure_ratio: 2.0,
            open_seconds: 0,
            max_open_seconds: 0,
            ..CircuitSettings::default()
        };
        assert_eq!(clamped.effective_window(), 1);
        assert_eq!(clamped.effective_minimum_samples(), 1);
        assert_eq!(clamped.effective_failure_ratio(), 1.0);
        assert_eq!(clamped.effective_open_seconds(), 1);
        assert_eq!(clamped.effective_max_open_seconds(), 1);
    }

    #[test]
    fn warnings_explain_valid_but_surprising_settings() {
        let mut settings = Settings::defaults();
        settings.search.max_page = 20;
        settings.server.bind_address = "127.0.0.1".into();
        settings.server.limiter = LimiterSettings::Enabled(false);
        settings.general.enable_metrics = true;
        settings.research.enabled = false;
        assert!(settings.warnings().is_empty(), "{:?}", settings.warnings());

        settings.search.max_page = 0;
        assert!(
            settings
                .warnings()
                .iter()
                .any(|warning| warning.contains("max_page")),
            "the page cap is explained"
        );

        settings.search.max_page = 20;
        settings.server.bind_address = "0.0.0.0".into();
        assert!(
            settings
                .warnings()
                .iter()
                .any(|warning| warning.contains("limiter")),
            "a public bind without the limiter is called out"
        );

        settings.server.bind_address = "127.0.0.1".into();
        settings.research.enabled = true;
        assert!(
            settings
                .warnings()
                .iter()
                .any(|warning| warning.contains("api_key")),
            "research without a key is called out"
        );
    }

    #[test]
    fn unimplemented_settings_are_reported_once_each() {
        let mut settings = Settings::defaults();
        settings.general.debug = true;
        settings.server.http_protocol_version = Some("1.1".into());
        settings.outgoing.pool_connections = 7;
        settings.outgoing.keepalive_expiry = 1.0;
        settings.ui.search_on_category_select = false;
        settings.ui.hotkeys = "vim".into();
        settings.doi_resolvers = Some(toml::Table::new());

        let warnings = settings.warnings();
        for key in [
            "general.debug",
            "server.http_protocol_version",
            "outgoing.pool_connections",
            "outgoing.keepalive_expiry",
            "ui.search_on_category_select",
            "ui.hotkeys",
            "doi_resolvers",
        ] {
            let matching: Vec<&String> = warnings
                .iter()
                .filter(|warning| warning.contains(key))
                .collect();
            assert_eq!(matching.len(), 1, "{key}: {warnings:?}");
            assert!(
                matching[0].contains("accepted for compatibility but not implemented"),
                "{key}: {warnings:?}"
            );
        }

        // The built-in defaults do not trigger these warnings.
        assert!(
            Settings::defaults()
                .warnings()
                .iter()
                .all(|warning| !warning.contains("accepted for compatibility")),
            "{:?}",
            Settings::defaults().warnings()
        );
    }

    #[test]
    fn research_limits_must_be_positive() {
        let mut settings = Settings::defaults();
        settings.research.deadline = 0;
        assert!(settings.validate().is_err());
        settings.research.deadline = 120;
        settings.research.max_concurrency = 0;
        assert!(settings.validate().is_err());
        settings.research.max_concurrency = 2;
        settings.research.max_queries = 0;
        assert!(settings.validate().is_err());
        settings.research.max_queries = 5;
        settings.validate().expect("the defaults are valid");
    }

    #[test]
    fn a_relative_or_non_http_base_url_is_rejected() {
        let mut settings = Settings::defaults();
        settings.server.base_url = Some("/search".into());
        assert!(settings.validate().is_err(), "relative URL");
        settings.server.base_url = Some("ftp://example.com/".into());
        assert!(settings.validate().is_err(), "non-http scheme");
        settings.server.base_url = Some("https://search.example/".into());
        settings.validate().expect("absolute https URL");
        settings.server.base_url = Some(String::new());
        settings
            .validate()
            .expect("an empty value falls back to the listener");
    }

    #[test]
    fn user_engines_merge_by_name() {
        let settings = merged_toml(
            r#"
            use_default_settings = true
            [[engines]]
            name = "duckduckgo"
            engine = "duckduckgo"
            disabled = true
            [[engines]]
            name = "my instance"
            engine = "wikipedia"
            shortcut = "mywiki"
            categories = ["general"]
            "#,
        );
        let duckduckgo = settings.engine("duckduckgo").expect("duckduckgo kept");
        assert_eq!(duckduckgo.disabled, Some(true));
        assert!(settings.engine("my instance").is_some());
    }

    #[test]
    fn profiles_switch_the_provider_set() {
        let standard = Settings::load_profile(None, Profile::Standard).unwrap();
        let full = Settings::load_profile(None, Profile::Full).unwrap();
        let clean = Settings::load_profile(None, Profile::Clean).unwrap();

        let enabled = |settings: &Settings, name: &str| {
            settings.enabled_engines().any(|engine| engine.name == name)
        };
        assert!(!enabled(&standard, "exaapi"));
        assert!(!enabled(&standard, "yahoo japan"));
        assert!(enabled(&full, "yahoo japan"));
        assert!(enabled(&full, "exaapi"));
        assert!(enabled(&full, "wikipedia"));
        assert_eq!(clean.enabled_engines().count(), 0, "clean runs no engine");
        assert_eq!(clean.search.local_index_path, "index-docs");
        assert_eq!(clean.search.wikipedia_index_path, "index-wikipedia");
        assert!(!clean.search.official_sites);
        assert_eq!(Profile::parse("normal"), Ok(Profile::Standard));
        assert!(Profile::parse("everything").is_err());
    }

    #[test]
    fn a_settings_file_is_laid_over_the_profile() {
        let dir = std::env::temp_dir().join(format!("rustxng-profile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        std::fs::write(
            &path,
            "[server]\nport = 19999\n[[engines]]\nname = \"yahoo japan\"\ndisabled = true\n",
        )
        .unwrap();
        let settings = Settings::load_profile(Some(&path), Profile::Full).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(settings.server.port, 19999);
        assert_eq!(settings.general.instance_name, "RustXNG (full)");
        assert!(
            !settings
                .enabled_engines()
                .any(|engine| engine.name == "yahoo japan")
        );
        assert!(
            settings
                .enabled_engines()
                .any(|engine| engine.name == "yahoo")
        );
    }

    #[test]
    fn an_override_by_name_needs_no_engine_id() {
        let settings = merged_toml(
            r#"
            [[engines]]
            name = "yahoo"
            disabled = false
            "#,
        );
        let yahoo = settings.engine("yahoo").expect("yahoo kept");
        assert_eq!(yahoo.disabled, Some(false));
        assert_eq!(yahoo.engine, "yahoo");
        assert!(settings.validate().is_ok());

        let unknown = merged_toml(
            r#"
            [[engines]]
            name = "no such engine"
            "#,
        );
        assert!(unknown.validate().is_err());
    }

    #[test]
    fn invalid_format_is_rejected() {
        let mut settings = Settings::defaults();
        settings.search.formats.push("yaml".into());
        assert!(settings.validate().is_err());
    }

    #[test]
    fn deepseek_fetch_settings_are_parsed() {
        let settings = merged_toml(
            r#"
            [fetch]
            provider = "deepseek"
            timeout = 12.0
            [fetch.deepseek]
            base_url = "https://api.deepseek.com"
            api_key = "KEY"
            "#,
        );
        assert_eq!(settings.fetch.provider, FetchProvider::DeepSeek);
        assert_eq!(settings.fetch.timeout, 12.0);
        assert_eq!(settings.fetch.deepseek.api_key, "KEY");
    }

    #[test]
    fn fetch_timeout_must_be_finite_and_representable() {
        for (name, value) in [
            ("zero", "0.0"),
            ("negative", "-1.0"),
            ("infinity", "inf"),
            ("negative infinity", "-inf"),
            ("not a number", "nan"),
            ("out of range", "1e300"),
        ] {
            let settings = merged_toml(&format!("[fetch]\ntimeout = {value}\n"));
            assert!(
                settings.validate().is_err(),
                "fetch.timeout = {value} ({name}) must be rejected"
            );
        }
    }

    #[test]
    fn search_timeout_limit_must_be_finite_and_representable() {
        for (name, value) in [
            ("zero", "0.0"),
            ("negative", "-1.0"),
            ("infinity", "inf"),
            ("negative infinity", "-inf"),
            ("not a number", "nan"),
            ("out of range", "1e300"),
        ] {
            let settings = merged_toml(&format!("[search]\ntimeout_limit = {value}\n"));
            assert!(
                settings.validate().is_err(),
                "search.timeout_limit = {value} ({name}) must be rejected"
            );
        }
    }

    #[test]
    fn deepseek_engine_is_disabled_by_default() {
        let settings = Settings::defaults();
        let engine = settings.engine("deepseek").expect("deepseek entry");
        assert_eq!(engine.disabled, Some(true));
    }

    #[test]
    fn duplicate_shortcuts_are_rejected() {
        let mut settings = Settings::defaults();
        let mut clone = settings.engines[0].clone();
        clone.name = "clone of the first engine".into();
        clone.shortcut = settings.engines[0].shortcut.clone();
        settings.engines.push(clone);
        assert!(settings.validate().is_err());
    }
}
