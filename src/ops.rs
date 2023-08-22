use std::mem;

use crate::{auxtx::*, StmControl};
use crate::{
    transaction::{with_tx, Transaction, TX},
    Stm, StmError, StmResult,
};

/// Abandon the transaction and retry after some of the variables read have changed.
pub fn retry<T>() -> Stm<T> {
    Err(StmControl::Retry)
}

/// Retry unless a given condition has been met.
pub fn guard(cond: bool) -> Stm<()> {
    if cond {
        Ok(())
    } else {
        retry()
    }
}

/// Abort the transaction with an error.
///
/// Use this with [atomically_or_err] and a method that returns [StmResult] instead of [Stm].
pub fn abort<T, E1, E2>(e: E1) -> StmResult<T, E2>
where
    E1: Into<E2>,
{
    Err(StmError::Abort(e.into()))
}

/// Run the first function; if it returns a `Retry`,
/// run the second function; if that too returns `Retry`
/// then combine the values they have read, so that
/// the overall retry will react to any change.
///
/// If they return `Failure` then just return that result,
/// since the transaction can be retried right now.
pub fn or<F, G, T>(f: F, g: G) -> Stm<T>
where
    F: FnOnce() -> Stm<T>,
    G: FnOnce() -> Stm<T>,
{
    let mut snapshot = with_tx(|tx| tx.clone());

    match f() {
        Err(StmControl::Retry) => {
            // Restore the original transaction state.
            with_tx(|tx| {
                mem::swap(tx, &mut snapshot);
            });

            match g() {
                retry @ Err(StmControl::Retry) =>
                // Add any variable read in the first attempt.
                {
                    with_tx(|tx| {
                        for (id, lvar) in snapshot.log.into_iter() {
                            match tx.log.get(&id) {
                                Some(lvar) if lvar.read => {}
                                _ => {
                                    tx.log.insert(id, lvar);
                                }
                            }
                        }
                    });
                    retry
                }
                other => other,
            }
        }
        other => other,
    }
}

/// Create a new transaction and run `f` until it returns a successful result and
/// can be committed without running into version conflicts.
///
/// Make sure `f` is free of any side effects, because it can be called repeatedly.
pub async fn atomically<T, F>(f: F) -> T
where
    F: Fn() -> Stm<T>,
{
    atomically_aux(|| NoAux, |_| f()).await
}

/// Like [atomically], but this version also takes an auxiliary transaction system
/// that gets committed or rolled back together with the STM transaction.
pub async fn atomically_aux<T, F, A, X>(aux: A, f: F) -> T
where
    X: Aux,
    A: Fn() -> X,
    F: Fn(&mut X) -> Stm<T>,
{
    atomically_or_err_aux::<_, (), _, _, _>(aux, |atx| f(atx).map_err(StmError::Control))
        .await
        .expect("Didn't expect `abort`. Use `atomically_or_err` instead.")
}

/// Create a new transaction and run `f` until it returns a successful result and
/// can be committed without running into version conflicts, or until it returns
/// an `Abort` in which case the contained error is returned.
///
/// Make sure `f` is free of any side effects, becuase it can be called repeatedly
/// and also be aborted.
pub async fn atomically_or_err<T, E, F>(f: F) -> Result<T, E>
where
    F: Fn() -> StmResult<T, E>,
{
    atomically_or_err_aux(|| NoAux, |_| f()).await
}

/// Like [atomically_or_err], but this version also takes an auxiliary transaction system.
///
/// Aux is passed explicitly to the closure as it's more important to see which methods
/// use it and which don't, because it is ultimately an external dependency that needs to
/// be carefully managed.
///
/// For example the method might need only read-only access, in which case a more
/// permissive transaction can be constructed, than if we need write access to arbitrary
/// data managed by that system.
pub async fn atomically_or_err_aux<T, E, F, A, X>(aux: A, f: F) -> Result<T, E>
where
    X: Aux,
    A: Fn() -> X,
    F: Fn(&mut X) -> StmResult<T, E>,
{
    loop {
        // Install a new transaction into the thread local context.
        TX.with(|tref| {
            let mut t = tref.borrow_mut();
            if t.is_some() {
                // Nesting is not supported. Use `or` instead.
                panic!("Already running in an atomic transaction!")
            }
            *t = Some(Transaction::new());
        });

        // Create a new auxiliary transaction.
        let mut atx = aux();

        // Run one attempt of the atomic operation.
        let result = f(&mut atx);

        // Take the transaction from the thread local, leaving it empty.
        let tx = TX.with(|tref| tref.borrow_mut().take().unwrap());

        // See if we manage to commit some value.
        if let Some(value) = {
            match result {
                Ok(value) => {
                    if let Some(version) = tx.commit(atx) {
                        tx.notify(version);
                        Some(Ok(value))
                    } else {
                        None
                    }
                }
                Err(err) => {
                    atx.rollback();
                    match err {
                        StmError::Control(StmControl::Failure) => {
                            // We can retry straight away.
                            None
                        }
                        StmError::Control(StmControl::Retry) => {
                            // Wait until there's a change.
                            tx.wait().await;
                            None
                        }
                        StmError::Abort(e) => {
                            // Don't retry, return the error to the caller.
                            Some(Err(e))
                        }
                    }
                }
            }
        } {
            return value;
        }
    }
}
