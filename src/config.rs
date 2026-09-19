//! `~/.forge/config.toml`: defaults, unknown-section preservation, migration.
//!
//! Writes are mutex-serialized by the caller convention (single-owner
//! `AppState` performs them on one thread) and go through atomic files.
//! Unknown TOML sections are preserved across load/save so newer or
//! hand-written configuration is never clobbered. There is deliberately no
//! memory section: the memory feature is out of scope for this bring-up.

use std::fmt;
use std::path::Path;

use crate::{branding, fs_atomic};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    Off,
    SafeOnly,
    AiAssisted,
    Yolo,
}

impl PermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PermissionMode::Off => "off",
            PermissionMode::SafeOnly => "safe-only",
            PermissionMode::AiAssisted => "ai-assisted",
            PermissionMode::Yolo => "yolo",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(PermissionMode::Off),
            "safe-only" => Some(PermissionMode::SafeOnly),
            "ai-assisted" => Some(PermissionMode::AiAssisted),
            "yolo" => Some(PermissionMode::Yolo),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PermissionConfig {
    pub mode: PermissionMode,
    pub allow: Vec<String>,
    pub block: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AiConfig {
    pub provider: String,
    pub model: String,
    pub threshold: f64,
    pub timeout_seconds: u64,
    pub auto_refresh: bool,
    pub key_env: String,
}

/// One operator-registered external bot client (`[[bots]]`): a routing
/// name, group grants, reserved tool grants, and the path of the file
/// holding its credential. Secrets never live in this document; the
/// broker reads the token file and fails closed when it is missing.
/// Grants are parsed and stored but not enforced in this release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotRegistration {
    pub name: String,
    pub token_file: String,
    pub groups: Vec<String>,
    pub grants: Vec<String>,
}

/// Telegram mobile transport (`[telegram]`): disabled and empty by
/// default. The bot token itself never lives in this document; `token_file`
/// points at a `0600` file the poller reads, mirroring `[[bots]]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub token_file: String,
    pub allowed_user_ids: Vec<i64>,
    /// Telegram chat receiving `message_user` forwards. `0` (the default)
    /// disables forwarding while the in-app badge still records.
    pub notify_chat_id: i64,
    pub poll_seconds: u64,
    pub backoff_min_seconds: u64,
    pub backoff_max_seconds: u64,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        TelegramConfig {
            enabled: false,
            token_file: String::new(),
            allowed_user_ids: Vec::new(),
            notify_chat_id: 0,
            poll_seconds: 20,
            backoff_min_seconds: 60,
            backoff_max_seconds: 900,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub theme: String,
    pub prefix: String,
    pub permission: PermissionConfig,
    pub ai: AiConfig,
    pub telegram: TelegramConfig,
    pub claudling_enabled: bool,
    pub telemetry_enabled: bool,
    pub clikan_enabled: bool,
    /// Rounded pill session tabs (needs a Nerd Font); plain labels off.
    pub pills_enabled: bool,
    pub messaging_idle_minutes: u64,
    pub bots: Vec<BotRegistration>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "default".to_string(),
            prefix: "ctrl-b".to_string(),
            permission: PermissionConfig {
                mode: PermissionMode::Yolo,
                allow: Vec::new(),
                block: Vec::new(),
            },
            ai: AiConfig {
                provider: "openai".to_string(),
                model: "gpt-4.1-mini".to_string(),
                threshold: 0.6,
                timeout_seconds: 10,
                auto_refresh: true,
                key_env: "APE_API_KEY".to_string(),
            },
            telegram: TelegramConfig::default(),
            claudling_enabled: false,
            telemetry_enabled: true,
            clikan_enabled: false,
            pills_enabled: true,
            messaging_idle_minutes: 10,
            bots: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Io(String),
    Parse(String),
    BadValue(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "config I/O: {e}"),
            ConfigError::Parse(e) => write!(f, "config parse: {e}"),
            ConfigError::BadValue(e) => write!(f, "config value: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Loaded configuration plus the raw document it came from, so [`save`]
/// preserves sections this version does not understand.
pub struct LoadedConfig {
    pub config: Config,
    raw: toml::Table,
}

impl LoadedConfig {
    /// Load `path`, or defaults when the file does not exist. A present but
    /// unparsable file is an error, never silent defaults.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(ConfigError::Io(e.to_string())),
        };
        let raw: toml::Table = if text.trim().is_empty() {
            toml::Table::new()
        } else {
            toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?
        };
        let config = extract(&raw)?;
        Ok(LoadedConfig { config, raw })
    }

    /// Save back through the raw document: known keys are replaced, unknown
    /// sections pass through untouched.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let mut raw = self.raw.clone();
        merge(&mut raw, &self.config);
        let text = toml::to_string(&raw).map_err(|e| ConfigError::Parse(e.to_string()))?;
        fs_atomic::write_atomic(path, text.as_bytes())
            .map_err(|e| ConfigError::Io(e.to_string()))
    }

    /// Convenience: load `~/.forge/config.toml` under `home`.
    pub fn load_home(home: &Path) -> Result<Self, ConfigError> {
        Self::load(&branding::config_file(home))
    }

    /// Convenience: save to `~/.forge/config.toml` under `home`.
    pub fn save_home(&self, home: &Path) -> Result<(), ConfigError> {
        Self::save(self, &branding::config_file(home))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Migration {
    AlreadyCurrent,
    Migrated,
    LegacyAbsent,
    Collision,
}

/// Move legacy `~/.ccpp` to `~/.forge` when appropriate. Never clobbers an
/// existing `~/.forge`: that is a [`Migration::Collision`] and both
/// directories are left intact.
pub fn migrate_legacy(home: &Path) -> Result<Migration, ConfigError> {
    let current = branding::config_dir(home);
    let legacy = branding::legacy_config_dir(home);
    match (current.exists(), legacy.exists()) {
        (true, true) => Ok(Migration::Collision),
        (true, false) => Ok(Migration::AlreadyCurrent),
        (false, true) => {
            std::fs::rename(&legacy, &current).map_err(|e| ConfigError::Io(e.to_string()))?;
            Ok(Migration::Migrated)
        }
        (false, false) => Ok(Migration::LegacyAbsent),
    }
}

fn subtable<'a>(raw: &'a toml::Table, key: &str) -> Option<&'a toml::Table> {
    raw.get(key)?.as_table()
}

