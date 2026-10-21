fmt:
	cargo fmt --all

dev:
	cargo build

check:
	cargo fmt --all -- --check
	cargo check --locked --workspace --all-targets --all-features
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

test:
	cargo test --locked --release --workspace --all-features
