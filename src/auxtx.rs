/// An auxiliary transaction that gets co-committed with the STM transaction,
/// unless the STM transaction is aborted or has to be retried, in which case
/// the auxiliary transaction is rolled back.
///
/// The auxiliary transaction can also signal that its is unable to be committed,
/// in which case the whole atomic transaction will be retried.
///
/// The database is not expected to return an error here, because of how `Transaction::commit` works.
/// If there is a failure that needs to be surfaced, at the moment the database would have to buffer
/// that error and return it on a subsequent operation, mapped to an `StmError::Abort`.
pub trait Aux {
    /// Commit the auxiliary transaction if the STM transaction did not detect any errors.
    /// The STM transaction is checked first, because committing the database involves IO
    /// and is expected to be slower.
    ///
    /// Return `false` if there are write conflicts in the persistent database itself,
    /// to cause a complete retry for the whole transaction.
    fn commit(self) -> bool;
    /// Rollback the auxiliary transaction if the STM transaction was aborted, or it's going to be retried.
    fn rollback(self);
}

/// Empty implementation for when we are not using any auxiliary transaction.
pub(crate) struct NoAux;

impl Aux for NoAux {
    fn commit(self) -> bool {
        true
    }
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
        abort, atomically, atomically_aux, atomically_or_err_aux, retry, test::TestError1, TVar,
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
        fn commit(mut self) -> bool {
            let mut guard = self.db.counter.lock().unwrap();
            *guard = self.counter;
            self.finished = true;
            true
        }
        fn rollback(mut self) {
            self.finished = true;
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

        atomically_or_err_aux::<_, TestError1, _, _, _>(
            || db.begin(),
            |atx| {
                atx.counter = 1;
                abort(TestError1)?;
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
            atomically_aux(
                || dbc.begin(),
                |atx| {
                    let a = tac.read()?;
                    atx.counter += *a;
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
        assert_eq!(db.counter(), 1);

        // Writing a value to `ta` will trigger the retry.
        atomically(|| ta.write(10)).await;
        handle.await.unwrap();

        assert_eq!(db.counter(), 11);
    }
}
