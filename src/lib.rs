// For benchmarks.
#![cfg_attr(feature = "unstable", feature(test))]

#[cfg(feature = "unstable")]
extern crate test as etest;

use std::error::Error;

mod ops;
mod transaction;
mod vars;
mod version;

/// Auxiliary transactions.
pub mod auxtx;

/// `TVar` is the public interface to lift data into the transactional
/// context for subsequent read and write operations.
pub use vars::TVar;

/// The primary verbs to interact with STM transactions.
pub use ops::{
    abort, atomically, atomically_aux, atomically_or_err, atomically_or_err_aux, guard, or, retry,
};

/// Transactional queue implementations.
#[cfg(feature = "queues")]
pub mod queues;

pub enum StmError {
    /// The transaction failed because a value changed.
    /// It can be retried straight away.
    Failure,
    /// Retry was called and now the transaction has
    /// to wait until at least one of the variables it
    /// read have changed, before being retried.
    Retry,
}

/// STM error extended with the ability to abort the transaction
/// with a dynamic error. It is separate so that we rest assured
/// that `atomically` will not throw an error, that only
/// `atomically_or_err` allows abortions.
pub enum StmDynError {
    /// Regular error.
    Control(StmError),
    /// Abort the transaction and return an error.
    Abort(Box<dyn Error + Send + Sync>),
}

impl From<StmError> for StmDynError {
    fn from(e: StmError) -> Self {
        StmDynError::Control(e)
    }
}

pub type StmResult<T> = Result<T, StmError>;
pub type StmDynResult<T> = Result<T, StmDynError>;

#[cfg(test)]
mod test;