fn str_val(raw: &toml::Table, key: &str, default: &str) -> String {
    raw.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_string()
}

fn bool_val(raw: &toml::Table, key: &str, default: bool) -> bool {
    raw.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

fn u64_val(raw: &toml::Table, key: &str, default: u64) -> Result<u64, ConfigError> {
    match raw.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_integer()
            .filter(|&n| n >= 0)
            .map(|n| n as u64)
            .ok_or_else(|| ConfigError::BadValue(format!("[{key}] must be a non-negative integer"))),
    }
}

fn float_val(raw: &toml::Table, key: &str, default: f64) -> Result<f64, ConfigError> {
    match raw.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_float()
            .or_else(|| v.as_integer().map(|n| n as f64))
            .ok_or_else(|| ConfigError::BadValue(format!("[{key}] must be a number"))),
    }
}

fn i64_list(raw: &toml::Table, key: &str, section: &str) -> Result<Vec<i64>, ConfigError> {
    match raw.get(key) {
        None => Ok(Vec::new()),
        Some(v) => v
            .as_array()
            .ok_or_else(|| ConfigError::BadValue(format!("[{section}.{key}] must be an integer list")))?
            .iter()
            .map(|e| {
                e.as_integer()
                    .ok_or_else(|| ConfigError::BadValue(format!("[{section}.{key}] must be an integer list")))
            })
            .collect(),
    }
}

fn str_list(raw: &toml::Table, key: &str, section: &str) -> Result<Vec<String>, ConfigError> {
    match raw.get(key) {
        None => Ok(Vec::new()),
        Some(v) => v
            .as_array()
            .ok_or_else(|| ConfigError::BadValue(format!("[{section}.{key}] must be a string list")))?
            .iter()
            .map(|e| {
                e.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ConfigError::BadValue(format!("[{section}.{key}] must be a string list")))
            })
            .collect(),
    }
}

