use crate::auxtx::*;
use crate::vars::{LVar, TVar, VVar, ID};
use crate::version::{current_version, next_version, Version};
use crate::{StmError, StmResult};
use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

thread_local! {
  /// Using a thread local transaction for easier syntax than
  /// if we had to pass around the transaction everywhere.
  /// There is a 2x performance penalty for this.
  pub(crate) static TX: RefCell<Option<Transaction>> = RefCell::new(None);
}

/// Borrow the thread local transaction and pass it to a function.
pub fn with_tx<F, T>(f: F) -> T
where
    F: FnOnce(&mut Transaction) -> T,
{
    TX.with(|tref| {
        let mut tx = tref.borrow_mut();
        match tx.as_mut() {
            None => panic!("Not running in an atomic transaction!"),
            Some(tx) => f(tx),
        }
    })
}

#[derive(Clone)]
pub struct Transaction {
    /// Version of the STM at the start of the transaction.
    /// When we commit, it's going to be done with the version
    /// at the end of the transaction, so that we can detect
    /// if another transaction committed a write-only value
    /// after we have started.
    version: Version,
    /// The local store of the transaction will only be accessed by a single thread,
    /// so it doesn't need to be wrapped in an `Arc`. We have exclusive access through
    /// the mutable reference to the transaction.
    ///
    /// BTreeMap is used to keep the keys sorted:
    /// It will prevent deadlocks when we try to obtain all the locks later.
    pub(crate) log: BTreeMap<ID, LVar>,
    /// A read-only transaction can be committed without taking out locks a second time.
    has_writes: bool,
    /// Time to wait during retries if no variables have been
    /// read by the transaction. This would be strange, but
    /// it's better than blocking a thread forever.
    pub empty_retry_wait_timeout: Duration,
}

impl Transaction {
    pub(crate) fn new() -> Transaction {
        Transaction {
            version: current_version(),
            log: BTreeMap::new(),
            has_writes: false,
            empty_retry_wait_timeout: Duration::from_secs(60),
        }
    }

    /// Read a value from the local store, or the STM system.
    /// If it has changed since the beginning of the transaction,
    /// return a failure immediately, because we are not reading
    /// a consistent snapshot.
    pub fn read<T: Any + Sync + Send>(&mut self, tvar: &TVar<T>) -> StmResult<Arc<T>> {
        match self.log.get(&tvar.id) {
            Some(lvar) => Ok(lvar.vvar.downcast()),
            None => {
                let guard = tvar.svar.vvar.read();
                if guard.version > self.version {
                    // The TVar has been written to since we started this transaction.
                    // There is no point carrying on with the rest of it, but we can retry.
                    Err(StmError::Failure)
                } else {
                    self.log.insert(
                        tvar.id,
                        LVar {
                            svar: tvar.svar.clone(),
                            vvar: guard.clone(),
                            read: true,
                            write: false,
                        },
                    );
                    Ok(guard.downcast())
                }
            }
        }
    }

    /// Write a value into the local store. If it has not been read
    /// before, just insert it with the version at the start of the
    /// transaction.
    pub fn write<T: Any + Sync + Send>(&mut self, tvar: &TVar<T>, value: T) -> StmResult<()> {
        match self.log.get_mut(&tvar.id) {
            Some(lvar) => {
                lvar.write = true;
                lvar.vvar.value = Arc::new(value);
            }
            None => {
                self.log.insert(
                    tvar.id,
                    LVar {
                        svar: tvar.svar.clone(),
                        vvar: VVar {
                            // So we didn't bother reading the value before attempting to overwrite,
                            // and therefore we don't know what version it had. Let's use the maximum
                            // it could have had at the time of the transaction.
                            version: self.version,
                            value: Arc::new(value),
                        },
                        read: false,
                        write: true,
                    },
                );
            }
        };
        self.has_writes = true;
        Ok(())
    }

