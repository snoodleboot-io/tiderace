use crate::hooks::{Hook, HookEvent, Priority};

/// The Rust-native hook host (design 12, ADR-E001): registers [`Hook`] plugins and dispatches typed
/// [`HookEvent`]s to them by **static method call** over a registered `Vec` — none of `pluggy`'s
/// per-call, Python-level dispatch tax. Ordering (`Priority`, then stable registration order) is
/// resolved **once**, the first time an event is dispatched.
#[derive(Default)]
pub struct HookHost {
    hooks: Vec<(Priority, Box<dyn Hook>)>,
    resolved: bool,
}

impl HookHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin at `priority` (use [`Priority::NORMAL`] when unsure).
    pub fn register(&mut self, priority: Priority, hook: Box<dyn Hook>) {
        self.hooks.push((priority, hook));
        self.resolved = false;
    }

    /// Dispatch one event to every plugin in priority order. The order is resolved once and reused.
    pub fn dispatch(&mut self, event: &HookEvent<'_>) {
        if !self.resolved {
            // Stable sort by descending priority → higher priority first, ties keep insertion order.
            self.hooks.sort_by_key(|(p, _)| std::cmp::Reverse(*p));
            self.resolved = true;
        }
        for (_priority, hook) in &mut self.hooks {
            hook.handle(event);
        }
    }

    /// The number of registered plugins.
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// Whether no plugins are registered.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::domain::{NodeId, Outcome, RunReport, TestResult};

    /// A sample native plugin that records the events it observes into a shared log.
    struct Recorder {
        name: String,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl Hook for Recorder {
        fn handle(&mut self, event: &HookEvent<'_>) {
            let label = match event {
                HookEvent::SessionStart => "start".to_string(),
                HookEvent::CollectionDone { count } => format!("collected:{count}"),
                HookEvent::TestStart(node) => format!("test_start:{}", node.as_str()),
                HookEvent::TestFinish(r) => format!("test_finish:{}", r.node_id.as_str()),
                HookEvent::SessionFinish(rep) => format!("finish:{}", rep.total()),
            };
            self.log.borrow_mut().push(format!("{}:{label}", self.name));
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    #[test]
    fn dispatches_all_events_to_a_plugin_in_order() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut host = HookHost::new();
        host.register(
            Priority::NORMAL,
            Box::new(Recorder {
                name: "rec".into(),
                log: log.clone(),
            }),
        );

        let node = NodeId::new("t.py::a");
        let result = TestResult::new(node.clone(), Outcome::Passed, 1, "");
        let report = RunReport::new(vec![result.clone()]);

        host.dispatch(&HookEvent::SessionStart);
        host.dispatch(&HookEvent::CollectionDone { count: 1 });
        host.dispatch(&HookEvent::TestStart(&node));
        host.dispatch(&HookEvent::TestFinish(&result));
        host.dispatch(&HookEvent::SessionFinish(&report));

        assert_eq!(
            *log.borrow(),
            vec![
                "rec:start",
                "rec:collected:1",
                "rec:test_start:t.py::a",
                "rec:test_finish:t.py::a",
                "rec:finish:1",
            ]
        );
    }

    #[test]
    fn higher_priority_runs_first_ties_keep_registration_order() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut host = HookHost::new();
        // Registered low, normal, high — but dispatch must order high → normal → low.
        host.register(
            Priority::LOW,
            Box::new(Recorder {
                name: "low".into(),
                log: log.clone(),
            }),
        );
        host.register(
            Priority::NORMAL,
            Box::new(Recorder {
                name: "n1".into(),
                log: log.clone(),
            }),
        );
        host.register(
            Priority::NORMAL,
            Box::new(Recorder {
                name: "n2".into(),
                log: log.clone(),
            }),
        );
        host.register(
            Priority::HIGH,
            Box::new(Recorder {
                name: "high".into(),
                log: log.clone(),
            }),
        );

        host.dispatch(&HookEvent::SessionStart);

        assert_eq!(
            *log.borrow(),
            vec!["high:start", "n1:start", "n2:start", "low:start"],
            "high first; NORMAL ties keep registration order; low last"
        );
    }
}
