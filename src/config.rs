use crate::error::AppError;
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub r2: R2Config,
    pub local: LocalConfig,
    pub reaper: ReaperConfig,
    pub identity: IdentityConfig,
    #[serde(default)]
    pub new: Option<NewConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewConfig {
    pub default_template: Option<String>,
    #[serde(default)]
    pub trim_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct R2Config {
    pub account_id: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalConfig {
    pub working_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReaperConfig {
    pub binary_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityConfig {
    pub user: String,
    pub machine: String,
}

/// Returns the canonical config file path: `~/.config/whirlwind/config.toml`.
pub fn config_path() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("whirlwind")
        .join("config.toml")
}

impl Config {
    /// Load config from `~/.config/whirlwind/config.toml`.
    ///
    /// Returns `AppError::ConfigMissing` if the file does not exist,
    /// `AppError::ConfigInvalid` if the TOML cannot be parsed.
    pub fn load() -> Result<Config, AppError> {
        let path = config_path();
        Self::load_from_path(&path)
    }

    /// Load config from an arbitrary path.
    ///
    /// Used by tests to load from a path other than the fixed config location.
    /// Returns `AppError::ConfigMissing` if the file does not exist,
    /// `AppError::ConfigInvalid` if the TOML cannot be parsed.
    pub fn load_from_path(path: &Path) -> Result<Config, AppError> {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AppError::ConfigMissing
            } else {
                AppError::ConfigInvalid(format!("could not read {}: {}", path.display(), e))
            }
        })?;

        toml::from_str(&contents).map_err(|e| {
            AppError::ConfigInvalid(format!("TOML parse error in {}: {}", path.display(), e))
        })
    }

    /// Serialize config to `~/.config/whirlwind/config.toml`.
    ///
    /// Creates parent directories if they do not exist. On Unix, sets file
    /// permissions to `0o600` (owner read/write only).
    pub fn save(&self) -> Result<(), AppError> {
        let path = config_path();

        // Create parent directory if missing.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::ConfigInvalid(format!(
                    "could not create config directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let toml_str = toml::to_string_pretty(self)
            .map_err(|e| AppError::ConfigInvalid(format!("failed to serialize config: {}", e)))?;

        std::fs::write(&path, &toml_str).map_err(|e| {
            AppError::ConfigInvalid(format!(
                "could not write config to {}: {}",
                path.display(),
                e
            ))
        })?;

        // Restrict permissions to owner read/write on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&path, permissions).map_err(|e| {
                AppError::ConfigInvalid(format!(
                    "could not set permissions on {}: {}",
                    path.display(),
                    e
                ))
            })?;
        }

        Ok(())
    }

    /// Validate that all required config fields are non-empty.
    ///
    /// Does not check whether paths exist on disk — that is each command's
    /// responsibility at runtime.
    pub fn validate(&self) -> Result<(), AppError> {
        if self.r2.account_id.is_empty() {
            return Err(AppError::ConfigInvalid(
                "r2.account_id is empty".to_string(),
            ));
        }
        if self.r2.access_key_id.is_empty() {
            return Err(AppError::ConfigInvalid(
                "r2.access_key_id is empty".to_string(),
            ));
        }
        if self.r2.secret_access_key.is_empty() {
            return Err(AppError::ConfigInvalid(
                "r2.secret_access_key is empty".to_string(),
            ));
        }
        if self.r2.bucket.is_empty() {
            return Err(AppError::ConfigInvalid("r2.bucket is empty".to_string()));
        }
        if self.local.working_dir == Path::new("") {
            return Err(AppError::ConfigInvalid(
                "local.working_dir is empty".to_string(),
            ));
        }
        // reaper.binary_path existence is checked by the session command, not here.
        if self.identity.user.is_empty() {
            return Err(AppError::ConfigInvalid(
                "identity.user is empty".to_string(),
            ));
        }
        if self.identity.machine.is_empty() {
            return Err(AppError::ConfigInvalid(
                "identity.machine is empty".to_string(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fully-populated valid Config for use in tests.
    fn sample_config() -> Config {
        Config {
            r2: R2Config {
                account_id: "abc123def456".to_string(),
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
                bucket: "podcast-projects".to_string(),
            },
            local: LocalConfig {
                working_dir: PathBuf::from("/Users/alice/podcast"),
            },
            reaper: ReaperConfig {
                binary_path: PathBuf::from("/Applications/REAPER.app/Contents/MacOS/REAPER"),
            },
            identity: IdentityConfig {
                user: "alice".to_string(),
                machine: "alice-macbook".to_string(),
            },
            new: None,
        }
    }

    #[test]
    fn config_round_trips_through_toml() {
        let original = sample_config();
        let toml_str = toml::to_string_pretty(&original).expect("serialize failed");
        let restored: Config = toml::from_str(&toml_str).expect("deserialize failed");

        assert_eq!(restored.r2.account_id, original.r2.account_id);
        assert_eq!(restored.r2.access_key_id, original.r2.access_key_id);
        assert_eq!(restored.r2.secret_access_key, original.r2.secret_access_key);
        assert_eq!(restored.r2.bucket, original.r2.bucket);
        assert_eq!(restored.local.working_dir, original.local.working_dir);
        assert_eq!(restored.reaper.binary_path, original.reaper.binary_path);
        assert_eq!(restored.identity.user, original.identity.user);
        assert_eq!(restored.identity.machine, original.identity.machine);
    }

    #[test]
    fn config_without_new_section_deserializes() {
        let toml_str = r#"
[r2]
account_id = "abc123"
access_key_id = "KEY"
secret_access_key = "SECRET"
bucket = "my-bucket"

[local]
working_dir = "/Users/alice/podcast"

[reaper]
binary_path = "/Applications/REAPER.app/Contents/MacOS/REAPER"

[identity]
user = "alice"
machine = "alice-macbook"
"#;
        let config: Config = toml::from_str(toml_str).expect("deserialize failed");
        assert!(
            config.new.is_none(),
            "expected new to be None when section is absent"
        );
    }

    #[test]
    fn config_with_new_section_round_trips() {
        let toml_str = r#"
[r2]
account_id = "abc123"
access_key_id = "KEY"
secret_access_key = "SECRET"
bucket = "my-bucket"

[local]
working_dir = "/Users/alice/podcast"

[reaper]
binary_path = "/Applications/REAPER.app/Contents/MacOS/REAPER"

[identity]
user = "alice"
machine = "alice-macbook"

[new]
default_template = "my-template"
trim_seconds = 2.5
"#;
        let config: Config = toml::from_str(toml_str).expect("deserialize failed");
        let new_cfg = config
            .new
            .as_ref()
            .expect("expected new section to be present");
        assert_eq!(new_cfg.default_template.as_deref(), Some("my-template"));
        assert_eq!(new_cfg.trim_seconds, 2.5);

        // Round-trip through serialization.
        let serialized = toml::to_string_pretty(&config).expect("serialize failed");
        let restored: Config = toml::from_str(&serialized).expect("re-deserialize failed");
        let restored_new = restored
            .new
            .as_ref()
            .expect("expected new section after round-trip");
        assert_eq!(
            restored_new.default_template.as_deref(),
            Some("my-template")
        );
        assert_eq!(restored_new.trim_seconds, 2.5);
    }

    #[test]
    fn trim_seconds_defaults_to_zero_when_field_absent() {
        let toml_str = r#"
[r2]
account_id = "abc123"
access_key_id = "KEY"
secret_access_key = "SECRET"
bucket = "my-bucket"

[local]
working_dir = "/Users/alice/podcast"

[reaper]
binary_path = "/Applications/REAPER.app/Contents/MacOS/REAPER"

[identity]
user = "alice"
machine = "alice-macbook"

[new]
default_template = "default"
"#;
        let config: Config = toml::from_str(toml_str).expect("deserialize failed");
        let new_cfg = config.new.as_ref().expect("expected new section");
        assert_eq!(
            new_cfg.trim_seconds, 0.0,
            "expected trim_seconds to default to 0.0"
        );
    }

    #[test]
    fn config_path_contains_whirlwind() {
        let path = config_path();
        assert!(
            path.to_string_lossy().contains("whirlwind"),
            "expected 'whirlwind' in config path, got: {}",
            path.display()
        );
    }

    #[test]
    fn validate_fails_on_empty_user() {
        let mut config = sample_config();
        config.identity.user = String::new();
        let result = config.validate();
        assert!(
            result.is_err(),
            "expected validate() to fail with empty identity.user"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("identity.user"),
            "expected error message to mention 'identity.user', got: {msg}"
        );
    }

    #[test]
    fn validate_fails_on_empty_account_id() {
        let mut config = sample_config();
        config.r2.account_id = String::new();
        let result = config.validate();
        assert!(
            result.is_err(),
            "expected validate() to fail with empty r2.account_id"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("r2.account_id"),
            "expected error message to mention 'r2.account_id', got: {msg}"
        );
    }

    #[test]
    fn load_missing_file_returns_config_missing() {
        let result = Config::load_from_path(&PathBuf::from("/nonexistent/path/config.toml"));
        assert!(
            result.is_err(),
            "expected an error loading from a nonexistent path"
        );
        let err = result.unwrap_err();
        assert!(
            matches!(err, crate::error::AppError::ConfigMissing),
            "expected ConfigMissing, got: {:?}",
            err
        );
    }
}
