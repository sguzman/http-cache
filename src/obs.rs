use crate::config::{EnvMode, LoggingConfig, LoggingFormat, TimeConfig};
use chrono::Utc;
use chrono_tz::Tz;
use std::fs;
use std::io::{self, Write};
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt;
use tracing_subscriber::EnvFilter;

static LOG_GUARDS: OnceLock<Mutex<Vec<WorkerGuard>>> = OnceLock::new();

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

    if env == EnvMode::Dev {
        if let Some((file_writer, guard)) = dev_log_writer() {
            let guards = LOG_GUARDS.get_or_init(|| Mutex::new(Vec::new()));
            if let Ok(mut guards) = guards.lock() {
                guards.push(guard);
            }

            match logging.format {
                LoggingFormat::Json => {
                    builder
                        .json()
                        .with_writer(move || TeeWriter::new(io::stderr(), file_writer.clone()))
                        .init();
                }
                LoggingFormat::Pretty => {
                    builder
                        .pretty()
                        .with_writer(move || TeeWriter::new(io::stderr(), file_writer.clone()))
                        .init();
                }
            }
            return;
        }

        eprintln!("failed to initialize dev file logging under logs/");
    }

    match logging.format {
        LoggingFormat::Json => {
            builder.json().init();
        }
        LoggingFormat::Pretty => {
            builder.pretty().init();
        }
    }
}

fn dev_log_writer() -> Option<(tracing_appender::non_blocking::NonBlocking, WorkerGuard)> {
    fs::create_dir_all("logs").ok()?;
    let file_appender = tracing_appender::rolling::daily("logs", "httpcache-dev.log");
    Some(tracing_appender::non_blocking(file_appender))
}

struct TeeWriter<A, B> {
    left: A,
    right: B,
}

impl<A, B> TeeWriter<A, B> {
    fn new(left: A, right: B) -> Self {
        Self { left, right }
    }
}

impl<A, B> Write for TeeWriter<A, B>
where
    A: Write,
    B: Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.left.write_all(buf)?;
        self.right.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.left.flush()?;
        self.right.flush()
    }
}

#[derive(Clone)]
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
