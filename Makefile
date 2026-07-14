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
	@cargo_root="$${CARGO_INSTALL_ROOT:-$${CARGO_HOME:-$$HOME/.cargo}}"; \
	installed="$$cargo_root/bin/dux"; \
	resolved="$$(command -v dux || true)"; \
	if [ -n "$$resolved" ] && [ "$$resolved" != "$$installed" ]; then \
		echo "Warning: your shell resolves dux to $$resolved, not $$installed"; \
		echo "Put $$(dirname "$$installed") before $$(dirname "$$resolved") in PATH or remove the older dux."; \
	else \
		echo "dux resolves to $$installed"; \
	fi

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
