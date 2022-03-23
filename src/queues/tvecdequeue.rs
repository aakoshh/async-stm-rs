use super::TQueueLike;
use crate::test_queue_mod;
use crate::{retry, StmResult, TVar};
use std::{any::Any, collections::VecDeque};

#[derive(Clone)]
/// Unbounded queue backed by a single `VecDequeue`.
///
/// The drawback is that reads and writes both touch the same `TVar`.
pub struct TVecDequeue<T> {
    queue: TVar<VecDeque<T>>,
}

impl<T> TVecDequeue<T>
where
    T: Any + Sync + Send + Clone,
{
    /// Create an empty `TVecDequeue`.
    #[allow(dead_code)]
    pub fn new() -> TVecDequeue<T> {
        TVecDequeue {
            queue: TVar::new(VecDeque::new()),
        }
    }
}

impl<T: Any + Send + Sync + Clone> Default for TVecDequeue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TQueueLike<T> for TVecDequeue<T>
where
    T: Any + Sync + Send + Clone,
{
    fn write(&self, value: T) -> StmResult<()> {
        let mut queue = self.queue.read_clone()?;
        queue.push_back(value);
        self.queue.write(queue)
    }

    fn read(&self) -> StmResult<T> {
        let mut queue = self.queue.read_clone()?;
        match queue.pop_front() {
            None => retry(),
            Some(value) => {
                self.queue.write(queue)?;
                Ok(value)
            }
        }
    }

    fn is_empty(&self) -> StmResult<bool> {
        self.queue.read().map(|v| v.is_empty())
    }
}

test_queue_mod!(|| { crate::queues::tvecdequeue::TVecDequeue::<i32>::new() });
