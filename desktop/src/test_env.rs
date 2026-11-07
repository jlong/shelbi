//! Tests that set process-wide env vars (`SHELBI_HOME`) hold this lock, so
//! they run one at a time.

use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Hold the env until the guard drops (a panicked holder doesn't wedge it).
pub fn lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Every thread in this binary falls back to one throwaway home, mounted before
/// main because a resolve runs from the first test on. A window's fixtures start
/// the same background jobs a real window does, and those outlive the test that
/// started them. A test about specific state still mounts its own under [`lock`].
#[used]
#[link_section = "__DATA,__mod_init_func"]
static MOUNT_SHARED_HOME: extern "C" fn() = {
    extern "C" fn mount() {
        shelbi_state::mount_shared_test_home();
    }
    mount
};
