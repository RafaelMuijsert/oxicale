use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, PasswordHasher, Version};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ipnet::IpNet;
use oxicale_core::UserName;
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Local,
    TrustedHeader,
    None,
    Deny,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AuthError {
    #[error("invalid credentials")]
    Invalid,
    #[error("too many failed attempts")]
    RateLimited,
    #[error("authentication backend unavailable: {0}")]
    Unavailable(String),
}

#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub backend: Backend,
    pub none_user: String,
    pub realm: String,
    pub password_file: PathBuf,
    pub rate_limit_failures: usize,
    pub rate_limit_window: Duration,
    pub trusted_header: String,
    pub trusted_proxies: Vec<IpNet>,
    pub require_proxy: bool,
    pub behind_proxy: bool,
}

impl AuthConfig {
    pub fn from_config(config: &crate::config::Config) -> Self {
        let backend = match config.auth.backend {
            crate::config::AuthBackend::Local => Backend::Local,
            crate::config::AuthBackend::TrustedHeader => Backend::TrustedHeader,
            crate::config::AuthBackend::None => Backend::None,
            crate::config::AuthBackend::Deny => Backend::Deny,
        };
        Self {
            backend,
            none_user: config.auth.none_user.clone(),
            realm: config.auth.realm.clone(),
            password_file: config.auth.password_file.clone(),
            rate_limit_failures: config.auth.rate_limit_failures,
            rate_limit_window: config.auth.rate_limit_window.std(),
            trusted_header: config.auth.trusted_header.header.clone(),
            trusted_proxies: config.server.trusted_networks(),
            require_proxy: config.auth.trusted_header.require_proxy,
            behind_proxy: config.server.behind_proxy,
        }
    }
}

pub fn argon2() -> Argon2<'static> {
    let params = Params::new(65_536, 3, 4, None).expect("valid Argon2 parameters");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

#[derive(Clone, Debug)]
pub struct PasswordFile {
    path: PathBuf,
    entries: HashMap<String, String>,
    dummy_hash: String,
}

impl PasswordFile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, AuthError> {
        let path = path.as_ref().to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(&path)
                .map_err(|error| AuthError::Unavailable(error.to_string()))?;
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                return Err(AuthError::Unavailable(
                    "password file must not be group- or world-readable".to_string(),
                ));
            }
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|error| AuthError::Unavailable(error.to_string()))?;
        let mut entries = HashMap::new();
        for (line_number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (user, hash) = line.split_once(':').ok_or_else(|| {
                AuthError::Unavailable(format!("invalid password file line {}", line_number + 1))
            })?;
            if user.is_empty() || hash.is_empty() {
                return Err(AuthError::Unavailable(format!(
                    "invalid password file line {}",
                    line_number + 1
                )));
            }
            PasswordHash::new(hash).map_err(|_| {
                AuthError::Unavailable("invalid Argon2 hash in password file".into())
            })?;
            entries.insert(user.to_string(), hash.to_string());
        }
        let salt = SaltString::encode_b64(b"oxicale-dummy-salt").expect("static salt");
        let dummy_hash = argon2()
            .hash_password(b"oxicale-dummy-password", &salt)
            .map_err(|error| AuthError::Unavailable(error.to_string()))?
            .to_string();
        Ok(Self {
            path,
            entries,
            dummy_hash,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn verify(&self, username: &str, password: &str) -> bool {
        let hash = self.entries.get(username).unwrap_or(&self.dummy_hash);
        let known = self.entries.contains_key(username);
        let parsed = match PasswordHash::new(hash) {
            Ok(parsed) => parsed,
            Err(_) => return false,
        };
        let valid = argon2()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
        known && valid
    }
}

pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut rand::thread_rng());
    Ok(argon2()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|error| AuthError::Unavailable(error.to_string()))?
        .to_string())
}

pub fn upsert_password_file(path: &Path, username: &str, password: &str) -> Result<(), AuthError> {
    UserName::parse(username).map_err(|error| AuthError::Unavailable(error.to_string()))?;
    let hash = hash_password(password)?;
    let mut lines: Vec<String> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            if line.split_once(':').map(|(user, _)| user) != Some(username) {
                lines.push(line.to_string());
            }
        }
    }
    lines.push(format!("{username}:{hash}"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| AuthError::Unavailable(error.to_string()))?;
    }
    std::fs::write(path, format!("{}\n", lines.join("\n")))
        .map_err(|error| AuthError::Unavailable(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| AuthError::Unavailable(error.to_string()))?;
    }
    Ok(())
}

