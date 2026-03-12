use std::fmt;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

/// Fixed-width target column width. Targets longer than this are truncated.
const TARGET_WIDTH: usize = 30;

/// Log output target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogOutput {
    Stdout,
    File,
    Journald,
}

impl std::str::FromStr for LogOutput {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "stdout" => Ok(Self::Stdout),
            "file" => Ok(Self::File),
            "journald" | "journal" | "systemd" => Ok(Self::Journald),
            other => Err(format!(
                "unknown log output '{other}' (valid: stdout, file, journald)"
            )),
        }
    }
}

/// Log file rotation strategy.
#[derive(Debug, Clone, Copy, Default)]
pub enum LogRotation {
    #[default]
    Daily,
    Hourly,
    Never,
}

impl std::str::FromStr for LogRotation {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "daily" => Ok(Self::Daily),
            "hourly" => Ok(Self::Hourly),
            "never" | "none" => Ok(Self::Never),
            other => Err(format!(
                "unknown rotation '{other}' (valid: daily, hourly, never)"
            )),
        }
    }
}

/// Options for initializing the logging system.
pub struct LoggingOpts {
    pub outputs: Vec<LogOutput>,
    pub level: String,
    pub log_dir: String,
    pub rotation: LogRotation,
    pub no_color: bool,
}

/// Guard that must be held for the lifetime of the program.
/// Dropping this flushes any buffered log output (e.g., file writer).
pub struct LogGuard {
    _guards: Vec<tracing_appender::non_blocking::WorkerGuard>,
}

/// Initialize the logging system with the given options.
///
/// Returns a [`LogGuard`] that must be held until program exit to ensure
/// buffered output (file logs) is flushed.
pub fn init(opts: &LoggingOpts) -> anyhow::Result<LogGuard> {
    let mut guards: Vec<tracing_appender::non_blocking::WorkerGuard> = Vec::new();

    // Use type-erased boxed layers so we can compose dynamically
    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = Vec::new();

    let outputs = if opts.outputs.is_empty() {
        vec![LogOutput::Stdout]
    } else {
        opts.outputs.clone()
    };

    for output in &outputs {
        match output {
            LogOutput::Stdout => {
                let ansi = !opts.no_color && atty_stdout();
                let layer = tracing_subscriber::fmt::layer()
                    .event_format(AlignedFormatter { ansi })
                    .with_ansi(ansi)
                    .with_filter(build_filter(&opts.level));
                layers.push(Box::new(layer));
            }
            LogOutput::File => {
                std::fs::create_dir_all(&opts.log_dir)?;
                let file_appender = match opts.rotation {
                    LogRotation::Daily => {
                        tracing_appender::rolling::daily(&opts.log_dir, "torsten.log")
                    }
                    LogRotation::Hourly => {
                        tracing_appender::rolling::hourly(&opts.log_dir, "torsten.log")
                    }
                    LogRotation::Never => {
                        tracing_appender::rolling::never(&opts.log_dir, "torsten.log")
                    }
                };
                let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
                guards.push(guard);

                let layer = tracing_subscriber::fmt::layer()
                    .event_format(AlignedFormatter { ansi: false })
                    .with_ansi(false)
                    .with_writer(non_blocking)
                    .with_filter(build_filter(&opts.level));
                layers.push(Box::new(layer));
            }
            LogOutput::Journald => {
                #[cfg(feature = "journald")]
                {
                    let layer = tracing_journald::layer()
                        .map_err(|e| anyhow::anyhow!("Failed to connect to journald: {e}"))?
                        .with_filter(build_filter(&opts.level));
                    layers.push(Box::new(layer));
                }
                #[cfg(not(feature = "journald"))]
                {
                    anyhow::bail!(
                        "journald output requires the 'journald' feature (rebuild with --features journald)"
                    );
                }
            }
        }
    }

    Registry::default().with(layers).init();

    Ok(LogGuard { _guards: guards })
}

/// Build an `EnvFilter` from the given level string.
/// `RUST_LOG` env var takes priority if set.
fn build_filter(level: &str) -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level))
}

/// Check if stdout is a terminal (for auto-detecting color support).
fn atty_stdout() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

// ---------------------------------------------------------------------------
// Aligned formatter
// ---------------------------------------------------------------------------

/// Custom log event formatter with fixed-width columns:
///   `HH:MM:SS.mmm  LEVEL  target<30>  message`
struct AlignedFormatter {
    ansi: bool,
}

impl<S, N> FormatEvent<S, N> for AlignedFormatter
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        // Timestamp
        let now = chrono::Local::now();
        let timestamp = now.format("%H:%M:%S%.3f");

        // Level with color
        let level = *event.metadata().level();
        let level_str = if self.ansi {
            match level {
                tracing::Level::ERROR => "\x1b[31mERROR\x1b[0m",
                tracing::Level::WARN => "\x1b[33m WARN\x1b[0m",
                tracing::Level::INFO => "\x1b[32m INFO\x1b[0m",
                tracing::Level::DEBUG => "\x1b[34mDEBUG\x1b[0m",
                tracing::Level::TRACE => "\x1b[35mTRACE\x1b[0m",
            }
        } else {
            match level {
                tracing::Level::ERROR => "ERROR",
                tracing::Level::WARN => " WARN",
                tracing::Level::INFO => " INFO",
                tracing::Level::DEBUG => "DEBUG",
                tracing::Level::TRACE => "TRACE",
            }
        };

        // Target with fixed width
        let target = event.metadata().target();
        let target_display: String = if target.len() <= TARGET_WIDTH {
            format!("{target:<TARGET_WIDTH$}")
        } else {
            // Truncate from the left (keep the most specific part)
            let truncated = &target[target.len() - (TARGET_WIDTH - 2)..];
            format!("..{truncated}")
        };

        // Dim the target if ANSI is enabled
        if self.ansi {
            write!(
                writer,
                "{timestamp} {level_str} \x1b[2m{target_display}\x1b[0m "
            )?;
        } else {
            write!(writer, "{timestamp} {level_str} {target_display} ")?;
        }

        // Message fields
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_output_from_str() {
        assert_eq!("stdout".parse::<LogOutput>().unwrap(), LogOutput::Stdout);
        assert_eq!("file".parse::<LogOutput>().unwrap(), LogOutput::File);
        assert_eq!(
            "journald".parse::<LogOutput>().unwrap(),
            LogOutput::Journald
        );
        assert_eq!("journal".parse::<LogOutput>().unwrap(), LogOutput::Journald);
        assert_eq!("systemd".parse::<LogOutput>().unwrap(), LogOutput::Journald);
        assert_eq!("STDOUT".parse::<LogOutput>().unwrap(), LogOutput::Stdout);
        assert!("invalid".parse::<LogOutput>().is_err());
    }

    #[test]
    fn test_log_rotation_from_str() {
        assert!(matches!(
            "daily".parse::<LogRotation>().unwrap(),
            LogRotation::Daily
        ));
        assert!(matches!(
            "hourly".parse::<LogRotation>().unwrap(),
            LogRotation::Hourly
        ));
        assert!(matches!(
            "never".parse::<LogRotation>().unwrap(),
            LogRotation::Never
        ));
        assert!(matches!(
            "none".parse::<LogRotation>().unwrap(),
            LogRotation::Never
        ));
        assert!("invalid".parse::<LogRotation>().is_err());
    }
}
