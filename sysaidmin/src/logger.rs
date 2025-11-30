use chrono::Local;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct FileLogger {
    file: Arc<Mutex<File>>,
    level: LevelFilter,
}

impl FileLogger {
    pub fn new(log_path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;

        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            level: LevelFilter::Trace,
        })
    }

    pub fn init(log_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
        let logger = Self::new(log_path)?;
        log::set_boxed_logger(Box::new(logger))?;
        log::set_max_level(LevelFilter::Trace);
        Ok(())
    }

    fn write_log(&self, record: &Record) {
        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let level_str = match record.level() {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        };

        let message = format!(
            "[{}] {} [{}:{}] {}\n",
            timestamp,
            level_str,
            record.module_path().unwrap_or("<unknown>"),
            record.line().unwrap_or(0),
            record.args()
        );

        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(message.as_bytes());
            let _ = file.flush();
        }
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            self.write_log(record);
            // Only print warnings and errors to stderr to avoid interfering with TUI
            // Trace, debug, and info go only to the log file
            match record.level() {
                Level::Error | Level::Warn => {
                    eprintln!("{}", record.args());
                }
                _ => {
                    // Trace, debug, and info are silent on stderr - only in log file
                }
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}
