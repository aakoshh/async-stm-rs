use super::TQueueLike;
use crate::test_queue_mod;
use crate::{retry, StmResult, TVar};
use std::any::Any;

/// A `TVar` that can be empty, or be a cons cell of an item and
/// the tail of the list, which is also a `TVarList`.
type TVarList<T> = TVar<TList<T>>;

/// A linked list of `TVar`s.
#[derive(Clone)]
enum TList<T> {
    TNil,
    TCons(T, TVarList<T>),
}

/// Ubounded queue using a linked list of `TVar`s.
///
/// This implementation builds up a linked list of `TVar`s with a
/// read and a write pointer. The good thing is that the reads don't
/// cause retries in writes, unlike if it was just a single `TVar`
/// with one data structure in it. It may also help that it's more
/// granular, and `Transaction::downcast` will not clone a full
/// data structure.
#[derive(Clone)]
pub struct TChan<T> {
    read: TVar<TVarList<T>>,
    write: TVar<TVarList<T>>,
}

impl<T> TChan<T>
where
    T: Any + Sync + Send + Clone,
{
    /// Create an empty `TChan`.
    ///
    /// Both read and write `TVar`s will be pointing at a common `TVar`
    /// containing an empty list.
    /// ```text
    ///    [TNil]
    ///   / \
    /// [*]  [*]
    /// read write
    /// ```
    pub fn new() -> TChan<T> {
        let hole = TVar::new(TList::TNil);
        TChan {
            read: TVar::new(hole.clone()),
            write: TVar::new(hole),
        }
    }

    fn is_empty_list(tvl: &TVar<TVarList<T>>) -> StmResult<bool> {
        let list_var = tvl.read()?;
        let list = list_var.read()?;
        match list.as_ref() {
            TList::TNil => Ok(true),
            _ => Ok(false),
        }
    }
}

impl<T: Any + Send + Sync + Clone> Default for TChan<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TQueueLike<T> for TChan<T>
where
    T: Any + Sync + Send + Clone,
{
    /// Pop the head of the queue, or retry until there is an element if it's empty.
    ///
    /// Moves the read `TVar` down the list to point at the next item.
    /// ```text
    ///  [TCons(x, [TCons(y, [TNil])])]
    ///  |         |         |
    /// [ ]       [*]       [*]
    /// read0 ->  read1     write
    /// ```
    fn read(&self) -> StmResult<T> {
        let var_list = self.read.read()?;
        let list = var_list.read_clone()?;
        match list {
            TList::TNil => retry(),
            TList::TCons(value, tail) => {
                self.read.write(tail)?;
                Ok(value)
            }
        }
    }

    /// Push to the end of the queue.
    ///
    /// Replaces the contents of the current write `TVar` with a `TCons` and points
    /// the write `TVar` at a new `TNil`.
    /// ```text
    ///  [TCons(x, [TCons(y, [TNil])])]
    ///  |         |         |
    /// [*]       [ ]       [*]
    /// read      write0 -> write1
    /// ```
    fn write(&self, value: T) -> StmResult<()> {
        let new_list_end = TVar::new(TList::TNil);
        let var_list = self.write.read()?;
        var_list.write(TList::TCons(value, new_list_end.clone()))?;
        self.write.write(new_list_end)?;
        Ok(())
    }

    fn is_empty(&self) -> StmResult<bool> {
        if TChan::<T>::is_empty_list(&self.read)? {
            TChan::<T>::is_empty_list(&self.write)
        } else {
            Ok(false)
        }
    }
}

test_queue_mod!(|| { crate::queues::tchan::TChan::<i32>::new() });
