//! Diagnostics log at `%APPDATA%\GameLauncher\launcher.log`, recreated on every start, so
//! "it didn't work" can be traced to the exact step that failed.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static LOG: OnceLock<Mutex<File>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

pub fn init(dir: &Path) {
    START.get_or_init(Instant::now);
    match File::create(dir.join("launcher.log")) {
        Ok(file) => {
            let _ = LOG.set(Mutex::new(file));
        }
        Err(e) => eprintln!("[launcher] could not create launcher.log: {e}"),
    }
}

pub fn log(line: &str) {
    let seconds = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    let line = format!("[{seconds:8.2}] {line}");
    eprintln!("{line}");
    if let Some(file) = LOG.get()
        && let Ok(mut file) = file.lock()
    {
        let _ = writeln!(file, "{line}");
    }
}
