use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DurationSpec(pub u64);

impl DurationSpec {
    pub fn seconds(self) -> u64 {
        self.0
    }

    pub fn std(self) -> std::time::Duration {
        std::time::Duration::from_secs(self.0)
    }
}

impl FromStr for DurationSpec {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(Self(0));
        }
        let split = value
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(value.len());
        let (number, unit) = value.split_at(split);
        let number: u64 = number
            .parse()
            .map_err(|_| format!("invalid duration {value:?}"))?;
        let multiplier = match unit.trim() {
            "" | "s" | "sec" | "secs" => 1,
            "m" | "min" | "mins" => 60,
            "h" | "hr" | "hrs" => 3_600,
            "d" | "day" | "days" => 86_400,
            "w" | "week" | "weeks" => 604_800,
            other => return Err(format!("unknown duration unit {other:?}")),
        };
        Ok(Self(number * multiplier))
    }
}

impl Serialize for DurationSpec {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{}s", self.0))
    }
}

impl<'de> Deserialize<'de> for DurationSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = DurationSpec;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a duration such as 30s, 15m, 1h or 7d")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value.parse().map_err(E::custom)
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(DurationSpec(value))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByteSize(pub usize);

impl ByteSize {
    pub fn bytes(self) -> usize {
        self.0
    }
}

impl FromStr for ByteSize {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        if value.is_empty() {
            return Err("empty size".to_string());
        }
        let split = value
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(value.len());
        let (number, unit) = value.split_at(split);
        let number: f64 = number
            .parse()
            .map_err(|_| format!("invalid size {value:?}"))?;
        let unit = unit.trim().to_ascii_lowercase();
        let multiplier = match unit.as_str() {
            "" | "b" => 1f64,
            "k" | "kb" => 1_000f64,
            "m" | "mb" => 1_000_000f64,
            "g" | "gb" => 1_000_000_000f64,
            "ki" | "kib" => 1024f64,
            "mi" | "mib" => 1024f64 * 1024f64,
            "gi" | "gib" => 1024f64 * 1024f64 * 1024f64,
            other => return Err(format!("unknown size unit {other:?}")),
        };
        Ok(Self((number * multiplier) as usize))
    }
}

impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0 as u64)
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = ByteSize;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a size such as 10MiB or a byte count")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                value.parse().map_err(E::custom)
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(ByteSize(value as usize))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(ByteSize(value.max(0) as usize))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub auth: AuthConfig,
    pub rights: RightsConfig,
    pub calendar: CalendarConfig,
    pub addressbook: AddressbookConfig,
    pub subscriptions: SubscriptionsConfig,
    pub feeds: FeedsConfig,
    pub web: WebConfig,
    pub logging: LoggingConfig,
    pub metrics: MetricsConfig,
    pub limits: LimitsConfig,
}

#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            storage: StorageConfig::default(),
            auth: AuthConfig::default(),
            rights: RightsConfig::default(),
            calendar: CalendarConfig::default(),
            addressbook: AddressbookConfig::default(),
            subscriptions: SubscriptionsConfig::default(),
            feeds: FeedsConfig::default(),
            web: WebConfig::default(),
            logging: LoggingConfig::default(),
            metrics: MetricsConfig::default(),
            limits: LimitsConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: Vec<String>,
    pub base_path: String,
    pub external_url: Option<String>,
    pub behind_proxy: bool,
    pub trusted_proxies: Vec<String>,
    pub max_connections: usize,
    pub request_timeout: DurationSpec,
    pub allow_insecure_remote: bool,
    pub max_request_body: ByteSize,
    pub max_item_size: ByteSize,
    pub max_addressbook_item_size: ByteSize,
    pub tls: bool,
    pub tls_certificate: String,
    pub tls_key: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: vec!["127.0.0.1:5232".to_string()],
            base_path: "/".to_string(),
            external_url: None,
            behind_proxy: false,
            trusted_proxies: Vec::new(),
            max_connections: 100,
            request_timeout: DurationSpec(30),
            allow_insecure_remote: false,
            max_request_body: ByteSize(10 * 1024 * 1024),
            max_item_size: ByteSize(2 * 1024 * 1024),
            max_addressbook_item_size: ByteSize(10 * 1024 * 1024),
            tls: false,
            tls_certificate: String::new(),
            tls_key: String::new(),
        }
    }
}

