use std::time::Instant;

use crate::error::AppError;

/// Non-SDK failures only (no S3 code to classify); SDK errors go through classify_aws.
pub(super) fn s3_err<E: std::fmt::Display>(ctx: &str, e: E) -> AppError {
    AppError::S3(format!("{ctx}: {e}"))
}

pub(super) fn canceled(transfer_id: &str) -> AppError {
    AppError::Canceled(format!("transfer {transfer_id} canceled"))
}

/// Atomic nanoseconds keep `allow()` lock-free from every chunk callback.
pub(super) struct ProgressThrottle {
    last_ns: std::sync::atomic::AtomicU64,
    interval_ns: u64,
    epoch: Instant,
}

impl ProgressThrottle {
    pub(super) fn new(interval_ms: u64) -> Self {
        let interval_ns = interval_ms * 1_000_000;
        Self {
            // 0 = "never fired" sentinel; allow() fires immediately on first call.
            last_ns: std::sync::atomic::AtomicU64::new(0),
            interval_ns,
            epoch: Instant::now(),
        }
    }

    pub(super) fn allow(&self) -> bool {
        use std::sync::atomic::Ordering;
        // now_ns must never be 0 (the "never fired" sentinel).
        let now_ns = (self.epoch.elapsed().as_nanos() as u64).max(1);
        let last = self.last_ns.load(Ordering::Relaxed);
        if last == 0 || now_ns.saturating_sub(last) >= self.interval_ns {
            // CAS: one concurrent caller wins this tick.
            self.last_ns.compare_exchange(last, now_ns, Ordering::Relaxed, Ordering::Relaxed).is_ok()
        } else {
            false
        }
    }
}
