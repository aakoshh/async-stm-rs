use super::TQueueLike;
use crate::test_queue_mod;
use crate::{retry, StmResult, TVar};
use std::any::Any;

/// Unbounded queue using two vectors.
///
/// This implementation writes to one vector and reads from the other
/// until the reads vector becomes empty and the two need to be swapped.
/// Again reads don't block writes most of the time. It has an amortised
/// cost of O(1).
#[derive(Clone)]
pub struct TQueue<T> {
    read: TVar<Vec<T>>,
    write: TVar<Vec<T>>,
}

impl<T> TQueue<T>
where
    T: Any + Sync + Send + Clone,
{
    /// Create an empty `TQueue`.
    pub fn new() -> TQueue<T> {
        TQueue {
            read: TVar::new(Vec::new()),
            write: TVar::new(Vec::new()),
        }
    }
}

impl<T: Any + Send + Sync + Clone> Default for TQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TQueueLike<T> for TQueue<T>
where
    T: Any + Sync + Send + Clone,
{
    fn write(&self, value: T) -> StmResult<()> {
        let mut v = self.write.read_clone()?;
        v.push(value);
        self.write.write(v)
    }

    fn read(&self) -> StmResult<T> {
        let mut rv = self.read.read_clone()?;
        // Elements are stored in reverse order.
        match rv.pop() {
            Some(value) => {
                self.read.write(rv)?;
                Ok(value)
            }
            None => {
                let mut wv = self.write.read_clone()?;
                if wv.is_empty() {
                    retry()
                } else {
                    wv.reverse();
                    let value = wv.pop().unwrap();
                    self.read.write(wv)?;
                    self.write.write(Vec::new())?;
                    Ok(value)
                }
            }
        }
    }

    fn is_empty(&self) -> StmResult<bool> {
        if self.read.read()?.is_empty() {
            Ok(self.write.read()?.is_empty())
        } else {
            Ok(false)
        }
    }
}

test_queue_mod!(|| { crate::queues::tqueue::TQueue::<i32>::new() });
