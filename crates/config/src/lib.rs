pub mod proxy;

use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("failed to load config: {0}")]
    Load(#[from] external_config::ConfigError),
    #[error("env file not found: {0}")]
    EnvFile(String),
}

pub fn load_from_env() -> Result<proxy::ProxyConfig, ConfigError> {
    if Path::new(".env").exists() {
        let _ = dotenvy::from_path(".env");
    }
    let cfg = external_config::Config::builder()
        .add_source(external_config::Environment::default().separator("__"))
        .build()?;
    Ok(cfg.try_deserialize()?)
}
