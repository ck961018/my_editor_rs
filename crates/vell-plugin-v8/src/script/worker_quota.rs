use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// Reserved contract for Task 4 worker limits; consumed when Worker
// constructor dispatches spawns.
#[allow(dead_code)]
#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
pub(super) enum QuotaError {
    PerPluginExceeded,
    GlobalExceeded,
    DepthExceeded,
}

// Reserved contract for Task 4; the global quota is shared via Arc
// across the host and all spawned worker isolates.
#[allow(dead_code)]
#[derive(Debug)]
pub(super) struct WorkerQuota {
    per_plugin: usize,
    global: usize,
    depth: usize,
    global_count: AtomicUsize,
    per_plugin_counts: Mutex<HashMap<String, usize>>,
}

// Reserved contract for Task 4; Drop releases the quota slot.
#[allow(dead_code)]
#[derive(Debug)]
pub(super) struct QuotaHandle {
    plugin_id: String,
    quota: Arc<WorkerQuota>,
}

impl Drop for QuotaHandle {
    fn drop(&mut self) {
        self.quota.global_count.fetch_sub(1, Ordering::Relaxed);
        let mut counts = self.quota.per_plugin_counts.lock().unwrap();
        if let Some(c) = counts.get_mut(&self.plugin_id) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                counts.remove(&self.plugin_id);
            }
        }
    }
}

// Reserved contract for Task 4 worker limits.
#[allow(dead_code)]
impl WorkerQuota {
    pub(super) fn new(per_plugin: usize, global: usize, depth: usize) -> Self {
        Self {
            per_plugin,
            global,
            depth,
            global_count: AtomicUsize::new(0),
            per_plugin_counts: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn current_global(&self) -> usize {
        self.global_count.load(Ordering::Relaxed)
    }

    pub(super) fn try_acquire(
        self: &Arc<WorkerQuota>,
        plugin_id: &str,
        current_depth: usize,
    ) -> Result<QuotaHandle, QuotaError> {
        if current_depth >= self.depth {
            return Err(QuotaError::DepthExceeded);
        }
        let mut counts = self.per_plugin_counts.lock().unwrap();
        let pc = counts.entry(plugin_id.to_owned()).or_insert(0);
        if *pc >= self.per_plugin {
            return Err(QuotaError::PerPluginExceeded);
        }
        if self.current_global() >= self.global {
            return Err(QuotaError::GlobalExceeded);
        }
        *pc += 1;
        self.global_count.fetch_add(1, Ordering::Relaxed);
        Ok(QuotaHandle {
            plugin_id: plugin_id.to_owned(),
            quota: Arc::clone(self),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_acquire_succeeds_under_limit() {
        let quota = Arc::new(WorkerQuota::new(8, 32, 4));
        let h = quota.try_acquire("p1", 0).expect("under limit");
        assert_eq!(quota.current_global(), 1);
        drop(h);
        assert_eq!(quota.current_global(), 0);
    }

    #[test]
    fn try_acquire_fails_over_per_plugin() {
        let quota = Arc::new(WorkerQuota::new(2, 32, 4));
        let _h1 = quota.try_acquire("p1", 0).unwrap();
        let _h2 = quota.try_acquire("p1", 0).unwrap();
        let err = quota.try_acquire("p1", 0).unwrap_err();
        assert!(matches!(err, QuotaError::PerPluginExceeded));
    }

    #[test]
    fn try_acquire_fails_over_global() {
        let quota = Arc::new(WorkerQuota::new(100, 2, 4));
        let _h1 = quota.try_acquire("p1", 0).unwrap();
        let _h2 = quota.try_acquire("p2", 0).unwrap();
        let err = quota.try_acquire("p3", 0).unwrap_err();
        assert!(matches!(err, QuotaError::GlobalExceeded));
    }

    #[test]
    fn try_acquire_fails_over_depth() {
        let quota = Arc::new(WorkerQuota::new(100, 100, 2));
        let _h1 = quota.try_acquire("p1", 0).unwrap();
        let _h2 = quota.try_acquire("p1", 1).unwrap();
        let err = quota.try_acquire("p1", 2).unwrap_err();
        assert!(matches!(err, QuotaError::DepthExceeded));
    }
}
