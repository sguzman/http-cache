use crate::config::{EnvMode, LoggingConfig, LoggingFormat};
use tracing_subscriber::fmt;
use tracing_subscriber::EnvFilter;

pub fn init_tracing(env: EnvMode, logging: &LoggingConfig) {
    let default_level = if env == EnvMode::Dev {
        "debug"
    } else {
        "info"
    };

    let level = if logging.level.trim().is_empty() {
        default_level
    } else {
        &logging.level
    };

    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new(default_level));

    let builder = fmt()
        .with_env_filter(filter)
        .with_target(env == EnvMode::Dev);

    match logging.format {
        LoggingFormat::Json => {
            builder.json().init();
        }
        LoggingFormat::Pretty => {
            builder.pretty().init();
        }
    }
}