fn extract(raw: &toml::Table) -> Result<Config, ConfigError> {
    let empty = toml::Table::new();
    let perm = raw.get("permission").and_then(|v| v.as_table()).unwrap_or(&empty);
    let mode = match perm.get("mode").and_then(|v| v.as_str()) {
        None => PermissionMode::Yolo,
        Some(s) => PermissionMode::parse(s).ok_or_else(|| {
            ConfigError::BadValue(
                "[permission.mode] must be off, safe-only, ai-assisted or yolo".to_string(),
            )
        })?,
    };
    let ai = raw.get("ai").and_then(|v| v.as_table()).unwrap_or(&empty);
    let claudling = subtable(raw, "claudling");
    let telemetry = subtable(raw, "telemetry");
    let clikan = subtable(raw, "clikan");
    let pills = subtable(raw, "pills");
    let messaging = subtable(raw, "messaging");
    let telegram = subtable(raw, "telegram");
    let telegram_cfg = TelegramConfig {
        enabled: telegram.map(|t| bool_val(t, "enabled", false)).unwrap_or(false),
        token_file: telegram
            .map(|t| str_val(t, "token_file", ""))
            .unwrap_or_default(),
        allowed_user_ids: telegram
            .map(|t| i64_list(t, "allowed_user_ids", "telegram"))
            .transpose()?
            .unwrap_or_default(),
        notify_chat_id: match telegram.and_then(|t| t.get("notify_chat_id")) {
            None => 0,
            Some(v) => v.as_integer().ok_or_else(|| {
                ConfigError::BadValue("[telegram.notify_chat_id] must be an integer".to_string())
            })?,
        },
        poll_seconds: telegram
            .map(|t| u64_val(t, "poll_seconds", 20))
            .transpose()
            .map_err(|_| {
                ConfigError::BadValue("[telegram.poll_seconds] must be a non-negative integer".to_string())
            })?
            .unwrap_or(20),
        backoff_min_seconds: telegram
            .map(|t| u64_val(t, "backoff_min_seconds", 60))
            .transpose()
            .map_err(|_| {
                ConfigError::BadValue(
                    "[telegram.backoff_min_seconds] must be a non-negative integer".to_string(),
                )
            })?
            .unwrap_or(60),
        backoff_max_seconds: telegram
            .map(|t| u64_val(t, "backoff_max_seconds", 900))
            .transpose()
            .map_err(|_| {
                ConfigError::BadValue(
                    "[telegram.backoff_max_seconds] must be a non-negative integer".to_string(),
                )
            })?
            .unwrap_or(900),
    };
    if telegram_cfg.poll_seconds == 0 {
        return Err(ConfigError::BadValue(
            "[telegram.poll_seconds] must be at least 1".to_string(),
        ));
    }
    if telegram_cfg.backoff_min_seconds > telegram_cfg.backoff_max_seconds {
        return Err(ConfigError::BadValue(
            "[telegram.backoff_min_seconds] must not exceed [telegram.backoff_max_seconds]".to_string(),
        ));
    }
    Ok(Config {
        theme: str_val(raw, "theme", "default"),
        prefix: str_val(raw, "prefix", "ctrl-b"),
        permission: PermissionConfig {
            mode,
            allow: str_list(perm, "allow", "permission")?,
            block: str_list(perm, "block", "permission")?,
        },
        ai: AiConfig {
            provider: str_val(ai, "provider", "openai"),
            model: str_val(ai, "model", "gpt-4.1-mini"),
            threshold: float_val(ai, "threshold", 0.6)?,
            timeout_seconds: u64_val(ai, "timeout_seconds", 10)
                .map_err(|_| ConfigError::BadValue("[ai.timeout_seconds] must be a non-negative integer".to_string()))?,
            auto_refresh: bool_val(ai, "auto_refresh", true),
            key_env: str_val(ai, "key_env", "APE_API_KEY"),
        },
        telegram: telegram_cfg,
        claudling_enabled: claudling.map(|t| bool_val(t, "enabled", false)).unwrap_or(false),
        telemetry_enabled: telemetry.map(|t| bool_val(t, "enabled", true)).unwrap_or(true),
        clikan_enabled: clikan.map(|t| bool_val(t, "enabled", false)).unwrap_or(false),
        pills_enabled: pills.map(|t| bool_val(t, "enabled", true)).unwrap_or(true),
        messaging_idle_minutes: messaging
            .map(|t| u64_val(t, "idle_timeout_minutes", 10))
            .transpose()
            .map_err(|_| {
                ConfigError::BadValue(
                    "[messaging.idle_timeout_minutes] must be a non-negative integer".to_string(),
                )
            })?
            .unwrap_or(10),
        bots: if raw.contains_key("bots") {
            extract_bots(raw)?
        } else {
            Vec::new()
        },
    })
}

