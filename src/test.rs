use tokio::sync::mpsc::Receiver;

use crate::version::next_version;

use super::*;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

async fn recv_timeout<T>(mut rx: Receiver<T>) -> T {
    tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

#[test]
fn next_version_increments() {
    let a = next_version();
    let b = next_version();
    assert!(b > a)
}

#[test]
fn id_increments() {
    let a = TVar::new(42);
    let b = TVar::new(42);
    assert!(b.id > a.id)
}

#[tokio::test]
async fn basics() {
    let ta = TVar::new(1);
    let tb = TVar::new(vec![1, 2, 3]);

    let (a0, b0) = atomically(|| {
        let a = ta.read()?;
        let b = tb.read()?;
        let mut b1 = b.as_ref().clone();
        b1.push(4);
        tb.write(b1)?;
        Ok((a, b))
    })
    .await;

    assert_eq!(*a0, 1);
    assert_eq!(*b0, vec![1, 2, 3]);

    let b1 = atomically(|| tb.read()).await;
    assert_eq!(*b1, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn conflict_if_written_after_start() {
    let ta = Arc::new(TVar::new(1));
    let tac = ta.clone();

    let t = tokio::task::spawn_blocking(move || {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(atomically(|| {
                thread::sleep(Duration::from_millis(100));
                tac.read()
            }))
        })
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    atomically(|| ta.update(|x| x + 1)).await;

    let a = t.await.unwrap();
    // We have written a between the start of the transaction
    // and the time it read the value, so it should have restarted.
    assert_eq!(*a, 2);
}

#[tokio::test]
async fn no_confict_if_read_before_write() {
    let ta = Arc::new(TVar::new(1));
    let tac = ta.clone();

    let t = tokio::task::spawn_blocking(move || {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(atomically(|| {
                let a = tac.read()?;
                thread::sleep(Duration::from_millis(100));
                Ok(a)
            }))
        })
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    atomically(|| ta.update(|x| x + 1)).await;

    let a = t.await.unwrap();
    // Even though we spent a lot of time after reading the value
    // we didn't read anything else that changed, so it's a consistent
    // state for the lengthy calculation that followed.
    assert_eq!(*a, 1);
}

#[tokio::test]
async fn or_retry_first_return_second() {
    let ta = TVar::new(1);
    let tb = TVar::new("Hello");

    let (a, b) = atomically(|| {
        tb.write("World")?;
        or(
            || {
                ta.write(2)?;
                retry()
            },
            || Ok((ta.read()?, tb.read()?)),
        )
    })
    .await;

    assert_eq!(*a, 1);
    assert_eq!(*b, "World");
}

#[tokio::test]
async fn retry_wait_notify() {
    let ta = Arc::new(TVar::new(1));
    let tac = ta.clone();

    let (sender, receiver) = tokio::sync::mpsc::channel(1);

    tokio::spawn(async move {
        let a = atomically(|| {
            let a = tac.read()?;
            guard(*a > 1)?;
            Ok(a)
        })
        .await;

        sender.send(*a).await.unwrap()
    });

    tokio::time::sleep(Duration::from_millis(250)).await;
    atomically(|| ta.write(2)).await;

    let a = recv_timeout(receiver).await;

    assert_eq!(a, 2);
}

#[tokio::test]
async fn new_tvar_in_transaction() {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);

    tokio::spawn(async move {
        let a = atomically(|| {
            let t = TVar::new(1);
            t.write(2)?;
            t.read()
        })
        .await;

        sender.send(*a).await.unwrap();
    });

    let a = recv_timeout(receiver).await;

    assert_eq!(a, 2);
}

#[tokio::test]
async fn nested_atomically() {
    let x = TVar::new(0);

    // NOTE: Nesting `atomically` used to panic,
    // but now that it's async it's not immediately executed,
    // and since `atomically` only takes normal functions,
    // trying to execute one within the other would not compile.
    // However, we can return a future from an `atomically` block
    // to be executed outside of it.
    let a = atomically(|| {
        x.write(1)?;
        Ok(atomically(|| x.write(2)))
    })
    .await;

    assert_eq!(*atomically(|| { x.read() }).await, 1);
    a.await;
    assert_eq!(*atomically(|| { x.read() }).await, 2);
}

#[test]
#[should_panic]
fn read_outside_atomically() {
    let _ = TVar::new("Don't read it!").read();
}

#[test]
#[should_panic]
fn write_outside_atomically() {
    let _ = TVar::new(0).write(1);
}

#[tokio::test]
async fn nested_abort() {
    let r = TVar::new(0);
    let show = |label| {
        println!("{}: r = {}", label, r.read()?);
        Ok(())
    };
    let add1 = |x: i32| x + 1;
    let abort = retry;

    fn nested<F>(f: F) -> StmResult<()>
    where
        F: FnOnce() -> StmResult<()>,
    {
        or(f, || Ok(()))
    }

    let v = atomically(|| {
        show('a')?; // 0
        r.update(add1)?;
        show('b')?; // 1
        nested(|| {
            show('c')?; // still 1
            r.update(add1)?;
            show('d')?; // 2
            abort()
        })?;
        show('e')?; // back to 1 because abort
        nested(|| {
            show('f')?; // still 1
            r.update(add1)?;
            show('g') // 2
        })?;
        show('h')?; // 2
        r.update(add1)?;
        show('i')?; // 3
        r.read()
    })
    .await;

    assert_eq!(*v, 3);
}

#[derive(Debug, Clone)]
pub struct TestError;

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "test error instance")
    }
}

impl Error for TestError {}

#[tokio::test]
async fn abort_with_error() {
    let a = TVar::new(0);

    let r = atomically_or_err(|| {
        a.write(1)?;
        abort(TestError)?;
        Ok(())
    })
    .await;

    assert_eq!(
        r.err().map(|e| e.to_string()),
        Some("test error instance".to_owned())
    );
}
