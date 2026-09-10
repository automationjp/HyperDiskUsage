use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};

use rmcp::ErrorData;
use tokio::{sync::Semaphore, task::JoinHandle};

pub(super) fn capacity() -> Arc<Semaphore> {
    static CAPACITY: OnceLock<Arc<Semaphore>> = OnceLock::new();
    CAPACITY.get_or_init(|| Arc::new(Semaphore::new(2))).clone()
}

pub(super) struct Worker<T> {
    pub(super) handle: JoinHandle<T>,
    cancel: Arc<AtomicBool>,
}

impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl<T> Worker<T> {
    pub(super) async fn cancel_and_wait(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // A blocked filesystem call may not observe cancellation promptly. Its
        // permit stays in the closure even if this bounded wait expires.
        let _ = tokio::time::timeout(Duration::from_millis(250), &mut self.handle).await;
    }
}

pub(super) async fn start<F, T>(
    capacity: Arc<Semaphore>,
    cancelled: impl Future<Output = ()>,
    f: F,
) -> Result<Worker<T>, ErrorData>
where
    F: FnOnce(Arc<AtomicBool>) -> T + Send + 'static,
    T: Send + 'static,
{
    let permit = tokio::select! {
        biased;
        _ = cancelled => return Err(ErrorData::internal_error("scan cancelled", None)),
        permit = capacity.acquire_owned() => permit
            .map_err(|_| ErrorData::internal_error("scan capacity closed", None))?,
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let handle = tokio::task::spawn_blocking(move || {
        // The worker, not the request future, owns admission until actual exit.
        let _permit = permit;
        f(worker_cancel)
    });
    Ok(Worker { handle, cancel })
}

#[cfg(test)]
mod tests {
    use std::{future::pending, sync::mpsc, task::Poll};

    use tokio::sync::oneshot;

    use super::*;

    async fn assert_pending<F: Future>(mut future: std::pin::Pin<&mut F>) {
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    async fn blocked(capacity: Arc<Semaphore>) -> (Worker<()>, mpsc::Sender<()>) {
        let (started, ready) = oneshot::channel();
        let (release, wait) = mpsc::channel();
        let worker = start(capacity, pending(), move |_| {
            started.send(()).unwrap();
            let _ = wait.recv();
        })
        .await
        .unwrap();
        ready.await.unwrap();
        (worker, release)
    }

    #[tokio::test]
    async fn caps_active_workers_and_returns_success() {
        let capacity = Arc::new(Semaphore::new(2));
        let (mut first, release_first) = blocked(capacity.clone()).await;
        let (mut second, release_second) = blocked(capacity.clone()).await;
        let queued = start(capacity.clone(), pending(), |_| 42);
        tokio::pin!(queued);
        assert_pending(queued.as_mut()).await;
        assert_eq!(capacity.available_permits(), 0);
        release_first.send(()).unwrap();
        (&mut first.handle).await.unwrap();
        let mut third = queued.await.unwrap();
        assert_eq!((&mut third.handle).await.unwrap(), 42);
        release_second.send(()).unwrap();
        (&mut second.handle).await.unwrap();
        assert_eq!(capacity.available_permits(), 2);
    }

    #[tokio::test]
    async fn dropped_handler_keeps_capacity_until_worker_exits() {
        let capacity = Arc::new(Semaphore::new(1));
        let (worker, release) = blocked(capacity.clone()).await;
        let cancel = worker.cancel.clone();
        drop(worker);
        assert!(cancel.load(Ordering::Relaxed));
        let queued = start(capacity.clone(), pending(), |_| 42);
        tokio::pin!(queued);
        assert_pending(queued.as_mut()).await;
        release.send(()).unwrap();
        let mut next = queued.await.unwrap();
        assert_eq!((&mut next.handle).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn explicit_cancel_waits_boundedly_but_keeps_capacity() {
        let capacity = Arc::new(Semaphore::new(1));
        let (mut worker, release) = blocked(capacity.clone()).await;
        worker.cancel_and_wait().await;
        assert!(worker.cancel.load(Ordering::Relaxed));
        assert!(!worker.handle.is_finished());
        assert_eq!(capacity.available_permits(), 0);
        release.send(()).unwrap();
        (&mut worker.handle).await.unwrap();
        assert_eq!(capacity.available_permits(), 1);
    }

    #[tokio::test]
    async fn queued_cancellation_does_not_start_worker() {
        let capacity = Arc::new(Semaphore::new(1));
        let (mut worker, release) = blocked(capacity.clone()).await;
        let (cancel, cancelled) = oneshot::channel();
        let started = Arc::new(AtomicBool::new(false));
        let started_in_worker = started.clone();
        let queued = start(
            capacity,
            async {
                let _ = cancelled.await;
            },
            move |_| {
                started_in_worker.store(true, Ordering::Relaxed);
            },
        );
        tokio::pin!(queued);
        assert_pending(queued.as_mut()).await;
        cancel.send(()).unwrap();
        assert!(queued.await.is_err());
        release.send(()).unwrap();
        (&mut worker.handle).await.unwrap();
        assert!(!started.load(Ordering::Relaxed));
    }
}
