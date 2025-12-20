use proxy::config::Config;
use proxy::errors::ProxyError;
use proxy::run;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), ProxyError> {
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    let config = Config::load(&config_path)?;
    run(config).await
}
