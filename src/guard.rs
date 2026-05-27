/// A Drop-based rollback guard.
///
/// Register cleanup closures with `push`. When the guard is dropped,
/// all registered closures are called in LIFO order unless `commit` has been
/// called. Any errors from cleanup closures are logged but never panic.
pub struct RollbackGuard {
    committed: bool,
    actions: Vec<Box<dyn FnOnce() + Send>>,
}

impl RollbackGuard {
    pub fn new() -> Self {
        Self {
            committed: false,
            actions: Vec::new(),
        }
    }

    /// Register a cleanup action. Actions fire in reverse order on rollback.
    pub fn push<F: FnOnce() + Send + 'static>(&mut self, f: F) {
        self.actions.push(Box::new(f));
    }

    /// Commit: disarm the guard so no cleanup happens on drop.
    pub fn commit(mut self) {
        self.committed = true;
        // self.actions is dropped without being called
    }
}

impl Drop for RollbackGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // Fire in LIFO order
        while let Some(action) = self.actions.pop() {
            // Catch panics from cleanup code
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
            if let Err(e) = result {
                eprintln!("[guard] rollback action panicked: {e:?}");
            }
        }
    }
}

impl Default for RollbackGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn rollback_fires_on_drop() {
        let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let log2 = log.clone();
            let log3 = log.clone();
            let mut guard = RollbackGuard::new();
            guard.push(move || log2.lock().unwrap().push("first"));
            guard.push(move || log3.lock().unwrap().push("second"));
            // drop without commit
        }
        let entries = log.lock().unwrap().clone();
        // LIFO order
        assert_eq!(entries, vec!["second", "first"]);
    }

    #[test]
    fn commit_prevents_rollback() {
        let fired = Arc::new(Mutex::new(false));
        let fired2 = fired.clone();
        let mut guard = RollbackGuard::new();
        guard.push(move || *fired2.lock().unwrap() = true);
        guard.commit();
        assert!(!*fired.lock().unwrap());
    }
}
