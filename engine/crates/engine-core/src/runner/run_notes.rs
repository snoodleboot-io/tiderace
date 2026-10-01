//! What a run has to say beside its results — the memory sizing it applied, a cache it could
//! not save — returned to the caller, which decides where it goes (TID-115). The runner used to
//! print these to stderr itself, from inside the library.

/// The notes a run collected, in order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RunNotes {
    pub lines: Vec<String>,
}

impl RunNotes {
    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// A run's results and its notes.
#[derive(Debug, Default, Clone)]
pub struct RunOutcome {
    pub results: Vec<crate::domain::TestResult>,
    pub notes: RunNotes,
}
