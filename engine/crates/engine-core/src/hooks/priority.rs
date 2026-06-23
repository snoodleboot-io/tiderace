/// Hook dispatch priority (design 12). Higher runs **earlier**; ties keep stable registration order.
/// Resolved **once** at registration (sorted in the host), unlike pytest's per-call `tryfirst`/
/// `trylast`/`hookwrapper` ordering computed in Python on every hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority(pub i32);

impl Priority {
    /// Runs before normal plugins (pytest's `tryfirst`).
    pub const HIGH: Priority = Priority(100);
    /// Default.
    pub const NORMAL: Priority = Priority(0);
    /// Runs after normal plugins (pytest's `trylast`).
    pub const LOW: Priority = Priority(-100);
}

impl Default for Priority {
    fn default() -> Self {
        Priority::NORMAL
    }
}
