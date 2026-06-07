//! Application Configuration
//!
//! Handles loading and saving application configuration.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub hotkey: HotkeyConfig,
    #[serde(default)]
    pub floating_button: FloatingButtonConfig,
    #[serde(default)]
    pub asr: AsrConfig,
    #[serde(default)]
    pub text_insertion: TextInsertionConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            hotkey: HotkeyConfig::default(),
            floating_button: FloatingButtonConfig::default(),
            asr: AsrConfig::default(),
            text_insertion: TextInsertionConfig::default(),
        }
    }
}

impl AppConfig {
    /// Get the config file path
    pub fn config_path() -> PathBuf {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        exe_dir.join("config.toml")
    }

    /// Get the credentials file path
    pub fn credentials_path() -> PathBuf {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        exe_dir.join("credentials.json")
    }

    /// Load configuration from file or create default.
    ///
    /// Existing config files are automatically migrated to the current safe
    /// defaults so users do not have to learn or manually add new settings.
    pub fn load_or_default() -> Result<Self> {
        let path = Self::config_path();

        if path.exists() {
            let content = fs::read_to_string(&path)?;
            let mut config: AppConfig = toml::from_str(&content)?;
            if config.apply_safe_migrations(&content) || config.needs_migration(&content) {
                config.save()?;
            }
            Ok(config)
        } else {
            let config = AppConfig::default();
            config.save()?;
            Ok(config)
        }
    }

    fn apply_safe_migrations(&mut self, content: &str) -> bool {
        let mut changed = false;

        // The previous low-latency profile sent several optional ASR flags at
        // once. Some Doubao sessions accept it, but when the service rejects or
        // ignores that profile the UI appears to record while recognition never
        // produces text. Prefer the conservative protocol unless the user keeps
        // an explicit non-default override.
        let legacy_aggressive_asr = content.contains("low_latency_mode = true")
            && content.contains("enable_asr_twopass = true")
            && content.contains("enable_asr_threepass = false")
            && content.contains("interim_insert = true");
        if legacy_aggressive_asr {
            self.asr.low_latency_mode = false;
            self.asr.enable_asr_twopass = false;
            changed = true;
        }

        // 80 ms is too short for a number of Windows targets: they may process
        // Ctrl+V after we have already restored the original clipboard, making
        // clipboard insertion look like it did nothing.
        if content.contains("clipboard_restore_delay_ms = 80")
            && self.text_insertion.clipboard_restore_delay_ms <= 80
        {
            self.text_insertion.clipboard_restore_delay_ms = default_clipboard_restore_delay_ms();
            changed = true;
        }

        changed
    }

    fn needs_migration(&self, content: &str) -> bool {
        let required_keys = [
            "low_latency_mode",
            "enable_asr_twopass",
            "enable_asr_threepass",
            "interim_insert",
            "interim_update_interval_ms",
            "max_interim_rollback_chars",
            "final_drain_timeout_ms",
            "[text_insertion]",
            "clipboard_threshold_chars",
            "clipboard_restore_delay_ms",
        ];

        required_keys.iter().any(|key| !content.contains(key))
    }

    /// Save configuration to file
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        let content = toml::to_string_pretty(self)?;
        fs::write(&path, content)?;
        Ok(())
    }
}

/// General configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "zh-CN".to_string()
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            auto_start: false,
            language: default_language(),
        }
    }
}

/// Hotkey configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfig {
    #[serde(default = "default_hotkey_mode")]
    pub mode: String,
    #[serde(default = "default_combo_key")]
    pub combo_key: String,
    #[serde(default = "default_double_tap_key")]
    pub double_tap_key: String,
    #[serde(default = "default_double_tap_interval")]
    pub double_tap_interval: u64,
    #[serde(default = "default_tap_hold_key")]
    pub tap_hold_key: String,
    #[serde(default = "default_tap_hold_threshold")]
    pub tap_hold_threshold: u64,
}

fn default_hotkey_mode() -> String {
    "tap_hold".to_string()
}

fn default_combo_key() -> String {
    "Ctrl+Shift+V".to_string()
}

fn default_double_tap_key() -> String {
    "Ctrl".to_string()
}

fn default_double_tap_interval() -> u64 {
    300
}

fn default_tap_hold_key() -> String {
    "RightAlt".to_string()
}

fn default_tap_hold_threshold() -> u64 {
    300
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            mode: default_hotkey_mode(),
            combo_key: default_combo_key(),
            double_tap_key: default_double_tap_key(),
            double_tap_interval: default_double_tap_interval(),
            tap_hold_key: default_tap_hold_key(),
            tap_hold_threshold: default_tap_hold_threshold(),
        }
    }
}

/// Floating button configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloatingButtonConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_position")]
    pub position_x: i32,
    #[serde(default = "default_position")]
    pub position_y: i32,
}

fn default_true() -> bool {
    true
}

fn default_position() -> i32 {
    100
}

impl Default for FloatingButtonConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            position_x: 100,
            position_y: 100,
        }
    }
}

/// ASR configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrConfig {
    #[serde(default = "default_true")]
    pub vad_enabled: bool,
    #[serde(default = "default_false")]
    pub low_latency_mode: bool,
    #[serde(default = "default_false")]
    pub enable_asr_twopass: bool,
    #[serde(default = "default_false")]
    pub enable_asr_threepass: bool,
    #[serde(default = "default_true")]
    pub interim_insert: bool,
    #[serde(default = "default_interim_update_interval_ms")]
    pub interim_update_interval_ms: u64,
    #[serde(default = "default_max_interim_rollback_chars")]
    pub max_interim_rollback_chars: usize,
    #[serde(default = "default_final_drain_timeout_ms")]
    pub final_drain_timeout_ms: u64,
}

fn default_false() -> bool {
    false
}

fn default_interim_update_interval_ms() -> u64 {
    150
}

fn default_max_interim_rollback_chars() -> usize {
    6
}

fn default_final_drain_timeout_ms() -> u64 {
    1500
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            vad_enabled: true,
            low_latency_mode: false,
            enable_asr_twopass: false,
            enable_asr_threepass: false,
            interim_insert: true,
            interim_update_interval_ms: default_interim_update_interval_ms(),
            max_interim_rollback_chars: default_max_interim_rollback_chars(),
            final_drain_timeout_ms: default_final_drain_timeout_ms(),
        }
    }
}

/// Text insertion configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextInsertionConfig {
    #[serde(default = "default_text_insert_mode")]
    pub mode: String,
    #[serde(default = "default_clipboard_threshold_chars")]
    pub clipboard_threshold_chars: usize,
    #[serde(default = "default_clipboard_restore_delay_ms")]
    pub clipboard_restore_delay_ms: u64,
}

fn default_text_insert_mode() -> String {
    "auto".to_string()
}

fn default_clipboard_threshold_chars() -> usize {
    8
}

fn default_clipboard_restore_delay_ms() -> u64 {
    350
}

impl Default for TextInsertionConfig {
    fn default() -> Self {
        Self {
            mode: default_text_insert_mode(),
            clipboard_threshold_chars: default_clipboard_threshold_chars(),
            clipboard_restore_delay_ms: default_clipboard_restore_delay_ms(),
        }
    }
}
