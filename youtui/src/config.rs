use crate::get_config_dir;
use anyhow::{Context, Result};
use clap::ValueEnum;
use keymap::{YoutuiKeymap, YoutuiKeymapIR, YoutuiModeNamesIR};
use serde::{Deserialize, Serialize};
const CONFIG_FILE_NAME: &str = "config.toml";

pub mod keymap;

#[derive(Serialize, Deserialize)]
pub enum ApiKey {
    // BrowserToken takes the cookie, not the BrowserToken itself. This is because to obtain the
    // BrowserToken you must make a web request, and we want to obtain it as lazily as possible.
    BrowserToken(String),
    None,
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiKey::BrowserToken(_) => f
                .debug_tuple("BrowserToken")
                .field(&"/* private fields */")
                .finish(),
            ApiKey::None => f.debug_tuple("None").finish(),
        }
    }
}

#[derive(ValueEnum, Copy, PartialEq, Clone, Default, Debug, Serialize, Deserialize)]
pub enum AuthType {
    #[default]
    Browser,
    Unauthenticated,
}

fn default_volume() -> u8 {
    50
}

fn default_yt_dlp_command() -> String {
    String::from("yt-dlp")
}

fn default_notifications_enabled() -> bool {
    true
}

fn default_cache_size() -> usize {
    1
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub auth_type: AuthType,
    pub yt_dlp_command: String,
    pub keybinds: YoutuiKeymap,
    pub volume: u8,
    pub notifications_enabled: bool,
    pub download_cache_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auth_type: Default::default(),
            yt_dlp_command: default_yt_dlp_command(),
            keybinds: Default::default(),
            volume: default_volume(),
            notifications_enabled: true,
            download_cache_size: default_cache_size(),
        }
    }
}

#[derive(Default, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Intermediate representation of Config for serde.
pub struct ConfigIR {
    pub auth_type: AuthType,
    #[serde(default = "default_yt_dlp_command")]
    pub yt_dlp_command: String,
    pub keybinds: YoutuiKeymapIR,
    pub mode_names: YoutuiModeNamesIR,
    #[serde(default = "default_volume")]
    pub volume: u8,
    #[serde(default = "default_notifications_enabled")]
    pub notifications_enabled: bool,
    #[serde(default = "default_cache_size")]
    pub download_cache_size: usize,
}

impl TryFrom<ConfigIR> for Config {
    type Error = anyhow::Error;
    fn try_from(value: ConfigIR) -> std::result::Result<Self, Self::Error> {
        let ConfigIR {
            auth_type,
            keybinds,
            mode_names,
            yt_dlp_command,
            volume,
            notifications_enabled,
            download_cache_size,
        } = value;
        Ok(Config {
            auth_type,
            keybinds: YoutuiKeymap::try_from_stringy(keybinds, mode_names)?,
            yt_dlp_command,
            volume,
            notifications_enabled,
            download_cache_size,
        })
    }
}

impl Config {
    pub async fn new(debug: bool) -> Result<Self> {
        let config_dir = get_config_dir()?;
        let config_file_location = config_dir.join(CONFIG_FILE_NAME);
        if let Ok(config_file) = tokio::fs::read_to_string(&config_file_location).await {
            // NOTE: This happens before logging / app is initialised, so `println!` is
            // used instead of `tracing::info!`
            if debug {
                println!(
                    "Loading config from {}",
                    config_file_location.to_string_lossy()
                );
            }
            let ir: ConfigIR = toml::from_str(&config_file)
                .context("Error deserializing config file from toml")?;
            Ok(Config::try_from(ir).context("Error processing config file")?)
        } else {
            if debug {
                println!(
                    "Config file not found in {}, using defaults",
                    config_file_location.to_string_lossy()
                );
            }
            Ok(Self::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::keymap::YoutuiKeymap;
    use crate::config::{Config, ConfigIR};
    use pretty_assertions::{assert_eq, assert_ne};

    async fn example_config_file() -> String {
        tokio::fs::read_to_string("./config/config.toml")
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_deserialize_default_config_to_ir() {
        let config_file = example_config_file().await;
        toml::from_str::<ConfigIR>(&config_file).unwrap();
    }
    #[tokio::test]
    async fn test_convert_ir_to_config() {
        let config_file = example_config_file().await;
        let ir: ConfigIR = toml::from_str(&config_file).unwrap();
        Config::try_from(ir).unwrap();
    }
    #[tokio::test]
    async fn test_unknown_keys_in_config() {
        let config_file = r#"auth_typo = 'Browser'"#;
        // ASSERT: the provided toml is valid so therefore the error is related
        // specifically to parsing into [ConfigIR]
        //
        // See https://github.com/nick42d/youtui/pull/366
        let config_toml: toml::Value = toml::from_str(config_file).unwrap();
        let ir: Result<ConfigIR, _> = config_toml.try_into();
        assert!(ir.is_err());
    }
    #[test]
    fn default_cache_size_is_one() {
        // Product decision (DECISIONS.md:13/16, AGENTS.md): cache max=1 — one
        // ALAC buffer playing + one cached ≈ 32MB. 3 would be ~48MB.
        assert_eq!(Config::default().download_cache_size, 1);
    }
    #[tokio::test]
    async fn test_unknown_keybind_parameters() {
        let config_file = r#"[keybinds.global]
raisevolume = {action = "vol_up", visiblity = "hidden"}"#;
        // ASSERT: the provided toml is valid so therefore the error is related
        // specifically to parsing into [ConfigIR]
        //
        // See https://github.com/nick42d/youtui/pull/366
        let config_toml: toml::Value = toml::from_str(config_file).unwrap();
        let ir: Result<ConfigIR, _> = config_toml.try_into();
        assert!(ir.is_err());
    }
    #[tokio::test]
    async fn test_default_config_equals_deserialized_config() {
        let config_file = example_config_file().await;
        let ConfigIR {
            auth_type,
            keybinds,
            mode_names,
            yt_dlp_command,
            volume: _,
            ..
        } = toml::from_str(&config_file).unwrap();
        let keybinds = YoutuiKeymap::try_from_stringy_exact(keybinds, mode_names).unwrap();
        let config = Config {
            auth_type,
            keybinds,
            yt_dlp_command,
            volume: super::default_volume(),
            notifications_enabled: true,
            download_cache_size: super::default_cache_size(),
        };
        assert_eq!(config, Config::default());
    }
    #[tokio::test]
    async fn test_default_config_equals_blank_config() {
        let ir: ConfigIR = toml::from_str("").unwrap();
        let config = Config::try_from(ir).unwrap();
        assert_eq!(config, Config::default());
    }
    #[tokio::test]
    async fn test_different_config_to_default() {
        let config_file = tokio::fs::read_to_string("./config/config.toml.vim-example")
            .await
            .unwrap();
        let ir: ConfigIR = toml::from_str(&config_file).unwrap();
        let config = Config::try_from(ir).unwrap();
        let def_config = Config::default();
        assert_ne!(config, def_config)
    }
}
