use std::{error::Error, mem};

use crate::aux::*;
use crate::{
    aux::NoAux,
    transaction::{with_tx, Transaction, TX},
    StmDynError, StmDynResult, StmError, StmResult,
};

/// Abandon the transaction and retry after some of the variables read have changed.
pub fn retry<T>() -> StmResult<T> {
    Err(StmError::Retry)
}

/// Retry unless a given condition has been met.
pub fn guard(cond: bool) -> StmResult<()> {
    if cond {
        Ok(())
    } else {
        retry()
    }
}

pub fn abort<T, E: Error + Send + Sync + 'static>(e: E) -> StmDynResult<T> {
    Err(StmDynError::Abort(Box::new(e)))
}

/// Run the first function; if it returns a `Retry`,
/// run the second function; if that too returns `Retry`
/// then combine the values they have read, so that
/// the overall retry will react to any change.
///
/// If they return `Failure` then just return that result,
/// since the transaction can be retried right now.
pub fn or<F, G, T>(f: F, g: G) -> StmResult<T>
where
    F: FnOnce() -> StmResult<T>,
    G: FnOnce() -> StmResult<T>,
{
    let mut snapshot = with_tx(|tx| tx.clone());

    match f() {
        Err(StmError::Retry) => {
            // Restore the original transaction state.
            with_tx(|tx| {
                mem::swap(tx, &mut snapshot);
            });

            match g() {
                retry @ Err(StmError::Retry) =>
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
///
/// Nesting calls to `atomically` is not supported at the moment and will result in a panic.
pub async fn atomically<F, T>(f: F) -> T
where
    F: Fn() -> StmResult<T>,
{
    atomically_aux(|| NoAux, |_| f()).await
}

/// Like `atomically`, but this version also takes an auxiliary transaction system
/// that gets committed or rolled back together with the STM transaction.
pub async fn atomically_aux<F, T, A, X>(aux: A, f: F) -> T
where
    X: Aux,
    A: Fn() -> X,
    F: Fn(&mut X) -> StmResult<T>,
{
    atomically_or_err_aux(aux, |atx| f(atx).map_err(StmDynError::Control))
        .await
        .expect("Didn't expect `abort`. Use `atomically_or_err` instead.")
}

/// Create a new transaction and run `f` until it returns a successful result and
/// can be committed without running into version conflicts, or until it returns
/// an `Abort` in which case the contained error is returned.
///
/// Make sure `f` is free of any side effects, becuase it can be called repeatedly
/// and also be aborted.
///
/// Nesting calls to `atomically_or_err` is not supported at thed moment and will result in a panic.
pub async fn atomically_or_err<F, T>(f: F) -> Result<T, Box<dyn Error + Send + Sync>>
where
    F: Fn() -> StmDynResult<T>,
{
    atomically_or_err_aux(|| NoAux, |_| f()).await
}

/// Like `atomically_or_err`, but this version also takes an auxiliary transaction system.
///
/// Aux is passed explicitly to the closure as it's more important to see which methods
/// use it and which don't, because it is ultimately an external dependency that needs to
/// be carefully managed.
///
/// For example the method might need only read-only access, in which case a more
/// permissive transaction can be constructed, than if we need write access to arbitrary
/// data managed by that system.
pub async fn atomically_or_err_aux<F, T, A, X>(
    aux: A,
    f: F,
) -> Result<T, Box<dyn Error + Send + Sync>>
where
    X: Aux,
    A: Fn() -> X,
    F: Fn(&mut X) -> StmDynResult<T>,
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
                        StmDynError::Control(StmError::Failure) => {
                            // We can retry straight away.
                            None
                        }
                        StmDynError::Control(StmError::Retry) => {
                            // Wait until there's a change.
                            tx.wait().await;
                            None
                        }
                        StmDynError::Abort(e) => {
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