impl ServerConfig {
    pub fn trusted_networks(&self) -> Vec<IpNet> {
        self.trusted_proxies
            .iter()
            .filter_map(|value| value.parse().ok())
            .collect()
    }

    pub fn normalized_base_path(&self) -> String {
        let trimmed = self.base_path.trim_end_matches('/');
        if trimmed.is_empty() {
            "/".to_string()
        } else {
            format!("{trimmed}/")
        }
    }

    pub fn is_loopback_only(&self) -> bool {
        self.bind.iter().all(|address| {
            address
                .parse::<std::net::SocketAddr>()
                .map(|socket| socket.ip().is_loopback())
                .unwrap_or(false)
        })
    }

    pub fn url_for(&self, path: &str) -> String {
        let external = self
            .external_url
            .clone()
            .unwrap_or_else(|| "http://localhost:5232".to_string());
        format!(
            "{}/{}",
            external.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    pub data_dir: PathBuf,
    pub secret_key_file: PathBuf,
    pub fsync: bool,
    pub index_path: PathBuf,
    pub lock_timeout: DurationSpec,
    pub sync_history_days: u64,
    pub sync_history_max_changes: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("/var/lib/oxicale"),
            secret_key_file: PathBuf::new(),
            fsync: true,
            index_path: PathBuf::new(),
            lock_timeout: DurationSpec(10),
            sync_history_days: 90,
            sync_history_max_changes: 100_000,
        }
    }
}

impl StorageConfig {
    pub fn resolved_secret_key_file(&self) -> PathBuf {
        if self.secret_key_file.as_os_str().is_empty() {
            self.data_dir.join(".oxicale.key")
        } else {
            self.secret_key_file.clone()
        }
    }

