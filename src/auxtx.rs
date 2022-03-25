/// An auxiliary transaction that gets co-committed with the STM transaction,
/// unless the STM transaction is aborted or has to be retried, in which case
/// the auxiliary transaction is rolled back.
///
/// One potential use case is to model the account state as a Sparse Merkle trie
/// backed by a key-value store, and accumulate writes during the execution of
/// a block in-memory, then when the commit comes, instead of flushing the changes
/// to disk, we could keep the changeset in memory until the block is finalised.
///
/// To manage the tree of changesets, we can create new instances of the `Aux`
/// construct before passing it to the block execution, where the instance
/// would be the new leaf, and `commit` just writes to its in-memory staging area.
/// This way we don't have to return any value from `commit`, to represent the
/// changeset.
///
/// When a block is finalised, we can take the root of the changeset tree and
/// apply it for real on the database, and discard any orphans.
pub trait Aux {
    type Prepared: AuxPrepared;
    /// Prepare for commit. If the transaction cannot be committed, roll it back and return `None`,
    /// in which case the STM transaction will be retried. This is an optimisation to delay taking
    /// out any exclusive locks.
    fn prepare(self) -> Option<Self::Prepared>;
    /// Rollback the auxiliary transaction if the STM transaction was aborted, or it's going to be retried.
    fn rollback(self);
}

pub trait AuxPrepared {
    /// Commit the auxiliary transaction if the STM transaction succeeded.
    fn commit(self);
    /// Rollback the auxiliary transaction if the STM transaction is going to be retried.
    fn rollback(self);
}

/// Empty implementation for when we are not using any auxiliary transaction.
pub(crate) struct NoAux;

impl Aux for NoAux {
    type Prepared = Self;
    fn prepare(self) -> Option<Self> {
        Some(self)
    }
    fn rollback(self) {}
}
impl AuxPrepared for NoAux {
    fn commit(self) {}
    fn rollback(self) {}
}

#[cfg(test)]
mod test {
    use std::{
        sync::{Arc, Mutex},
        thread,
    };

    use crate::auxtx::*;
    use crate::{
        abort, atomically, atomically_aux, atomically_or_err_aux, retry, test::TestError, TVar,
    };

    #[derive(Clone)]
    struct TestAuxDb {
        counter: Arc<Mutex<i32>>,
    }
    struct TestAuxTx<'a> {
        db: &'a TestAuxDb,
        counter: i32,
        finished: bool,
    }

    impl TestAuxDb {
        fn begin(&self) -> TestAuxTx {
            let guard = self.counter.lock().unwrap();
            TestAuxTx {
                db: self,
                counter: *guard,
                finished: false,
            }
        }
    }

    impl<'a> Aux for TestAuxTx<'a> {
        type Prepared = Self;
        fn prepare(self) -> Option<Self> {
            Some(self)
        }

        fn rollback(mut self) {
            self.finished = true;
        }
    }

    impl<'a> AuxPrepared for TestAuxTx<'a> {
        fn commit(mut self) {
            let mut guard = self.db.counter.lock().unwrap();
            *guard = self.counter;
            self.finished = true;
        }

        fn rollback(self) {
            Aux::rollback(self)
        }
    }

    impl Drop for TestAuxTx<'_> {
        fn drop(&mut self) {
            if !self.finished && !thread::panicking() {
                panic!("Transaction prematurely dropped. Must call `.commit()` or `.rollback()`.");
            }
        }
    }

    impl TestAuxDb {
        fn new() -> TestAuxDb {
            TestAuxDb {
                counter: Arc::new(Mutex::new(0)),
            }
        }

        fn counter(&self) -> i32 {
            *self.counter.lock().unwrap()
        }
    }

    #[tokio::test]
    async fn aux_commit_rollback() {
        let db = TestAuxDb::new();

        atomically_or_err_aux(
            || db.begin(),
            |atx| {
                atx.counter = 1;
                abort(TestError)?;
                Ok(())
            },
        )
        .await
        .expect_err("Should be aborted");

        assert_eq!(db.counter(), 0);

        atomically_aux(
            || db.begin(),
            |atx| {
                atx.counter = 1;
                Ok(())
            },
        )
        .await;

        assert_eq!(db.counter(), 1);

        let ta = TVar::new(42);
        let dbc = db.clone();
        let tac = ta.clone();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            let _ = atomically_aux(
                || dbc.begin(),
                |atx| {
                    let a = tac.read()?;
                    atx.counter = atx.counter + *a;
                    if *a == 42 {
                        // Signal that we are entering the retry.
                        sender.send(()).unwrap();
                        retry()?;
                    }
                    Ok(())
                },
            )
            .await;
        });

        let _ = receiver.recv().await;
        atomically(|| ta.write(10)).await;
        handle.await.unwrap();

        assert_eq!(db.counter(), 11);
    }
}
