# STM

`async-stm` is an implementation of Software Transactional Memory.
It was originally inspired by [rust-stm](https://github.com/Marthog/rust-stm),
but does things sligly differently, in a more traditional fashion.

It was also extended in the following ways:
* Made `atomically` asynchronous, so STM operations can be used with `tokio` without blocking a full thread.
* Added the ability to `abort` a transaction with an error, which the caller has to handle.
* The transaction is passed around in a thread-local variable, for a simplified `TVar` API.
* Reading a `TVar` returns an `Arc`, so cloning can be delayed until we have to modify the result.
* Added the option to pass in an [auxiliary transaction](src/aux.rs) that gets committed or rolled back together with the STM transaction, and can also cause cause a retry if it detects some conflict of its own. This is a potential way to have a hybrid persistent STM solution.
* Added some optional [queue](src/queues) implementations based on Simon Marlow's book, _Parallel and Concurrent Programming in Haskell_.

Please look at the [tests](src/test.rs) for example usage.

## Prerequisites

Install the following to be be able to build the project:
* [cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html)
* [rust nightly](https://rust-lang.github.io/rustup/concepts/channels.html)

```shell
curl https://sh.rustup.rs -sSf | sh
rustup toolchain install nightly
rustup default nightly
rustup update
```

## Benchmarks

There are benchmarks included to help compare the tradeoffs between the different queue implementations.

```shell
cargo bench "bench" --all-features
```

## See more

* https://www.microsoft.com/en-us/research/publication/beautiful-concurrency/
* https://bartoszmilewski.com/2010/09/11/beyond-locks-software-transactional-memory/

## License

This project is licensed under the [MIT license].

[MIT license]: https://github.com/aakoshh/async-stm-rs/blob/master/LICENSE
