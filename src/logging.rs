//! Logging: stderr (`RUST_LOG`; under dinit this is the service's log buffer or
//! `logfile`) and optionally `SLINTCON_LOG_FILE`. Panics are logged too.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

use log::{LevelFilter, Log, Metadata, Record};

struct Logger {
    stderr: env_logger::Logger,
    file: Option<Mutex<File>>,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.stderr.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        self.stderr.log(record);
        let line = format!("{}: {}", record.target(), record.args());
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = writeln!(file, "[{}] {line}", record.level());
        }
    }

    fn flush(&self) {
        self.stderr.flush();
    }
}

pub fn init() {
    // Warn by default; `RUST_LOG` (e.g. `slintcon=debug`) overrides it.
    let stderr = env_logger::Builder::new()
        .filter_level(LevelFilter::Warn)
        .parse_default_env()
        .build();
    let max_level = stderr.filter();
    let file = std::env::var_os("SLINTCON_LOG_FILE")
        .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok())
        .map(Mutex::new);
    if log::set_boxed_logger(Box::new(Logger { stderr, file })).is_ok() {
        log::set_max_level(max_level);
    }

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        previous(info);
    }));
}
