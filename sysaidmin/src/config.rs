use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use log::{debug, info, trace, warn};
use serde::Deserialize;

use crate::allowlist::AllowlistConfig;

const DEFAULT_MODEL: &str = "claude-4-5-sonnet";
const DEFAULT_SHELL: &str = "/bin/bash";
const DEFAULT_API_URL: &str = "https://api.anthropic.com/v1/messages";

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub api_key: String,
    pub api_url: String,
    pub model: String,
    pub default_shell: String,
    pub allowlist: AllowlistConfig,
    pub history_limit: usize,
    pub offline_mode: bool,
    pub dry_run: bool,
    pub session_root: PathBuf,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    anthropic_api_key: Option<String>,
    anthropic_api_url: Option<String>,
    anthropic_model: Option<String>,
    default_shell: Option<String>,
    allowlist: Option<AllowlistConfig>,
    history_limit: Option<usize>,
    offline_mode: Option<bool>,
    dry_run: Option<bool>,
    session_dir: Option<String>,
}

fn empty_file_config() -> FileConfig {
    FileConfig {
        anthropic_api_key: None,
        anthropic_api_url: None,
        anthropic_model: None,
        default_shell: None,
        allowlist: None,
        history_limit: None,
        offline_mode: None,
        dry_run: None,
        session_dir: None,
    }
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        info!("Loading application configuration");
        trace!("Reading file config");
        let file_cfg = read_file_config()?;

        trace!("Resolving API key");
        let api_key = resolve_api_key(file_cfg.anthropic_api_key.clone())?;
        debug!("API key resolved (length: {} chars)", api_key.len());

        let api_url = file_cfg
            .anthropic_api_url
            .unwrap_or_else(|| DEFAULT_API_URL.to_string());
        info!("API URL: {}", api_url);

        let model = file_cfg
            .anthropic_model
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        info!("Model: {}", model);

        let default_shell = file_cfg
            .default_shell
            .unwrap_or_else(|| DEFAULT_SHELL.to_string());
        debug!("Default shell: {}", default_shell);

        let allowlist = file_cfg.allowlist.unwrap_or_default();
        debug!("Allowlist loaded");

        let history_limit = file_cfg.history_limit.unwrap_or(50);
        debug!("History limit: {}", history_limit);

        let offline_mode = file_cfg.offline_mode.unwrap_or(false);
        if offline_mode {
            warn!("Offline mode enabled");
        }

        let dry_run = resolve_bool("SYSAIDMIN_DRYRUN")
            .or(file_cfg.dry_run)
            .unwrap_or(false);
        if dry_run {
            warn!("Dry-run mode enabled");
        }

        trace!("Resolving session directory");
        let session_root = resolve_session_dir(file_cfg.session_dir.as_deref())?;
        info!("Session root: {}", session_root.display());

        info!("Configuration loaded successfully");
        Ok(Self {
            api_key,
            api_url,
            model,
            default_shell,
            allowlist,
            history_limit,
            offline_mode,
            dry_run,
            session_root,
        })
    }
}

fn read_file_config() -> Result<FileConfig> {
    let Some(path) = config_file_path() else {
        debug!("No config file path found, using defaults");
        return Ok(empty_file_config());
    };

    if !path.exists() {
        debug!(
            "Config file does not exist: {}, using defaults",
            path.display()
        );
        return Ok(empty_file_config());
    }

    info!("Reading config file: {}", path.display());
    let data = fs::read_to_string(&path)
        .with_context(|| format!("failed reading config file {}", path.display()))?;

    trace!("Parsing TOML config");
    toml::from_str(&data).with_context(|| {
        format!(
            "invalid TOML in {} (make sure string values are quoted)",
            path.display()
        )
    })
}

fn config_file_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("sysaidmin").join("config.toml"))
}

fn resolve_api_key(file_key: Option<String>) -> Result<String> {
    if let Some(key) = env_value("SYSAIDMIN_API_KEY") {
        return Ok(key);
    }
    if let Some(key) = env_value("ANTHROPIC_API_KEY") {
        return Ok(key);
    }
    if let Some(key) = env_value("CLAUDE_API_KEY") {
        return Ok(key);
    }
    if let Some(key) = file_key {
        return Ok(key);
    }
    if let Some(key) = read_dotfile_key()? {
        return Ok(key);
    }
    let config_hint = config_file_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.sysaidmin/config.toml".to_string());
    Err(anyhow!(
        "Missing API key.\nSet SYSAIDMIN_API_KEY / ANTHROPIC_API_KEY\n\
         or add `anthropic_api_key = \"sk-...\"` to {config_hint}"
    ))
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn resolve_bool(name: &str) -> Option<bool> {
    let val = env_value(name)?;
    match val.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn resolve_session_dir(file_override: Option<&str>) -> Result<PathBuf> {
    if let Some(env_path) = env_value("SYSAIDMIN_SESSION_DIR") {
        return Ok(PathBuf::from(env_path));
    }
    if let Some(path) = file_override {
        return Ok(PathBuf::from(path));
    }
    let base = dirs::data_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local/share")));
    Ok(base.unwrap_or_else(|| PathBuf::from(".")).join("sysaidmin"))
}

fn read_dotfile_key() -> Result<Option<String>> {
    let Some(home) = dirs::home_dir() else {
        return Ok(None);
    };
    let legacy = home.join(".sysaidmin");
    if !legacy.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&legacy)
        .with_context(|| format!("failed reading {}", legacy.display()))?;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '=').collect();
        if parts.len() == 2
            && (parts[0].trim().eq_ignore_ascii_case("ANTHROPIC_API_KEY")
                || parts[0].trim().eq_ignore_ascii_case("api_key"))
            {
                return Ok(Some(parts[1].trim().trim_matches('"').to_string()));
            }
    }
    Ok(None)
}

impl Default for FileConfig {
    fn default() -> Self {
        empty_file_config()
    }
}