    pub fn resolved_index_path(&self) -> PathBuf {
        if self.index_path.as_os_str().is_empty() {
            self.data_dir.join("index.sqlite3")
        } else {
            self.index_path.clone()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthBackend {
    Local,
    TrustedHeader,
    None,
    Deny,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    pub backend: AuthBackend,
    pub none_user: String,
    pub realm: String,
    pub password_file: PathBuf,
    pub session_lifetime: DurationSpec,
    pub session_cookie_name: String,
    pub secure_cookies: bool,
    pub rate_limit_failures: usize,
    pub rate_limit_window: DurationSpec,
    pub trusted_header: TrustedHeaderConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            backend: AuthBackend::Local,
            none_user: "anonymous".to_string(),
            realm: "Oxicale".to_string(),
            password_file: PathBuf::from("/etc/oxicale/users"),
            session_lifetime: DurationSpec(12 * 3600),
            session_cookie_name: "oxicale_session".to_string(),
            secure_cookies: true,
            rate_limit_failures: 10,
            rate_limit_window: DurationSpec(300),
            trusted_header: TrustedHeaderConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrustedHeaderConfig {
    pub header: String,
    pub require_proxy: bool,
}

impl Default for TrustedHeaderConfig {
    fn default() -> Self {
        Self {
            header: "X-Remote-User".to_string(),
            require_proxy: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightsBackend {
    OwnerOnly,
    OwnerWrite,
    Authenticated,
    FromFile,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RightsConfig {
    pub backend: RightsBackend,
    pub file: PathBuf,
}

impl Default for RightsConfig {
    fn default() -> Self {
        Self {
            backend: RightsBackend::OwnerOnly,
            file: PathBuf::from("/etc/oxicale/rights"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarConfig {
    pub default_name: String,
    pub default_slug: String,
    pub create_default: bool,
    pub allow_client_creation: bool,
    pub default_timezone: String,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            default_name: "Calendar".to_string(),
            default_slug: "calendar".to_string(),
            create_default: true,
            allow_client_creation: true,
            default_timezone: "UTC".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AddressbookConfig {
    pub enabled: bool,
    pub default_name: String,
    pub default_slug: String,
    pub create_default: bool,
}

impl Default for AddressbookConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_name: "Contacts".to_string(),
            default_slug: "contacts".to_string(),
            create_default: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubscriptionsConfig {
    pub enabled: bool,
    pub allow_http: bool,
    pub allow_private_networks: bool,
    pub allow_loopback: bool,
    pub allowed_hosts: Vec<String>,
    pub denied_hosts: Vec<String>,
    pub redirect_limit: usize,
    pub connect_timeout: DurationSpec,
    pub request_timeout: DurationSpec,
    pub max_response_size: ByteSize,
    pub max_components: usize,
    pub max_item_size: ByteSize,
    pub default_interval: DurationSpec,
    pub min_interval: DurationSpec,
    pub max_interval: DurationSpec,
    pub max_per_user: usize,
    pub user_agent: String,
    pub skip_broken_components: bool,
    pub outbound_proxy: String,
}

impl Default for SubscriptionsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_http: false,
            allow_private_networks: false,
            allow_loopback: false,
            allowed_hosts: Vec::new(),
            denied_hosts: Vec::new(),
            redirect_limit: 5,
            connect_timeout: DurationSpec(10),
            request_timeout: DurationSpec(60),
            max_response_size: ByteSize(10 * 1024 * 1024),
            max_components: 100_000,
            max_item_size: ByteSize(2 * 1024 * 1024),
            default_interval: DurationSpec(3600),
            min_interval: DurationSpec(900),
            max_interval: DurationSpec(7 * 86_400),
            max_per_user: 50,
            user_agent: "Oxicale/0.1".to_string(),
            skip_broken_components: false,
            outbound_proxy: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeedsConfig {
    pub enabled: bool,
    pub default_cache_control: String,
    pub rate_limit_per_minute: usize,
    pub max_per_calendar: usize,
    pub default_expiry: DurationSpec,
    pub allow_http_auth: bool,
}

impl Default for FeedsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_cache_control: "private, max-age=300, must-revalidate".to_string(),
            rate_limit_per_minute: 120,
            max_per_calendar: 20,
            default_expiry: DurationSpec(0),
            allow_http_auth: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Auto,
    Light,
    Dark,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebConfig {
    pub enabled: bool,
    pub theme: Theme,
    pub allow_basic_api_auth: bool,
    pub csp_extra: String,
    pub require_https: bool,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            theme: Theme::Auto,
            allow_basic_api_auth: true,
            csp_extra: String::new(),
            require_https: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogTarget {
    Stderr,
    Stdout,
    File,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub level: String,
    pub format: LogFormat,
    pub target: LogTarget,
    pub file: String,
    pub redact_headers: Vec<String>,
    pub log_request_bodies: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: LogFormat::Text,
            target: LogTarget::Stderr,
            file: String::new(),
            redact_headers: vec![
                "Authorization".to_string(),
                "Cookie".to_string(),
                "Set-Cookie".to_string(),
            ],
            log_request_bodies: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub bind: String,
    pub path: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "127.0.0.1:9090".to_string(),
            path: "/metrics".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    pub max_collections_per_user: usize,
    pub max_items_per_collection: usize,
    pub max_principals: usize,
    pub max_connections_per_user: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_collections_per_user: 100,
            max_items_per_collection: 100_000,
            max_principals: 10_000,
            max_connections_per_user: 20,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LoadedConfig {
    pub config: Config,
    pub warnings: Vec<String>,
    pub loaded_files: Vec<PathBuf>,
}

impl Config {
    /// Load configuration according to the documented priority order.
    pub fn load(explicit: Option<&Path>) -> Result<LoadedConfig, String> {
        let mut files: Vec<PathBuf> = Vec::new();
        let etc = PathBuf::from("/etc/oxicale/oxicale.toml");
        if etc.is_file() {
            files.push(etc);
        }
        let cwd = PathBuf::from("oxicale.toml");
        if cwd.is_file() {
            files.push(cwd);
        }
        if let Ok(env_path) = std::env::var("OXICALE_CONFIG") {
            let path = PathBuf::from(env_path);
            if !path.is_file() {
                return Err(format!(
                    "OXICALE_CONFIG points to missing file {}",
                    path.display()
                ));
            }
            files.push(path);
        }
        if let Some(explicit) = explicit {
            if !explicit.is_file() {
                return Err(format!(
                    "configuration file {} does not exist",
                    explicit.display()
                ));
            }
            files.push(explicit.to_path_buf());
        }

        let mut merged = serde_json::Value::Object(Default::default());
        let mut loaded_files = Vec::new();
        for path in files {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let value: toml::Value = toml::from_str(&text)
                .map_err(|error| format!("invalid TOML in {}: {error}", path.display()))?;
            let json = serde_json::to_value(value)
                .map_err(|error| format!("cannot convert {}: {error}", path.display()))?;
            merge_json(&mut merged, json);
            loaded_files.push(path);
        }
        apply_env(&mut merged)?;
        let config: Config = serde_json::from_value(merged)
            .map_err(|error| format!("invalid configuration: {error}"))?;
        let warnings = config.validate()?;
        Ok(LoadedConfig {
            config,
            warnings,
            loaded_files,
        })
    }

    pub fn from_toml(text: &str) -> Result<Config, String> {
        let value: toml::Value =
            toml::from_str(text).map_err(|error| format!("invalid TOML: {error}"))?;
        let json = serde_json::to_value(value).map_err(|error| error.to_string())?;
        serde_json::from_value(json).map_err(|error| format!("invalid configuration: {error}"))
    }

    pub fn validate(&self) -> Result<Vec<String>, String> {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let base = &self.server.base_path;
        if !base.starts_with('/') || base.contains("..") || base.contains('\\') {
            errors.push("server.base_path must be a clean absolute path".to_string());
        }
        if self.server.behind_proxy && self.server.trusted_proxies.is_empty() {
            errors.push(
                "server.behind_proxy requires a non-empty server.trusted_proxies list".to_string(),
            );
        }
        for proxy in &self.server.trusted_proxies {
            if proxy.parse::<IpNet>().is_err() {
                errors.push(format!("invalid trusted proxy range {proxy:?}"));
            }
        }
        if self.auth.backend == AuthBackend::None
            && !self.server.is_loopback_only()
            && !self.server.allow_insecure_remote
        {
            errors.push(
                "auth.backend = 'none' may not bind beyond loopback without server.allow_insecure_remote = true"
                    .to_string(),
            );
        }
        if self.auth.backend == AuthBackend::TrustedHeader {
            if !self.server.behind_proxy {
                errors.push(
                    "auth.backend = 'trusted_header' requires server.behind_proxy = true"
                        .to_string(),
                );
            }
            if self.server.trusted_proxies.is_empty() {
                errors.push(
                    "auth.backend = 'trusted_header' requires server.trusted_proxies".to_string(),
                );
            }
            if self.auth.trusted_header.header.trim().is_empty() {
                errors.push("auth.trusted_header.header must not be empty".to_string());
            }
        }
        if self.server.tls {
            if self.server.tls_certificate.is_empty() || self.server.tls_key.is_empty() {
                errors.push("server.tls requires tls_certificate and tls_key".to_string());
            } else {
                if !Path::new(&self.server.tls_certificate).is_file() {
                    errors.push(format!(
                        "TLS certificate {} is not readable",
                        self.server.tls_certificate
                    ));
                }
                if !Path::new(&self.server.tls_key).is_file() {
                    errors.push(format!("TLS key {} is not readable", self.server.tls_key));
                }
            }
        }
        if self.subscriptions.allow_private_networks || self.subscriptions.allow_loopback {
            warnings.push(
                "subscription network access is relaxed; SSRF protections for private/loopback addresses are disabled"
                    .to_string(),
            );
        }
        if self.subscriptions.enabled
            && self.server.external_url.is_none()
            && !self.server.is_loopback_only()
        {
            warnings.push(
                "server.external_url is unset; generated feed URLs may be incorrect".to_string(),
            );
        }
        if self.subscriptions.min_interval.seconds() < 900 {
            errors.push("subscriptions.min_interval must be at least 15m".to_string());
        }
        if self.subscriptions.default_interval.seconds() < self.subscriptions.min_interval.seconds()
        {
            errors.push(
                "subscriptions.default_interval must be at least subscriptions.min_interval"
                    .to_string(),
            );
        }
        if self.subscriptions.max_interval.seconds() < self.subscriptions.min_interval.seconds() {
            errors.push(
                "subscriptions.max_interval must be at least subscriptions.min_interval"
                    .to_string(),
            );
        }
        if self.web.enabled && self.server.bind.iter().any(|b| b.trim().is_empty()) {
            errors.push("server.bind contains an empty address".to_string());
        }
        if self.logging.target == LogTarget::File && self.logging.file.is_empty() {
            errors.push("logging.target = 'file' requires logging.file".to_string());
        }
        if self
            .calendar
            .default_timezone
            .parse::<chrono_tz::Tz>()
            .is_err()
        {
            errors.push(format!(
                "calendar.default_timezone {:?} is not a valid IANA timezone",
                self.calendar.default_timezone
            ));
        }
        if oxicale_core::Slug::parse(self.calendar.default_slug.clone()).is_err() {
            errors.push("calendar.default_slug is not a valid path segment".to_string());
        }
        if oxicale_core::Slug::parse(self.addressbook.default_slug.clone()).is_err() {
            errors.push("addressbook.default_slug is not a valid path segment".to_string());
        }
        if self.rights.backend == RightsBackend::FromFile {
            errors.push(
                "rights.backend = 'from_file' is not implemented in this release; use owner_only, owner_write or authenticated"
                    .to_string(),
            );
        }
        if self.metrics.enabled {
            if self.metrics.bind.parse::<std::net::SocketAddr>().is_err() {
                errors.push(format!(
                    "metrics.bind {:?} is not a valid socket address",
                    self.metrics.bind
                ));
            }
            if !self.metrics.path.starts_with('/') {
                errors.push("metrics.path must start with '/'".to_string());
            }
        }
        if self.web.require_https {
            match &self.server.external_url {
                Some(url) if url.starts_with("https://") => {}
                Some(_) => errors.push(
                    "web.require_https requires server.external_url to use https".to_string(),
                ),
                None if !self.server.tls => errors.push(
                    "web.require_https requires server.external_url or built-in TLS".to_string(),
                ),
                None => {}
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        if self.auth.backend == AuthBackend::None {
            warnings.push(
                "authentication is disabled; only use this on a trusted loopback development host"
                    .to_string(),
            );
        }
        Ok(warnings)
    }

    pub fn subscription_limits(&self) -> (usize, usize) {
        (
            self.subscriptions.max_response_size.bytes(),
            self.subscriptions.max_components,
        )
    }

    pub fn external_url(&self) -> String {
        self.server
            .external_url
            .clone()
            .unwrap_or_else(|| "http://localhost:5232".to_string())
            .trim_end_matches('/')
            .to_string()
    }

    pub fn peer_is_trusted(&self, peer: IpAddr) -> bool {
        self.server
            .trusted_networks()
            .iter()
            .any(|network| network.contains(&peer))
    }
}

fn merge_json(base: &mut serde_json::Value, overlay: serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(base), serde_json::Value::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_json(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn apply_env(value: &mut serde_json::Value) -> Result<(), String> {
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    for (key, val) in std::env::vars() {
        if let Some(rest) = key.strip_prefix("OXICALE__") {
            vars.insert(rest.to_ascii_lowercase(), val);
        }
    }
    for (key, raw) in vars {
        let path: Vec<&str> = key.split("__").filter(|part| !part.is_empty()).collect();
        if path.is_empty() {
            continue;
        }
        set_json_path(value, &path, parse_env_value_for(&path, &raw));
    }
    Ok(())
}

fn set_json_path(value: &mut serde_json::Value, path: &[&str], parsed: serde_json::Value) {
    if !value.is_object() {
        *value = serde_json::Value::Object(Default::default());
    }
    let object = value.as_object_mut().expect("value is an object");
    if path.len() == 1 {
        object.insert(path[0].to_string(), parsed);
        return;
    }
    let child = object
        .entry(path[0].to_string())
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    set_json_path(child, &path[1..], parsed);
}

fn parse_env_value_for(path: &[&str], raw: &str) -> serde_json::Value {
    let bool_field = matches!(
        path.last().copied(),
        Some(
            "enabled"
                | "allow_http"
                | "allow_private_networks"
                | "allow_loopback"
                | "behind_proxy"
                | "allow_insecure_remote"
                | "tls"
                | "fsync"
                | "create_default"
                | "allow_client_creation"
                | "secure_cookies"
                | "require_proxy"
                | "allow_basic_api_auth"
                | "require_https"
                | "log_request_bodies"
                | "skip_broken_components"
                | "allow_http_auth"
        )
    );
    let trimmed = raw.trim();
    if bool_field && (trimmed == "1" || trimmed == "0") {
        return serde_json::Value::Bool(trimmed == "1");
    }
    let parsed = parse_env_value(raw);
    let array_field = matches!(
        path.last().copied(),
        Some("bind" | "trusted_proxies" | "allowed_hosts" | "denied_hosts" | "redact_headers")
    );
    if array_field {
        return match parsed {
            serde_json::Value::Array(values) => serde_json::Value::Array(values),
            other => serde_json::Value::Array(vec![other]),
        };
    }
    parsed
}

fn parse_env_value(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return value;
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "true" => return serde_json::Value::Bool(true),
        "false" => return serde_json::Value::Bool(false),
        _ => {}
    }
    if trimmed.contains(',') {
        return serde_json::Value::Array(
            trimmed
                .split(',')
                .map(|part| serde_json::Value::String(part.trim().to_string()))
                .collect(),
        );
    }
    serde_json::Value::String(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn durations_and_sizes() {
        assert_eq!("30s".parse::<DurationSpec>().unwrap().seconds(), 30);
        assert_eq!("15m".parse::<DurationSpec>().unwrap().seconds(), 900);
        assert_eq!("1h".parse::<DurationSpec>().unwrap().seconds(), 3600);
        assert_eq!("7d".parse::<DurationSpec>().unwrap().seconds(), 604_800);
        assert!("1y".parse::<DurationSpec>().is_err());
        assert_eq!(
            "10MiB".parse::<ByteSize>().unwrap().bytes(),
            10 * 1024 * 1024
        );
        assert_eq!("2M".parse::<ByteSize>().unwrap().bytes(), 2_000_000);
        assert_eq!("1024".parse::<ByteSize>().unwrap().bytes(), 1024);
    }

    #[test]
    fn defaults_are_safe() {
        let config = Config::default();
        assert_eq!(config.auth.backend, AuthBackend::Local);
        assert!(!config.server.tls);
        assert!(!config.subscriptions.allow_http);
        assert!(!config.subscriptions.allow_private_networks);
        assert!(!config.subscriptions.allow_loopback);
        assert_eq!(config.calendar.default_slug, "calendar");
        assert!(config.addressbook.enabled);
        assert!(config.validate().unwrap().is_empty());
    }

    #[test]
    fn parses_partial_toml_with_defaults() {
        let config = Config::from_toml(
            r#"
[server]
bind = ["127.0.0.1:8080"]
base_path = "/dav"

[subscriptions]
allow_http = true
default_interval = "30m"
"#,
        )
        .unwrap();
        assert_eq!(config.server.bind[0], "127.0.0.1:8080");
        assert_eq!(config.server.base_path, "/dav");
        assert!(config.subscriptions.allow_http);
        assert_eq!(config.subscriptions.default_interval.seconds(), 1800);
        assert_eq!(config.feeds.rate_limit_per_minute, 120);
    }

    #[test]
    fn rejects_unknown_keys_and_bad_values() {
        assert!(Config::from_toml("[server]\nunknown_key = 1\n").is_err());
        assert!(Config::from_toml(
            "[subscriptions]\nmin_interval = \"1m\"\ndefault_interval = \"1h\"\n"
        )
        .unwrap()
        .validate()
        .is_err());
        let mut config = Config::default();
        config.server.behind_proxy = true;
        assert!(config.validate().is_err());
        let mut config = Config::default();
        config.auth.backend = AuthBackend::None;
        config.server.bind = vec!["0.0.0.0:5232".to_string()];
        assert!(config.validate().is_err());
        config.server.allow_insecure_remote = true;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn environment_overrides() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("OXICALE__SERVER__BIND", "127.0.0.1:9999");
        std::env::set_var("OXICALE__SUBSCRIPTIONS__ALLOW_HTTP", "true");
        std::env::set_var("OXICALE__FEEDS__RATE_LIMIT_PER_MINUTE", "5");
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[storage]\ndata_dir = \"/tmp/ox\"\n").unwrap();
        let loaded = Config::load(Some(&path)).unwrap();
        assert_eq!(loaded.config.server.bind, vec!["127.0.0.1:9999"]);
        assert!(loaded.config.subscriptions.allow_http);
        assert_eq!(loaded.config.feeds.rate_limit_per_minute, 5);
        std::env::remove_var("OXICALE__SERVER__BIND");
        std::env::remove_var("OXICALE__SUBSCRIPTIONS__ALLOW_HTTP");
        std::env::remove_var("OXICALE__FEEDS__RATE_LIMIT_PER_MINUTE");
    }
}