pub struct RateLimiter {
    failures: Mutex<HashMap<String, VecDeque<Instant>>>,
    limit: usize,
    window: Duration,
}

impl RateLimiter {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            failures: Mutex::new(HashMap::new()),
            limit,
            window,
        }
    }

    pub fn check(&self, key: &str) -> bool {
        let mut map = self.failures.lock().expect("rate limiter poisoned");
        let now = Instant::now();
        let entry = map.entry(key.to_string()).or_default();
        while entry
            .front()
            .is_some_and(|instant| now.duration_since(*instant) > self.window)
        {
            entry.pop_front();
        }
        entry.len() < self.limit
    }

    pub fn record_failure(&self, key: &str) {
        let mut map = self.failures.lock().expect("rate limiter poisoned");
        let now = Instant::now();
        let entry = map.entry(key.to_string()).or_default();
        while entry
            .front()
            .is_some_and(|instant| now.duration_since(*instant) > self.window)
        {
            entry.pop_front();
        }
        entry.push_back(now);
    }

    pub fn clear(&self, key: &str) {
        let mut map = self.failures.lock().expect("rate limiter poisoned");
        map.remove(key);
    }
}

pub struct AuthService {
    config: AuthConfig,
    limiter: Arc<RateLimiter>,
    password_file: Mutex<Option<(Arc<PasswordFile>, std::time::SystemTime)>>,
}

impl AuthService {
    pub fn new(config: AuthConfig) -> Self {
        let limiter = Arc::new(RateLimiter::new(
            config.rate_limit_failures.max(1),
            config.rate_limit_window,
        ));
        Self {
            config,
            limiter,
            password_file: Mutex::new(None),
        }
    }

    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    pub fn realm(&self) -> &str {
        &self.config.realm
    }

    fn passwords(&self) -> Result<Arc<PasswordFile>, AuthError> {
        let mut guard = self.password_file.lock().expect("password file mutex");
        let modified = std::fs::metadata(&self.config.password_file)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| AuthError::Unavailable(error.to_string()))?;
        if let Some((file, cached_modified)) = guard.as_ref() {
            if *cached_modified == modified {
                return Ok(file.clone());
            }
        }
        let file = Arc::new(PasswordFile::load(&self.config.password_file)?);
        *guard = Some((file.clone(), modified));
        Ok(file)
    }

    pub fn invalidate_password_cache(&self) {
        *self.password_file.lock().expect("password file mutex") = None;
    }

    /// Authenticate a Basic Authorization header value (already decoded is not
    /// allowed; pass the raw header).
    pub fn authenticate_basic(
        &self,
        header: Option<&str>,
        peer: IpAddr,
    ) -> Result<UserName, AuthError> {
        match self.config.backend {
            Backend::None => UserName::parse(self.config.none_user.clone())
                .map_err(|error| AuthError::Unavailable(error.to_string())),
            Backend::Deny => Err(AuthError::Invalid),
            Backend::TrustedHeader => Err(AuthError::Invalid),
            Backend::Local => {
                let (username, password) = parse_basic(header).ok_or(AuthError::Invalid)?;
                let key = format!("{}|{}", username.to_lowercase(), peer);
                if !self.limiter.check(&key) {
                    return Err(AuthError::RateLimited);
                }
                let file = self.passwords()?;
                if file.verify(&username, &password) {
                    self.limiter.clear(&key);
                    UserName::parse(username).map_err(|_| AuthError::Invalid)
                } else {
                    self.limiter.record_failure(&key);
                    Err(AuthError::Invalid)
                }
            }
        }
    }

    pub fn authenticate_trusted_header(
        &self,
        header_value: Option<&str>,
        peer: IpAddr,
    ) -> Result<UserName, AuthError> {
        if self.config.backend != Backend::TrustedHeader {
            return Err(AuthError::Invalid);
        }
        if self.config.require_proxy
            && (!self.config.behind_proxy
                || !self
                    .config
                    .trusted_proxies
                    .iter()
                    .any(|network| network.contains(&peer)))
        {
            return Err(AuthError::Invalid);
        }
        let value = header_value.ok_or(AuthError::Invalid)?;
        UserName::parse(value.trim()).map_err(|_| AuthError::Invalid)
    }

    pub fn fail(&self, key: &str) {
        self.limiter.record_failure(key);
    }

    pub fn allowed(&self, key: &str) -> bool {
        self.limiter.check(key)
    }
}

