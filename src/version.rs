use std::sync::atomic::{AtomicU64, Ordering};

/// MVCC version.
pub type Version = u64;

/// Global vector clock.
static VERSION: AtomicU64 = AtomicU64::new(0);

/// Increment the global vector clock and return the new value.
pub fn next_version() -> Version {
    let prev = VERSION.fetch_add(1, Ordering::Relaxed);
    prev + 1
}

/// Get the current version of the vector clock.
pub fn current_version() -> Version {
    VERSION.load(Ordering::Relaxed)
}
