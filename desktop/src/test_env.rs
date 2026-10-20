//! Tests that set process-wide env vars (`SHELBI_HOME`) hold this lock, so
//! they run one at a time.

use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Hold the env until the guard drops (a panicked holder doesn't wedge it).
pub fn lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}
