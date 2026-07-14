.PHONY: run install fmt fmt-check lint lint-fix profiling

run:
	cargo run

install:
	@case "$$(uname -s)" in \
		Darwin|Linux) ;; \
		*) echo "Error: make install is supported on macOS and Linux only."; exit 1 ;; \
	esac
	@echo "Installing dux $$(cat DEV_VERSION)"
	@cargo install --path . --force --locked --quiet
	@echo "Installed dux $$(cat DEV_VERSION)"

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

lint:
	cargo clippy --all-targets --all-features

lint-fix:
	cargo clippy --all-targets --all-features --fix --allow-dirty --allow-staged

profiling:
	@command -v flamegraph >/dev/null 2>&1 || { echo "Error: 'flamegraph' not found. Install it with: cargo install flamegraph"; exit 1; }
	cargo flamegraph --profile profiling --bin dux -o flamegraph.svg