    /// In a critical section, check that every variable we have read/written
    /// hasn't got a higher version number in the committed store.
    /// If so, add all written values to the store.
    ///
    /// This is also the place where the auxiliary transaction can be committed,
    /// while the locks are being held, so there's no gap where the two datasets
    /// are inconsistent.
    pub(crate) fn commit<X: Aux>(&self, atx: X) -> Option<Version> {
        let commit = |atx: X, inc: bool| {
            if atx.commit() {
                // Incrementing after locks are taken; if it only differs by one, no other transaction took place;
                // but we already checked for conflicts, it looks like at this point there's no way to use this info.
                let version = if inc { next_version() } else { self.version };
                Some(version)
            } else {
                None
            }
        };

        let rollback = |atx: X| {
            atx.rollback();
            None
        };

        // If there were no writes, then the read would have already detected conflicts when their
        // values were retrieved. We can go ahead and just return without locking again.
        if !self.has_writes {
            return commit(atx, false);
        }

        // Acquire write locks to all values written in the transaction, read locks for the rest,
        // but do this in the deterministic order of IDs to avoid deadlocks.
        let mut write_locks = Vec::new();
        let mut read_locks = Vec::new();

        for (_, lvar) in self.log.iter() {
            if lvar.write {
                let lock = lvar.svar.vvar.write();
                if lock.version > lvar.vvar.version {
                    return rollback(atx);
                }
                write_locks.push((lvar, lock));
            } else {
                let lock = lvar.svar.vvar.read();
                if lock.version > lvar.vvar.version {
                    return rollback(atx);
                }
                read_locks.push(lock);
            }
        }

        // See if the auxiliary transaction can be committed first, while we hold all the in-memory locks.
        if let Some(commit_version) = commit(atx, true) {
            for (lvar, mut lock) in write_locks {
                lock.version = commit_version;
                lock.value = lvar.vvar.value.clone();
            }
            return Some(commit_version);
        } else {
            return None;
        }
    }

    /// For each variable that the transaction has read, subscribe to future
    /// change notifications, then park this thread.
    pub(crate) async fn wait(self) {
        let read_log = self
            .log
            .iter()
            .filter(|(_, lvar)| lvar.read)
            .collect::<Vec<_>>();

        // If there are no variables subscribed to then just wait a bit.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        // Register in the wait queues.
        if !read_log.is_empty() {
            let locks = read_log
                .iter()
                .map(|(_, lvar)| lvar.svar.queue.lock())
                .collect::<Vec<_>>();

            // Don't register if a producer already committed changes by the time we got here.
            let has_updates = locks
                .iter()
                .any(|lock| lock.last_written_version > self.version);

            if has_updates {
                return;
            }

            for mut lock in locks {
                lock.add(tx.clone())
            }
        }

        // Drop the original so we don't keep the `rx` open if there are no senders.
        drop(tx);

        if read_log.is_empty() {
            tokio::time::sleep(self.empty_retry_wait_timeout).await;
        } else {
            // Wait for a true signal; ignore pruning attempts from over subscribed variables.
            while let Some(false) = rx.recv().await {}
            // NOTE: Here we could deregister from the wait queues, but that would require
            // taking out the locks again. Since the notifiers take locks too to increment
            // the version, let them do the clean up. One side effect is that a thread
            // may be unparked some variable that changes less frequently, which still
            // remembers it with an obsolete notification flag. In this case the thread
            // will just park itself again.
        }
    }

    /// Unpark any thread waiting on any of the modified `TVar`s.
    pub(crate) fn notify(self, commit_version: Version) {
        if !self.has_writes {
            return;
        }

        let write_log = self
            .log
            .iter()
            .filter(|(_, lvar)| lvar.write)
            .collect::<Vec<_>>();

        let locks = write_log
            .iter()
            .map(|(_, lvar)| lvar.svar.queue.lock())
            .collect::<Vec<_>>();

        for mut lock in locks {
            lock.notify_all(commit_version);
        }
    }
}
