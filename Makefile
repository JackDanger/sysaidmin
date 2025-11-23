# SYSAIDMIN Makefile
# Provides convenient targets for building, testing, and running

# Variables
CARGO := cargo
DOCKER_SCRIPT := ./build.sh
BINARY_NAME := sysaidmin
TARGET_DIR := target
DIST_DIR := dist
ARCHES := amd64 arm64 armhf riscv64

# Default target
.DEFAULT_GOAL := help

.PHONY: help
help: ## Show this help message
	@echo "SYSAIDMIN Makefile"
	@echo ""
	@echo "Available targets:"
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-20s %s\n", $$1, $$2}'
	@echo ""
	@echo "Architecture-specific targets:"
	@echo "  sysaidmin-<arch>        Build binary for architecture (amd64, arm64, armhf, riscv64)"
	@echo "  sysaidmin-<arch>.deb    Build .deb package for architecture"
	@echo ""
	@echo "Examples:"
	@echo "  make build              Build for native architecture"
	@echo "  make sysaidmin-amd64    Build amd64 binary"
	@echo "  make sysaidmin-arm64.deb Build arm64 .deb package"
	@echo "  make build-all          Build all architectures"

.PHONY: build
build: ## Build for native architecture
	$(CARGO) build --release --workspace

.PHONY: build-dev
build-dev: ## Build for native architecture (dev mode)
	$(CARGO) build --workspace

.PHONY: build-all
build-all: ## Build all architectures (requires Docker)
	@echo "Building all architectures via Docker..."
	$(DOCKER_SCRIPT)

# Architecture-specific binary targets
.PHONY: sysaidmin-amd64 sysaidmin-arm64 sysaidmin-armhf sysaidmin-riscv64
sysaidmin-amd64: ## Build amd64 binary
	@echo "Building amd64 binary..."
	SYSAIDMIN_ARCHES="amd64" $(DOCKER_SCRIPT)
	@echo "Binary available at: $(DIST_DIR)/amd64/bin/$(BINARY_NAME)"

sysaidmin-arm64: ## Build arm64 binary
	@echo "Building arm64 binary..."
	SYSAIDMIN_ARCHES="arm64" $(DOCKER_SCRIPT)
	@echo "Binary available at: $(DIST_DIR)/arm64/bin/$(BINARY_NAME)"

sysaidmin-armhf: ## Build armhf binary
	@echo "Building armhf binary..."
	SYSAIDMIN_ARCHES="armhf" $(DOCKER_SCRIPT)
	@echo "Binary available at: $(DIST_DIR)/armhf/bin/$(BINARY_NAME)"

sysaidmin-riscv64: ## Build riscv64 binary
	@echo "Building riscv64 binary..."
	SYSAIDMIN_ARCHES="riscv64" $(DOCKER_SCRIPT)
	@echo "Binary available at: $(DIST_DIR)/riscv64/bin/$(BINARY_NAME)"

# Architecture-specific .deb package targets
.PHONY: sysaidmin-amd64.deb sysaidmin-arm64.deb sysaidmin-armhf.deb sysaidmin-riscv64.deb
sysaidmin-amd64.deb: sysaidmin-amd64 ## Build amd64 .deb package
	@echo ".deb package available at: $(DIST_DIR)/amd64/deb/$(BINARY_NAME)_*.deb"

sysaidmin-arm64.deb: sysaidmin-arm64 ## Build arm64 .deb package
	@echo ".deb package available at: $(DIST_DIR)/arm64/deb/$(BINARY_NAME)_*.deb"

sysaidmin-armhf.deb: sysaidmin-armhf ## Build armhf .deb package
	@echo ".deb package available at: $(DIST_DIR)/armhf/deb/$(BINARY_NAME)_*.deb"

sysaidmin-riscv64.deb: sysaidmin-riscv64 ## Build riscv64 .deb package
	@echo ".deb package available at: $(DIST_DIR)/riscv64/deb/$(BINARY_NAME)_*.deb"

.PHONY: test
test: ## Run all tests
	$(CARGO) test --workspace

.PHONY: test-verbose
test-verbose: ## Run tests with verbose output
	$(CARGO) test --workspace -- --nocapture

.PHONY: run
run: build-dev ## Run the program (builds in dev mode first)
	$(CARGO) run --workspace

.PHONY: run-release
run-release: build ## Run the program (release build)
	$(CARGO) run --release --workspace

.PHONY: fmt
fmt: ## Format code
	$(CARGO) fmt --all

.PHONY: lint
lint: ## Run clippy linter
	$(CARGO) clippy --workspace --all-targets -- -D warnings

.PHONY: clean
clean: ## Clean build artifacts
	$(CARGO) clean
	@echo "Cleaned cargo build artifacts"

.PHONY: clean-all
clean-all: clean ## Clean build artifacts and dist directory
	rm -rf $(DIST_DIR)
	@echo "Cleaned all build artifacts and dist directory"

.PHONY: check
check: ## Check code without building
	$(CARGO) check --workspace

# Convenience targets for common operations
.PHONY: all
all: test build ## Run tests and build (default workflow)

.PHONY: ci
ci: fmt lint test build ## Run CI checks (format, lint, test, build)

