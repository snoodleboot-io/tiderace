//! Test discovery — finds tests WITHOUT importing Python (the engine owns discovery).

mod collector;
mod regex_collector;

pub use collector::Collector;
pub use regex_collector::RegexCollector;
