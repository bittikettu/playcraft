//! In-memory ring logger.
//!
//! A TUI cannot print to stderr, so log records go to a bounded ring that the UI can show
//! (and turn warnings into banners), and optionally to a log file.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use log::{Level, LevelFilter, Log, Metadata, Record};

const CAPACITY: usize = 500;

#[derive(Debug, Clone)]
pub struct LogLine {
    /// Monotonic sequence number, used to find lines newer than the last one seen.
    pub seq: u64,
    pub level: Level,
    /// Seconds since the logger was initialised.
    pub elapsed_secs: f64,
    pub message: String,
}

struct Inner {
    lines: VecDeque<LogLine>,
    next_seq: u64,
    file: Option<File>,
}

pub struct RingLogger {
    start: Instant,
    level: LevelFilter,
    inner: Mutex<Inner>,
}

static LOGGER: OnceLock<RingLogger> = OnceLock::new();

/// Installs the ring logger as the global `log` backend. Safe to call more than once;
/// only the first call has an effect.
pub fn init(level: LevelFilter, file: Option<&Path>) -> &'static RingLogger {
    let mut installed = false;
    let logger = LOGGER.get_or_init(|| {
        installed = true;
        let file = file.and_then(|p| {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            OpenOptions::new().create(true).append(true).open(p).ok()
        });
        RingLogger {
            start: Instant::now(),
            level,
            inner: Mutex::new(Inner {
                lines: VecDeque::with_capacity(CAPACITY),
                next_seq: 1,
                file,
            }),
        }
    });
    if installed && log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
    logger
}

/// The installed logger, if `init` has been called.
pub fn get() -> Option<&'static RingLogger> {
    LOGGER.get()
}

impl RingLogger {
    /// All retained lines with a sequence number greater than `seq`.
    pub fn lines_after(&self, seq: u64) -> Vec<LogLine> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .lines
            .iter()
            .filter(|l| l.seq > seq)
            .cloned()
            .collect()
    }

    pub fn snapshot(&self) -> Vec<LogLine> {
        self.lines_after(0)
    }
}

impl Log for RingLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let elapsed_secs = self.start.elapsed().as_secs_f64();
        let message = record.args().to_string();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(file) = inner.file.as_mut() {
            let _ = writeln!(
                file,
                "[{elapsed_secs:10.3}] {:5} {}: {message}",
                record.level(),
                record.target()
            );
        }
        let seq = inner.next_seq;
        inner.next_seq += 1;
        if inner.lines.len() == CAPACITY {
            inner.lines.pop_front();
        }
        inner.lines.push_back(LogLine {
            seq,
            level: record.level(),
            elapsed_secs,
            message,
        });
    }

    fn flush(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(file) = inner.file.as_mut() {
            let _ = file.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_lines_and_filters_by_sequence() {
        let logger = init(LevelFilter::Debug, None);
        log::warn!("first warning");
        log::info!("second line");
        let lines = logger.snapshot();
        let first = lines
            .iter()
            .find(|l| l.message == "first warning")
            .expect("logged");
        assert_eq!(first.level, Level::Warn);
        let newer = logger.lines_after(first.seq);
        assert!(newer.iter().any(|l| l.message == "second line"));
        assert!(newer.iter().all(|l| l.seq > first.seq));
    }
}
