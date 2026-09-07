//! Layered ntlmrain configuration.

use std::{
    collections::BTreeMap,
    env, fmt, fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_REMOTE_URL: &str = "https://lookup.ntlmrain.com";

pub const ENV_REMOTE_URL: &str = "NTLMRAIN_REMOTE_URL";
pub const ENV_REMOTE_USERNAME: &str = "NTLMRAIN_REMOTE_USERNAME";
pub const ENV_REMOTE_PASSWORD: &str = "NTLMRAIN_REMOTE_PASSWORD";
pub const ENV_REMOTE_AUTH: &str = "NTLMRAIN_REMOTE_AUTH";

#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub remote: RemoteConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            remote: RemoteConfig {
                url: DEFAULT_REMOTE_URL.to_owned(),
                username: None,
                password: None,
            },
        }
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("remote", &self.remote)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RemoteConfig {
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl RemoteConfig {
    pub fn auth(&self) -> Option<(&str, &str)> {
        self.username.as_deref().zip(self.password.as_deref())
    }
}

impl fmt::Debug for RemoteConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteConfig")
            .field("url", &redact_url(&self.url))
            .field("username", &self.username.as_deref().map(|_| "<redacted>"))
            .field("password", &self.password.as_deref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Values collected by the CLI. `remote_auth = Some(false)` corresponds to a
/// `--no-remote-auth` flag; other optional values are applied when present.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigOverrides {
    pub remote_url: Option<String>,
    pub remote_username: Option<String>,
    pub remote_password: Option<String>,
    pub remote_auth: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    #[serde(default)]
    pub remote: RemoteFileConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteFileConfig {
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// When false, clear both credentials after applying this layer.
    pub auth: Option<bool>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot determine the platform configuration directory")]
    NoConfigDirectory,
    #[error("failed to read configuration {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("failed to parse configuration {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{ENV_REMOTE_AUTH} must be true/false, yes/no, on/off, or 1/0")]
    InvalidAuthEnvironment,
    #[error("remote URL cannot be empty")]
    EmptyRemoteUrl,
    #[error("remote authentication requires both a non-empty username and password")]
    IncompleteCredentials,
}

impl Config {
    /// Load platform config (if present), environment variables, then CLI
    /// overrides. An explicitly supplied path must exist.
    pub fn load(
        config_path: Option<&Path>,
        overrides: &ConfigOverrides,
    ) -> Result<Self, ConfigError> {
        let path = match config_path {
            Some(path) => Some((path.to_path_buf(), true)),
            None => Some((default_config_path()?, false)),
        };
        let file = match path {
            Some((path, required)) => match fs::read_to_string(&path) {
                Ok(contents) => Some(parse_config_file(&path, &contents)?),
                Err(error) if !required && error.kind() == io::ErrorKind::NotFound => None,
                Err(source) => return Err(ConfigError::Read { path, source }),
            },
            None => None,
        };
        Self::resolve(file.as_ref(), |name| env::var(name).ok(), overrides)
    }

    /// Resolve layers with an injected environment source. This is useful for
    /// deterministic embedding and tests without mutating process-global env.
    pub fn resolve<F>(
        file: Option<&ConfigFile>,
        mut environment: F,
        overrides: &ConfigOverrides,
    ) -> Result<Self, ConfigError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let mut config = Self::default();
        if let Some(file) = file {
            apply_file(&mut config, &file.remote);
        }

        if let Some(url) = environment(ENV_REMOTE_URL) {
            config.remote.url = url;
        }
        if let Some(username) = environment(ENV_REMOTE_USERNAME) {
            config.remote.username = Some(username);
        }
        if let Some(password) = environment(ENV_REMOTE_PASSWORD) {
            config.remote.password = Some(password);
        }
        if let Some(auth) = environment(ENV_REMOTE_AUTH)
            && !parse_bool(&auth).ok_or(ConfigError::InvalidAuthEnvironment)?
        {
            clear_auth(&mut config);
        }

        if let Some(url) = &overrides.remote_url {
            config.remote.url = url.clone();
        }
        if let Some(username) = &overrides.remote_username {
            config.remote.username = Some(username.clone());
        }
        if let Some(password) = &overrides.remote_password {
            config.remote.password = Some(password.clone());
        }
        if overrides.remote_auth == Some(false) {
            clear_auth(&mut config);
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.remote.url.trim().is_empty() {
            return Err(ConfigError::EmptyRemoteUrl);
        }
        match (&self.remote.username, &self.remote.password) {
            (None, None) => Ok(()),
            (Some(username), Some(password)) if !username.is_empty() && !password.is_empty() => {
                Ok(())
            }
            _ => Err(ConfigError::IncompleteCredentials),
        }
    }
}

pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    crate::platform::project_dirs()
        .map(|directories| directories.config_dir().join("config.toml"))
        .ok_or(ConfigError::NoConfigDirectory)
}

pub fn parse_config_file(path: &Path, contents: &str) -> Result<ConfigFile, ConfigError> {
    toml::from_str(contents).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn apply_file(config: &mut Config, file: &RemoteFileConfig) {
    if let Some(url) = &file.url {
        config.remote.url = url.clone();
    }
    if let Some(username) = &file.username {
        config.remote.username = Some(username.clone());
    }
    if let Some(password) = &file.password {
        config.remote.password = Some(password.clone());
    }
    if file.auth == Some(false) {
        clear_auth(config);
    }
}

fn clear_auth(config: &mut Config) {
    config.remote.username = None;
    config.remote.password = None;
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://<redacted>@{host}{tail}"),
        None => url.to_owned(),
    }
}

/// Convenience environment source for callers that already collected a map.
pub fn map_environment<'a>(
    values: &'a BTreeMap<String, String>,
) -> impl FnMut(&str) -> Option<String> + 'a {
    |name| values.get(name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_defaults_use_https_without_authentication() {
        let config = Config::default();
        assert_eq!(config.remote.url, "https://lookup.ntlmrain.com");
        assert_eq!(config.remote.auth(), None);
    }

    #[test]
    fn precedence_is_cli_over_env_over_file_over_defaults() {
        let file: ConfigFile = toml::from_str(
            r#"
                [remote]
                url = "http://from-file"
                username = "file-user"
                password = "file-pass"
            "#,
        )
        .unwrap();
        let environment = BTreeMap::from([
            (ENV_REMOTE_URL.to_owned(), "http://from-env".to_owned()),
            (ENV_REMOTE_USERNAME.to_owned(), "env-user".to_owned()),
            (ENV_REMOTE_PASSWORD.to_owned(), "env-pass".to_owned()),
        ]);
        let overrides = ConfigOverrides {
            remote_url: Some("http://from-cli".into()),
            remote_username: Some("cli-user".into()),
            remote_password: Some("cli-pass".into()),
            remote_auth: None,
        };
        let config =
            Config::resolve(Some(&file), map_environment(&environment), &overrides).unwrap();
        assert_eq!(config.remote.url, "http://from-cli");
        assert_eq!(config.remote.auth(), Some(("cli-user", "cli-pass")));
    }

    #[test]
    fn every_layer_can_disable_auth() {
        let file: ConfigFile = toml::from_str("[remote]\nauth = false").unwrap();
        let config = Config::resolve(Some(&file), |_| None, &ConfigOverrides::default()).unwrap();
        assert_eq!(config.remote.auth(), None);

        let environment = BTreeMap::from([(ENV_REMOTE_AUTH.into(), "off".into())]);
        let config = Config::resolve(
            None,
            map_environment(&environment),
            &ConfigOverrides::default(),
        )
        .unwrap();
        assert_eq!(config.remote.auth(), None);

        let config = Config::resolve(
            None,
            |_| None,
            &ConfigOverrides {
                remote_auth: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(config.remote.auth(), None);
    }

    #[test]
    fn rejects_unknown_keys_invalid_auth_and_incomplete_credentials() {
        assert!(parse_config_file(Path::new("test.toml"), "unknown = 1").is_err());
        let environment = BTreeMap::from([(ENV_REMOTE_AUTH.into(), "perhaps".into())]);
        assert!(matches!(
            Config::resolve(
                None,
                map_environment(&environment),
                &ConfigOverrides::default()
            ),
            Err(ConfigError::InvalidAuthEnvironment)
        ));
        let overrides = ConfigOverrides {
            remote_username: Some(String::new()),
            ..Default::default()
        };
        assert!(matches!(
            Config::resolve(None, |_| None, &overrides),
            Err(ConfigError::IncompleteCredentials)
        ));
    }

    #[test]
    fn explicit_missing_config_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.toml");
        assert!(matches!(
            Config::load(Some(&missing), &ConfigOverrides::default()),
            Err(ConfigError::Read { .. })
        ));
    }
}
