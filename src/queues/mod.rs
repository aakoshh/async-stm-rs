pub mod tbqueue;
pub mod tchan;
pub mod tqueue;
pub mod tvecdequeue;

use crate::StmResult;

/// Transactional queue-like structure.
///
/// This is a common interface between the various implementations in Simon Marlow's book.
pub trait TQueueLike<T>: Clone + Send {
    /// Pop the head of the queue, or retry until there is an element if it's empty.
    fn read(&self) -> StmResult<T>;
    /// Push to the end of the queue.
    fn write(&self, value: T) -> StmResult<()>;
    /// Check if the queue is empty.
    fn is_empty(&self) -> StmResult<bool>;
}

#[cfg(test)]
mod test {
    use std::time::Duration;
    use tokio::sync::mpsc::{self, UnboundedReceiver};

    use etest::Bencher;

    use super::TQueueLike;
    use crate::atomically;

    async fn recv_timeout<T>(rx: &mut UnboundedReceiver<T>, t: Duration) -> T {
        tokio::time::timeout(t, rx.recv()).await.unwrap().unwrap()
    }

    pub async fn test_write_and_read_back<Q: 'static + TQueueLike<i32>>(mq: fn() -> Q) {
        let queue = mq();
        let (x, y) = atomically(|| {
            queue.write(42)?;
            queue.write(31)?;
            let x = queue.read()?;
            let y = queue.read()?;
            Ok((x, y))
        })
        .await;

        assert_eq!(42, x);
        assert_eq!(31, y);
    }

    /// Run multiple threads.
    ///
    /// Thread 1: Read from the channel, block until it's non-empty, then return the value.
    ///
    /// Thread 2: Wait a bit, then write a value.
    ///
    /// Check that Thread 1 has been woken up to read the value written by Thread 2.
    pub async fn test_threaded<Q: TQueueLike<i32> + Sync + 'static>(mq: fn() -> Q) {
        let queue1 = mq();
        // Clone for Thread 2
        let queue2 = queue1.clone();

        let (sender, mut receiver) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let x = atomically(|| queue2.read()).await;
            sender.send(x).unwrap();
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        atomically(|| queue1.write(42)).await;

        let x = recv_timeout(&mut receiver, Duration::from_millis(500)).await;

        assert_eq!(42, x);
    }

    pub async fn test_is_empty<Q: 'static + TQueueLike<i32>>(mq: fn() -> Q) {
        let queue = mq();
        let is_empty = atomically(|| queue.is_empty()).await;

        assert!(is_empty);
    }

    pub async fn test_non_empty<Q: 'static + TQueueLike<i32>>(mq: fn() -> Q) {
        let queue = mq();
        atomically(|| queue.write(42)).await;
        let is_empty = atomically(|| queue.is_empty()).await;
        assert!(!is_empty);
    }

    // Benchmarks based on https://github.com/simonmar/parconc-examples/blob/master/chanbench.hs

    fn new_bench_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    /// Two threads, one reading from and one writing to the channel.
    pub fn bench_two_threads_read_write<Q: TQueueLike<i32> + Sync + 'static>(
        b: &mut Bencher,
        mq: fn() -> Q,
    ) {
        let rt = new_bench_runtime();

        b.iter(|| {
            rt.block_on(async {
                let n = 1000;
                let queue1 = mq();
                let queue2 = queue1.clone();

                let sender = tokio::spawn(async move {
                    for i in 1..n {
                        atomically(|| queue1.write(i)).await;
                    }
                });

                let receiver = tokio::spawn(async move {
                    for i in 1..n {
                        let r = atomically(|| queue2.read()).await;
                        assert_eq!(i, r);
                    }
                });

                tokio::time::timeout(Duration::from_secs(10), sender)
                    .await
                    .unwrap()
                    .unwrap();

                tokio::time::timeout(Duration::from_secs(10), receiver)
                    .await
                    .unwrap()
                    .unwrap();
            });
        });
    }

    /// One thread, writing a large number of values then reading them.
    pub fn bench_one_thread_write_many_then_read<Q: TQueueLike<i32>>(
        b: &mut Bencher,
        mq: fn() -> Q,
    ) {
        let rt = new_bench_runtime();

        b.iter(|| {
            rt.block_on(async {
                let n = 1000;
                let chan = mq();

                for i in 1..n {
                    atomically(|| chan.write(i)).await;
                }
                for i in 1..n {
                    let r = atomically(|| chan.read()).await;
                    assert_eq!(i, r);
                }
            });
        });
    }

    // One thread, repeatedly writing and then reading a number of values.
    pub fn bench_one_thread_repeat_write_read<Q: TQueueLike<i32>>(b: &mut Bencher, mq: fn() -> Q) {
        let rt = new_bench_runtime();

        b.iter(|| {
            rt.block_on(async {
                let n = 1000;
                let m = 100;
                let chan = mq();

                for i in 1..(n / m) {
                    for j in 1..m {
                        atomically(|| chan.write(i * m + j)).await;
                    }
                    for j in 1..m {
                        let r = atomically(|| chan.read()).await;
                        assert_eq!(i * m + j, r);
                    }
                }
            });
        });
    }
}

/// Reuse the same test definitions for each implementation of the `TQueueLike` trait
/// by calling this macro with a function to create a new instance of the queue.
///
/// For example:
/// ```text
/// test_queue_mod!(|| { crate::queues::tchan::TChan::<i32>::new() });
/// ```
#[macro_export]
macro_rules! test_queue_mod {
    ($make:expr) => {
        #[cfg(test)]
        mod test_queue {
            use crate::queues::test as tq;

            #[tokio::test]
            async fn write_and_read_back() {
                tq::test_write_and_read_back($make).await;
            }

            #[tokio::test]
            async fn threaded() {
                tq::test_threaded($make).await;
            }

            #[tokio::test]
            async fn is_empty() {
                tq::test_is_empty($make).await;
            }

            #[tokio::test]
            async fn non_empty() {
                tq::test_non_empty($make).await;
            }
        }

        #[cfg(test)]
        mod bench_queue {
            use super::super::test as tq;
            use etest::Bencher;

            #[bench]
            fn two_threads_read_write(b: &mut Bencher) {
                tq::bench_two_threads_read_write(b, $make);
            }

            #[bench]
            fn one_thread_write_many_then_read(b: &mut Bencher) {
                tq::bench_one_thread_write_many_then_read(b, $make);
            }

            #[bench]
            fn one_thread_repeat_write_read(b: &mut Bencher) {
                tq::bench_one_thread_repeat_write_read(b, $make);
            }
        }
    };
}
