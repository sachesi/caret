//! How long frames take and how soon typed keys come back on screen, written to the file
//! `CARET_TIMINGS` names for the benchmark in `bench/`. Nothing is measured without it.
//!
//! Each line is one of
//!
//! ```text
//! frame BUILD RENDER            microseconds making the frame's quads, recording and
//!                               submitting its drawing
//! echo LATENCY                  microseconds from a key to the first frame showing output
//!                               that arrived after it
//! ```

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

struct Log {
    out: BufWriter<File>,
    /// The first key not yet answered by output, and whether output has come since.
    key: Option<(Instant, bool)>,
}

static LOG: LazyLock<Option<Mutex<Log>>> = LazyLock::new(|| {
    let path = std::env::var_os("CARET_TIMINGS").filter(|path| !path.is_empty())?;
    match File::create(&path) {
        Ok(file) => Some(Mutex::new(Log {
            out: BufWriter::new(file),
            key: None,
        })),
        Err(error) => {
            tracing::warn!("writing timings to {}: {error}", path.to_string_lossy());
            None
        }
    }
});

fn with(f: impl FnOnce(&mut Log)) {
    if let Some(log) = LOG.as_ref()
        && let Ok(mut log) = log.lock()
    {
        f(&mut log);
    }
}

pub fn enabled() -> bool {
    LOG.is_some()
}

/// A key the user typed went to the program.
pub fn key() {
    with(|log| {
        log.key.get_or_insert((Instant::now(), false));
    });
}

/// The program wrote something.
pub fn output() {
    with(|log| {
        if let Some((_, answered)) = &mut log.key {
            *answered = true;
        }
    });
}

pub fn frame(build: Duration, render: Duration) {
    with(|log| {
        let _ = writeln!(
            log.out,
            "frame {} {}",
            build.as_micros(),
            render.as_micros()
        );
        if let Some((typed, true)) = log.key {
            log.key = None;
            let _ = writeln!(log.out, "echo {}", typed.elapsed().as_micros());
        }
        let _ = log.out.flush();
    });
}
