lint:
	cargo fmt --all -- --check
	cargo clippy --features "queues" -- -D clippy::all

test:
	cargo test --features "queues"

build:
	cargo build --all-targets --features "queues"
