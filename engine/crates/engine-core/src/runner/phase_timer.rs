//! Where a run's time goes, phase by phase, on stderr when `TIDERACE_TIMING=1`; silent otherwise.
//! The daemon, the CLI and the pool each had their own spelling of this (TID-91's shim start-up
//! timer is the Python-side counterpart).

use std::time::Instant;

/// One named sequence of phases. `mark` prints the time since the previous mark and since the
/// start, prefixed by the program and the sequence name.
pub struct PhaseTimer {
    on: bool,
    program: &'static str,
    name: &'static str,
    started: Instant,
    last: Instant,
}

impl PhaseTimer {
    /// Start timing; prints only when `TIDERACE_TIMING` is set in the environment.
    pub fn start(program: &'static str, name: &'static str) -> Self {
        Self::with(std::env::var_os("TIDERACE_TIMING").is_some(), program, name)
    }

    /// Start timing with the printing decided by the caller.
    pub fn with(on: bool, program: &'static str, name: &'static str) -> Self {
        let now = Instant::now();
        Self {
            on,
            program,
            name,
            started: now,
            last: now,
        }
    }

    /// Whether marks print.
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Time since the previous mark.
    pub fn since_last(&self) -> std::time::Duration {
        self.last.elapsed()
    }

    /// End a phase: print its duration and the elapsed total, and start the next.
    pub fn mark(&mut self, label: &str) {
        let now = Instant::now();
        if self.on {
            eprintln!(
                "{}: timing: {}: {label} {}ms (at {}ms)",
                self.program,
                self.name,
                now.duration_since(self.last).as_millis(),
                now.duration_since(self.started).as_millis()
            );
        }
        self.last = now;
    }
}
