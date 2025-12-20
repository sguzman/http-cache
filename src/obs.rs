use crate::config::{EnvMode, LoggingConfig, LoggingFormat, TimeConfig};
use chrono::Utc;
use chrono_tz::Tz;
use std::str::FromStr;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt;
use tracing_subscriber::EnvFilter;

pub fn init_tracing(env: EnvMode, logging: &LoggingConfig, time: &TimeConfig) {
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

    let timer = TimezoneTimer::new(&time.timezone);
    let builder = fmt()
        .with_env_filter(filter)
        .with_target(env == EnvMode::Dev)
        .with_timer(timer);

    match logging.format {
        LoggingFormat::Json => {
            builder.json().init();
        }
        LoggingFormat::Pretty => {
            builder.pretty().init();
        }
    }
}

struct TimezoneTimer {
    tz: Tz,
}

impl TimezoneTimer {
    fn new(tz_name: &str) -> Self {
        let tz = Tz::from_str(tz_name).unwrap_or(chrono_tz::UTC);
        Self { tz }
    }
}

impl FormatTime for TimezoneTimer {
    fn format_time(
        &self,
        w: &mut tracing_subscriber::fmt::format::Writer<'_>,
    ) -> std::fmt::Result {
        let local = Utc::now().with_timezone(&self.tz);
        write!(w, "{}", local.to_rfc3339())
    }
}
