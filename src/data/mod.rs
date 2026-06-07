//! Data module for configuration and credential management

mod config;
mod credential;

pub use config::{
    AppConfig, AsrConfig, FloatingButtonConfig, GeneralConfig, HotkeyConfig, TextInsertionConfig,
};
pub use credential::CredentialStore;
