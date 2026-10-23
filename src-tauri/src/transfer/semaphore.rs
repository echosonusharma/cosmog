use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;

/// Resize-down records only what it reclaimed, else a later resize-up grows past the limit.
pub(super) struct ResizableSemaphore {
    sem: Arc<Semaphore>,
    current: Arc<Mutex<usize>>,
}

impl ResizableSemaphore {
    pub(super) fn new(n: usize) -> Self {
        let n = n.max(1);
        Self {
            sem: Arc::new(Semaphore::new(n)),
            current: Arc::new(Mutex::new(n)),
        }
    }

    pub(super) async fn acquire(&self) -> Result<tokio::sync::SemaphorePermit<'_>, tokio::sync::AcquireError> {
        self.sem.acquire().await
    }

    #[cfg(test)]
    pub(super) fn available(&self) -> usize {
        self.sem.available_permits()
    }

    pub(super) fn resize(&self, new_size: usize) {
        let new_size = new_size.max(1);
        let mut current = self.current.lock().unwrap();
        let old = *current;
        if new_size > old {
            self.sem.add_permits(new_size - old);
            *current = new_size;
        } else if new_size < old {
            let to_remove = old - new_size;
            let mut removed = 0;
            while removed < to_remove {
                match self.sem.try_acquire() {
                    Ok(permit) => {
                        permit.forget();
                        removed += 1;
                    }
                    Err(_) => break,
                }
            }
            *current = old - removed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resize_down_then_up_never_exceeds_limit() {
        let sem = ResizableSemaphore::new(4);
        let p1 = sem.acquire().await.unwrap();
        let p2 = sem.acquire().await.unwrap();
        let p3 = sem.acquire().await.unwrap();
        let p4 = sem.acquire().await.unwrap();
        assert_eq!(sem.available(), 0);

        // All permits in flight: nothing reclaimable, but capacity must not shrink.
        sem.resize(2);
        assert_eq!(sem.current.lock().unwrap().clone(), 4);

        drop((p1, p2, p3, p4));
        // Permits are back; resize-up must not add any on top of them.
        sem.resize(4);
        assert_eq!(sem.available(), 4);

        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(sem.acquire().await.unwrap());
        }
        assert!(
            sem.sem.try_acquire().is_err(),
            "capacity exceeded the limit after resize down+up"
        );

        drop(held);
        sem.resize(2);
        assert_eq!(sem.available(), 2);
    }
}
