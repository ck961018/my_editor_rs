use std::future::Future;

use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[derive(Debug)]
pub(crate) struct AppTasks {
    cancel: CancellationToken,
    detached_tasks: TaskTracker,
    critical_tasks: TaskTracker,
}

impl AppTasks {
    pub(crate) fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            detached_tasks: TaskTracker::new(),
            critical_tasks: TaskTracker::new(),
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub(crate) async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }

    // 预留：detached 任务（语法解析/搜索预计算等）超出当前范围，仅 tasks.rs 测试覆盖。
    #[allow(dead_code)]
    pub(crate) fn spawn_detached<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.detached_tasks.spawn(task);
    }

    pub(crate) fn spawn_critical<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.critical_tasks.spawn(task);
    }

    pub(crate) fn close_detached(&self) {
        self.detached_tasks.close();
    }

    pub(crate) fn close_critical(&self) {
        self.critical_tasks.close();
    }

    pub(crate) async fn wait_critical(&self) {
        self.critical_tasks.wait().await;
    }
}

impl Default for AppTasks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test(flavor = "multi_thread")]
    async fn wait_critical_waits_for_critical_task() {
        let tasks = AppTasks::new();
        let (tx, rx) = oneshot::channel();
        tasks.spawn_critical(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            tx.send(()).unwrap();
        });

        tasks.close_critical();
        tasks.wait_critical().await;

        assert!(rx.await.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn detached_tasks_do_not_block_waiting_for_critical_tasks() {
        let tasks = AppTasks::new();
        tasks.spawn_detached(async {
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        tasks.close_detached();
        tasks.close_critical();

        let result = tokio::time::timeout(Duration::from_millis(50), tasks.wait_critical()).await;
        assert!(result.is_ok());
    }
}
