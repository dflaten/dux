.PHONY: run fmt fmt-check lint lint-fix install profiling

run:
	cargo run

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

lint:
	cargo clippy --all-targets --all-features

lint-fix:
	cargo clippy --all-targets --all-features --fix --allow-dirty --allow-staged

install:
	@case "$$(uname -s)" in \
		Darwin|Linux) ;; \
		*) echo "Error: make install is supported on macOS and Linux only."; exit 1 ;; \
	esac
	cargo install --path . --locked

profiling:
	@command -v flamegraph >/dev/null 2>&1 || { echo "Error: 'flamegraph' not found. Install it with: cargo install flamegraph"; exit 1; }
	cargo flamegraph --profile profiling --bin dux -o flamegraph.svg
