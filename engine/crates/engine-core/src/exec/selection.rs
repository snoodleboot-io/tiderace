//! What a run selects — `-k`, `-m`, `--strict-markers` — as a value that travels with a request
//! (TID-90). Platform-neutral: the daemon and the CLI name it on every target, the warm pool that
//! applies it exists on Unix only.

/// What a run selects, applied by each worker forked off a warm image before it serves (TID-90):
/// the shim reads `-k` / `-m` / `--strict-markers` from its environment at start-up, and a
/// persistent image was started without this run's. `None` fields keep what the image has
/// (the project's own `addopts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Selection {
    pub keyword: Option<String>,
    pub marker: Option<String>,
    pub strict_markers: bool,
}

impl Selection {
    /// Whether this selection narrows anything at all.
    pub fn is_empty(&self) -> bool {
        self.keyword.is_none() && self.marker.is_none() && !self.strict_markers
    }
}
