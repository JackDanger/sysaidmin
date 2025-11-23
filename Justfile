default: build

build:
	cargo build --workspace

test:
	cargo test --workspace

fmt:
	cargo fmt --all

lint:
	cargo clippy --workspace --all-targets -- -D warnings

deb:
	cargo deb -p sysaidmin

deb-docker:
	./build-deb-docker.sh


