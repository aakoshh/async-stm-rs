// For benchmarks.
#![cfg_attr(feature = "unstable", feature(test))]

#[cfg(feature = "unstable")]
extern crate test as etest;

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

/// Transaction shortcutting signals handled by the STM framework.
pub enum StmControl {
    /// The transaction failed because a value changed.
    /// It can be retried straight away.
    Failure,
    /// Retry was called and now the transaction has
    /// to wait until at least one of the variables it
    /// read have changed, before being retried.
    Retry,
}

/// STM error extended with the ability to abort the transaction
/// with an error. It is separate so that we rest assured
/// that [atomically] will not return an error, that only
/// [atomically_or_err] allows abortions.
pub enum StmError<E> {
    /// Regular error.
    Control(StmControl),
    /// Abort the transaction and return an error.
    Abort(E),
}

/// Conversion to allow mixing methods returning [Stm]
/// with ones returning [StmResult] using the `?` operator.
impl<E> From<StmControl> for StmError<E> {
    fn from(e: StmControl) -> Self {
        StmError::Control(e)
    }
}

/// Type returned by STM methods that cannot be aborted,
/// the only reason they can fail is to be retried.
pub type Stm<T> = Result<T, StmControl>;

/// Type returned by STM methods that can be aborted with an error.
///
/// Such methods must be executed with [atomically_or_err].
pub type StmResult<T, E> = Result<T, StmError<E>>;

#[cfg(test)]
mod test;