/// Parse the operator `[[bots]]` table into registrations. Name and
/// token file are required; groups default empty (registration still
/// refuses groupless clients) and grants default to messaging-only.
fn extract_bots(raw: &toml::Table) -> Result<Vec<BotRegistration>, ConfigError> {
    let items = raw
        .get("bots")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ConfigError::BadValue("[[bots]] must be an array of tables".to_string()))?;
    let mut out = Vec::new();
    for item in items {
        let table = item
            .as_table()
            .ok_or_else(|| ConfigError::BadValue("[[bots]] entries must be tables".to_string()))?;
        let name = table
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ConfigError::BadValue("[[bots]] entries need a name".to_string()))?;
        let token_file = table
            .get("token_file")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ConfigError::BadValue("[[bots]] entries need a token_file".to_string())
            })?;
        out.push(BotRegistration {
            name: name.to_string(),
            token_file: token_file.to_string(),
            groups: str_list(table, "groups", "[[bots]]")?,
            grants: str_list(table, "grants", "[[bots]]")?,
        });
    }
    Ok(out)
}

fn merge(raw: &mut toml::Table, config: &Config) {
    raw.insert("theme".to_string(), toml::Value::String(config.theme.clone()));
    raw.insert("prefix".to_string(), toml::Value::String(config.prefix.clone()));
    let mut perm = toml::Table::new();
    perm.insert(
        "mode".to_string(),
        toml::Value::String(config.permission.mode.as_str().to_string()),
    );
    perm.insert(
        "allow".to_string(),
        toml::Value::Array(config.permission.allow.iter().map(|s| toml::Value::String(s.clone())).collect()),
    );
    perm.insert(
        "block".to_string(),
        toml::Value::Array(config.permission.block.iter().map(|s| toml::Value::String(s.clone())).collect()),
    );
    raw.insert("permission".to_string(), toml::Value::Table(perm));
    let mut ai = toml::Table::new();
    ai.insert("provider".to_string(), toml::Value::String(config.ai.provider.clone()));
    ai.insert("model".to_string(), toml::Value::String(config.ai.model.clone()));
    ai.insert("threshold".to_string(), toml::Value::Float(config.ai.threshold));
    ai.insert(
        "timeout_seconds".to_string(),
        toml::Value::Integer(config.ai.timeout_seconds as i64),
    );
    ai.insert("auto_refresh".to_string(), toml::Value::Boolean(config.ai.auto_refresh));
    ai.insert("key_env".to_string(), toml::Value::String(config.ai.key_env.clone()));
    raw.insert("ai".to_string(), toml::Value::Table(ai));
    for (section, enabled) in [
        ("claudling", config.claudling_enabled),
        ("telemetry", config.telemetry_enabled),
        ("clikan", config.clikan_enabled),
        ("pills", config.pills_enabled),
    ] {
        let mut t = toml::Table::new();
        t.insert("enabled".to_string(), toml::Value::Boolean(enabled));
        raw.insert(section.to_string(), toml::Value::Table(t));
    }
    let mut messaging = toml::Table::new();
    messaging.insert(
        "idle_timeout_minutes".to_string(),
        toml::Value::Integer(config.messaging_idle_minutes as i64),
    );
    raw.insert("messaging".to_string(), toml::Value::Table(messaging));
    let mut telegram = toml::Table::new();
    telegram.insert(
        "enabled".to_string(),
        toml::Value::Boolean(config.telegram.enabled),
    );
    telegram.insert(
        "token_file".to_string(),
        toml::Value::String(config.telegram.token_file.clone()),
    );
    telegram.insert(
        "allowed_user_ids".to_string(),
        toml::Value::Array(
            config
                .telegram
                .allowed_user_ids
                .iter()
                .map(|id| toml::Value::Integer(*id))
                .collect(),
        ),
    );
    telegram.insert(
        "notify_chat_id".to_string(),
        toml::Value::Integer(config.telegram.notify_chat_id),
    );
    telegram.insert(
        "poll_seconds".to_string(),
        toml::Value::Integer(config.telegram.poll_seconds as i64),
    );
    telegram.insert(
        "backoff_min_seconds".to_string(),
        toml::Value::Integer(config.telegram.backoff_min_seconds as i64),
    );
    telegram.insert(
        "backoff_max_seconds".to_string(),
        toml::Value::Integer(config.telegram.backoff_max_seconds as i64),
    );
    raw.insert("telegram".to_string(), toml::Value::Table(telegram));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_home() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-config-test-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_match_blueprint() {
        let c = Config::default();
        assert_eq!(c.theme, "default");
        assert_eq!(c.prefix, "ctrl-b");
        assert_eq!(c.permission.mode, PermissionMode::Yolo);
        assert!(c.permission.allow.is_empty() && c.permission.block.is_empty());
        assert_eq!(c.ai.provider, "openai");
        assert_eq!(c.ai.model, "gpt-4.1-mini");
        assert!((c.ai.threshold - 0.6).abs() < f64::EPSILON);
        assert_eq!(c.ai.timeout_seconds, 10);
        assert!(c.ai.auto_refresh);
        assert_eq!(c.ai.key_env, "APE_API_KEY");
        assert!(!c.claudling_enabled);
        assert!(c.telemetry_enabled);
        assert!(!c.clikan_enabled);
        assert!(c.pills_enabled, "pill tabs default on");
        assert_eq!(c.messaging_idle_minutes, 10);
    }

    #[test]
    fn pills_flag_parses_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("forge-pills-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::write(&path, "[pills]\nenabled = false\n").unwrap();
        let loaded = LoadedConfig::load(&path).unwrap();
        assert!(!loaded.config.pills_enabled);
        let back = extract(&loaded.raw).unwrap();
        assert!(!back.pills_enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_loads_defaults() {
        let home = scratch_home();
        let loaded = LoadedConfig::load_home(&home).unwrap();
        assert_eq!(loaded.config, Config::default());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unknown_sections_survive_roundtrip() {
        let home = scratch_home();
        let path = branding::config_file(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "theme = \"amber\"\n[memory]\nenabled = true\n[custom]\nkey = 42\n",
        )
        .unwrap();
        let loaded = LoadedConfig::load(&path).unwrap();
        assert_eq!(loaded.config.theme, "amber");
        loaded.save(&path).unwrap();
        let again = LoadedConfig::load(&path).unwrap();
        assert_eq!(again.config.theme, "amber");
        let text = std::fs::read_to_string(&path).unwrap();
        let doc: toml::Table = text.parse().unwrap();
        assert_eq!(doc["memory"]["enabled"].as_bool(), Some(true));
        assert_eq!(doc["custom"]["key"].as_integer(), Some(42));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let home = scratch_home();
        let path = branding::config_file(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[permission]\nmode = \"yolo\"\n").unwrap();
        let loaded = LoadedConfig::load(&path).unwrap();
        assert_eq!(loaded.config.permission.mode, PermissionMode::Yolo);
        assert_eq!(loaded.config.theme, "default");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn corrupt_file_and_bad_mode_error() {
        let home = scratch_home();
        let path = branding::config_file(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "theme = [unclosed\n").unwrap();
        assert!(matches!(
            LoadedConfig::load(&path),
            Err(ConfigError::Parse(_))
        ));
        std::fs::write(&path, "[permission]\nmode = \"super-yolo\"\n").unwrap();
        assert!(matches!(
            LoadedConfig::load(&path),
            Err(ConfigError::BadValue(_))
        ));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn bots_table_parses_registrations() {
        let home = scratch_home();
        let path = branding::config_file(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[[bots]]\nname = \"skippy\"\ntoken_file = \"~/.forge/bots/skippy.token\"\ngroups = [\"peers\"]\n\
             [[bots]]\nname = \"house\"\ntoken_file = \"/run/house.token\"\ngroups = [\"peers\", \"ops\"]\ngrants = [\"start_session\"]\n",
        )
        .unwrap();
        let loaded = LoadedConfig::load(&path).unwrap();
        assert_eq!(loaded.config.bots.len(), 2);
        assert_eq!(loaded.config.bots[0].name, "skippy");
        assert_eq!(loaded.config.bots[0].token_file, "~/.forge/bots/skippy.token");
        assert_eq!(loaded.config.bots[0].groups, vec!["peers".to_string()]);
        assert!(loaded.config.bots[0].grants.is_empty(), "grants default empty");
        assert_eq!(loaded.config.bots[1].grants, vec!["start_session".to_string()]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn bots_table_rejects_nameless_or_fileless_entries() {
        let home = scratch_home();
        let path = branding::config_file(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[[bots]]\ngroups = [\"peers\"]\ntoken_file = \"/x\"\n").unwrap();
        assert!(matches!(
            LoadedConfig::load(&path),
            Err(ConfigError::BadValue(_))
        ));
        std::fs::write(&path, "[[bots]]\nname = \"skippy\"\ngroups = [\"peers\"]\n").unwrap();
        assert!(matches!(
            LoadedConfig::load(&path),
            Err(ConfigError::BadValue(_))
        ));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn legacy_migration_cases() {
        // Legacy only -> migrated.
        let home = scratch_home();
        std::fs::create_dir_all(branding::legacy_config_dir(&home)).unwrap();
        std::fs::write(branding::legacy_config_dir(&home).join("x"), b"x").unwrap();
        assert_eq!(migrate_legacy(&home).unwrap(), Migration::Migrated);
        assert!(branding::config_dir(&home).is_dir());
        assert!(!branding::legacy_config_dir(&home).exists());
        let _ = std::fs::remove_dir_all(&home);

        // Both present -> collision, both intact.
        let home = scratch_home();
        std::fs::create_dir_all(branding::config_dir(&home)).unwrap();
        std::fs::create_dir_all(branding::legacy_config_dir(&home)).unwrap();
        assert_eq!(migrate_legacy(&home).unwrap(), Migration::Collision);
        assert!(branding::config_dir(&home).is_dir());
        assert!(branding::legacy_config_dir(&home).is_dir());
        let _ = std::fs::remove_dir_all(&home);

        // Forge only -> already current.
        let home = scratch_home();
        std::fs::create_dir_all(branding::config_dir(&home)).unwrap();
        assert_eq!(migrate_legacy(&home).unwrap(), Migration::AlreadyCurrent);
        let _ = std::fs::remove_dir_all(&home);

        // Neither -> absent.
        let home = scratch_home();
        assert_eq!(migrate_legacy(&home).unwrap(), Migration::LegacyAbsent);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn telegram_defaults_disabled_empty() {
        let c = Config::default();
        assert!(!c.telegram.enabled);
        assert!(c.telegram.token_file.is_empty());
        assert!(c.telegram.allowed_user_ids.is_empty());
        assert_eq!(c.telegram.poll_seconds, 20);
        assert_eq!(c.telegram.backoff_min_seconds, 60);
        assert_eq!(c.telegram.backoff_max_seconds, 900);
    }

    #[test]
    fn telegram_section_parses_and_round_trips() {
        let home = scratch_home();
        let path = branding::config_file(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[telegram]\nenabled = true\ntoken_file = \"/run/tg.token\"\nallowed_user_ids = [11, 22]\n",
        )
        .unwrap();
        let loaded = LoadedConfig::load(&path).unwrap();
        assert!(loaded.config.telegram.enabled);
        assert_eq!(loaded.config.telegram.token_file, "/run/tg.token");
        assert_eq!(loaded.config.telegram.allowed_user_ids, vec![11, 22]);
        assert_eq!(loaded.config.telegram.poll_seconds, 20);
        loaded.save(&path).unwrap();
        let again = LoadedConfig::load(&path).unwrap();
        assert_eq!(again.config.telegram, loaded.config.telegram);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn telegram_notify_chat_defaults_unset() {
        assert_eq!(Config::default().telegram.notify_chat_id, 0);
    }

    #[test]
    fn telegram_notify_chat_parses_and_round_trips() {
        let home = scratch_home();
        let path = branding::config_file(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[telegram]\nnotify_chat_id = 42\n").unwrap();
        let loaded = LoadedConfig::load(&path).unwrap();
        assert_eq!(loaded.config.telegram.notify_chat_id, 42);
        loaded.save(&path).unwrap();
        let again = LoadedConfig::load(&path).unwrap();
        assert_eq!(again.config.telegram.notify_chat_id, 42);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn telegram_rejects_inverted_backoff() {
        let home = scratch_home();
        let path = branding::config_file(&home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[telegram]\nbackoff_min_seconds = 900\nbackoff_max_seconds = 60\n",
        )
        .unwrap();
        assert!(matches!(
            LoadedConfig::load(&path),
            Err(ConfigError::BadValue(_))
        ));
        let _ = std::fs::remove_dir_all(&home);
    }
}