pub fn parse_basic(header: Option<&str>) -> Option<(String, String)> {
    let header = header?;
    let encoded = header
        .strip_prefix("Basic ")
        .or_else(|| header.strip_prefix("basic "))?;
    let decoded = STANDARD.decode(encoded.trim()).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    Some((username.to_string(), password.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_password_file(dir: &std::path::Path, username: &str, password: &str) -> PathBuf {
        let path = dir.join("users");
        upsert_password_file(&path, username, password).unwrap();
        path
    }

    fn local_config(path: PathBuf) -> AuthConfig {
        AuthConfig {
            backend: Backend::Local,
            none_user: "anonymous".into(),
            realm: "Oxicale".into(),
            password_file: path,
            rate_limit_failures: 3,
            rate_limit_window: Duration::from_secs(60),
            trusted_header: "X-Remote-User".into(),
            trusted_proxies: vec!["127.0.0.1/32".parse().unwrap()],
            require_proxy: true,
            behind_proxy: true,
        }
    }

    fn basic(user: &str, password: &str) -> String {
        format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
    }

    #[test]
    fn password_roundtrip_and_permissions() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_password_file(dir.path(), "alice", "correct horse");
        let file = PasswordFile::load(&path).unwrap();
        assert!(file.verify("alice", "correct horse"));
        assert!(!file.verify("alice", "wrong"));
        assert!(!file.verify("bob", "correct horse"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn rejects_world_readable_password_file() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::TempDir::new().unwrap();
            let path = write_password_file(dir.path(), "alice", "pw");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(PasswordFile::load(&path).is_err());
        }
    }

    #[test]
    fn local_authentication_and_rate_limit() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = write_password_file(dir.path(), "alice", "secret");
        let auth = AuthService::new(local_config(path));
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            auth.authenticate_basic(Some(&basic("alice", "secret")), peer)
                .unwrap()
                .as_str(),
            "alice"
        );
        assert_eq!(
            auth.authenticate_basic(Some(&basic("alice", "bad")), peer),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            auth.authenticate_basic(Some(&basic("alice", "bad")), peer),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            auth.authenticate_basic(Some(&basic("alice", "bad")), peer),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            auth.authenticate_basic(Some(&basic("alice", "secret")), peer),
            Err(AuthError::RateLimited)
        );
    }

    #[test]
    fn trusted_header_requires_trusted_peer() {
        let config = AuthConfig {
            backend: Backend::TrustedHeader,
            none_user: "anonymous".into(),
            realm: "Oxicale".into(),
            password_file: PathBuf::new(),
            rate_limit_failures: 10,
            rate_limit_window: Duration::from_secs(60),
            trusted_header: "X-Remote-User".into(),
            trusted_proxies: vec!["10.0.0.0/8".parse().unwrap()],
            require_proxy: true,
            behind_proxy: true,
        };
        let auth = AuthService::new(config);
        assert_eq!(
            auth.authenticate_trusted_header(Some("alice"), "10.1.2.3".parse().unwrap())
                .unwrap()
                .as_str(),
            "alice"
        );
        assert_eq!(
            auth.authenticate_trusted_header(Some("alice"), "192.0.2.1".parse().unwrap()),
            Err(AuthError::Invalid)
        );
        assert_eq!(
            auth.authenticate_trusted_header(Some("bad/user"), "10.1.2.3".parse().unwrap()),
            Err(AuthError::Invalid)
        );
    }

    #[test]
    fn none_and_deny_backends() {
        let mut config = local_config(PathBuf::new());
        config.backend = Backend::None;
        config.none_user = "anonymous".into();
        let auth = AuthService::new(config.clone());
        assert_eq!(
            auth.authenticate_basic(None, "127.0.0.1".parse().unwrap())
                .unwrap()
                .as_str(),
            "anonymous"
        );
        config.backend = Backend::Deny;
        let auth = AuthService::new(config);
        assert_eq!(
            auth.authenticate_basic(None, "127.0.0.1".parse().unwrap()),
            Err(AuthError::Invalid)
        );
    }

    #[test]
    fn rate_limiter_window() {
        let limiter = RateLimiter::new(2, Duration::from_millis(10));
        assert!(limiter.check("a"));
        limiter.record_failure("a");
        limiter.record_failure("a");
        assert!(!limiter.check("a"));
        std::thread::sleep(Duration::from_millis(20));
        assert!(limiter.check("a"));
        assert!(limiter.check("b"));
        let _ = &ENV_LOCK;
    }
}
