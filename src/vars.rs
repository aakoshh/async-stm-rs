use parking_lot::{Mutex, RwLock};

use crate::{transaction::with_tx, version::Version, StmResult};
use std::{
    any::Any,
    marker::PhantomData,
    mem,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

/// Unique ID for a `TVar`.
pub type ID = u64;

/// The value can be read by many threads, so it has to be tracked by an `Arc`.
/// Keeping it dynamic because trying to make `LVar` generic turned out to be
/// a bit of a nightmare.
type DynValue = Arc<dyn Any + Send + Sync>;

/// A versioned value. It will only be accessed through a transaction and a `TVar`.
#[derive(Clone)]
pub struct VVar {
    pub version: Version,
    pub value: DynValue,
}

impl VVar {
    /// Perform a downcast on a var. Returns an `Arc` that tracks when that variable
    /// will go out of scope. This avoids cloning on reads, if the value needs to be
    /// mutated then it can be cloned after being read.
    pub fn downcast<T: Any + Sync + Send>(&self) -> Arc<T> {
        match self.value.clone().downcast::<T>() {
            Ok(s) => s,
            Err(_) => unreachable!("TVar has wrong type"),
        }
    }
}

/// Using a channel to wake up tasks when a `TVar` they read changed.
type Signaler = tokio::sync::mpsc::UnboundedSender<()>;

pub struct WaitQueue {
    /// Store the last version which was written to avoid race condition where the notification
    /// happens before the waiters would subscribe and then there's no further event that would
    /// unpark them, causing them to wait forever or until they time out.
    ///
    /// This can happen if the order of events on thread A and B are:
    /// 1. `atomically` on A returns `Retry`
    /// 2. `commit` on B updates the versions
    /// 3. `notify` on B finds nobody in the wait queues
    /// 4. `wait` on A adds itself to the wait queues
    ///
    /// By having `notify` update the `last_written_version` we make sure that `wait` sees it.
    pub last_written_version: Version,

    /// Signalers for tasks waiting for the `TVar` to get an update.
    waiting: Vec<Signaler>,
}

impl WaitQueue {
    pub fn new() -> Self {
        WaitQueue {
            last_written_version: Default::default(),
            waiting: Vec::new(),
        }
    }

    pub fn add(&mut self, s: Signaler) {
        self.waiting.push(s)
    }

    pub fn notify_all(&mut self, commit_version: Version) {
        self.last_written_version = commit_version;

        if self.waiting.is_empty() {
            return;
        }

        let waiting = mem::take(&mut self.waiting);

        for tx in waiting {
            let _ = tx.send(());
        }
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Sync variable, hold the committed value and the waiting threads.
pub struct SVar {
    pub vvar: RwLock<VVar>,
    pub queue: Mutex<WaitQueue>,
}

/// A variable in the transaction log that remembers if it has been read and/or written to.
#[derive(Clone)]
pub struct LVar {
    // Hold on the original that we need to commit to.
    pub svar: Arc<SVar>,
    // Hold on to the value as it was read or written for MVCC comparison.
    pub vvar: VVar,
    /// Remember reads; these are the variables we need to watch if we retry.
    pub read: bool,
    /// Remember writes; these are the variables that need to be stored at the
    /// end of the transaction, but they don't need to be watched if we retry.
    pub write: bool,
}

/// `TVar` is our handle to a variable, but reading and writing go through a transaction.
/// It also tracks which threads are waiting on it.
#[derive(Clone)]
pub struct TVar<T> {
    pub(crate) id: ID,
    pub(crate) svar: Arc<SVar>,
    phantom: PhantomData<T>,
}

impl<T: Any + Sync + Send + Clone> TVar<T> {
    /// Create a new `TVar`. The initial version is 0, so that if a
    /// `TVar` is created in the middle of a transaction it will
    /// not cause any MVCC conflict during the commit.
    pub fn new(value: T) -> TVar<T> {
        // This is shared between all `TVar`s.
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        TVar {
            id: COUNTER.fetch_add(1, Ordering::Relaxed),
            svar: Arc::new(SVar {
                vvar: RwLock::new(VVar {
                    version: Default::default(),
                    value: Arc::new(value),
                }),
                queue: Mutex::new(WaitQueue::default()),
            }),
            phantom: PhantomData,
        }
    }

    /// Read the value of the `TVar` as a clone, for subsequent modification. Only call this inside `atomically`.
    pub fn read_clone(&self) -> StmResult<T> {
        with_tx(|tx| tx.read(self).map(|r| r.as_ref().clone()))
    }

    /// Read the value of the `TVar`. Only call this inside `atomically`.
    pub fn read(&self) -> StmResult<Arc<T>> {
        with_tx(|tx| tx.read(self))
    }

    /// Replace the value of the `TVar`. Only call this inside `atomically`.
    pub fn write(&self, value: T) -> StmResult<()> {
        with_tx(move |tx| tx.write(self, value))
    }

    /// Apply an update on the value of the `TVar`. Only call this inside `atomically`.
    pub fn update<F>(&self, f: F) -> StmResult<()>
    where
        F: FnOnce(T) -> T,
    {
        let v = self.read()?;
        self.write(f(v.as_ref().clone()))
    }

    /// Apply an update on the value of the `TVar` and return a value. Only call this inside `atomically`.
    pub fn modify<F, R>(&self, f: F) -> StmResult<R>
    where
        F: FnOnce(T) -> (T, R),
    {
        let v = self.read()?;
        let (w, r) = f(v.as_ref().clone());
        self.write(w)?;
        Ok(r)
    }

    /// Replace the value of the `TVar` and return the previous value. Only call this inside `atomically`.
    pub fn replace(&self, value: T) -> StmResult<Arc<T>> {
        let v = self.read()?;
        self.write(value)?;
        Ok(v)
    }
}
