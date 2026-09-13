//! Logging bootstrap.
//!
//! Verbosity is controlled with the `RUST_LOG` environment variable,
//! e.g. `RUST_LOG=wini=debug`. Defaults to `info`.

use std::io::Write;

/// Verbosity is controlled with the `RUST_LOG` environment variable,
/// e.g. `RUST_LOG=wini=debug`. Defaults to `info`.
pub fn init() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| v.parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info);

    let _ = env_logger::Builder::new()
        .filter_level(level)
        .format(|buf, record| {
            let level = record.level();
            writeln!(
                buf,
                "[{level:>5}] {} ({}): {}",
                buf.timestamp_seconds(),
                record.target().rsplit("::").next().unwrap_or(record.target()),
                record.args()
            )
        })
        .try_init();
}
