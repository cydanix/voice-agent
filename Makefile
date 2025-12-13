.PHONY: build release debug run run-debug clean install help

# Default target
.DEFAULT_GOAL := help

# Binary name
BINARY_NAME := voice-agent
TARGET_DIR := target/release
BINARY := $(TARGET_DIR)/$(BINARY_NAME)

# Build release with debug symbols
# Using RUSTFLAGS to add debug info without modifying Cargo.toml
release: $(BINARY)

$(BINARY):
	@echo "Building release binary with debug symbols..."
	RUSTFLAGS="-C debuginfo=2" cargo build --release
	@echo "Build complete: $(BINARY)"
	@ls -lh $(BINARY)

# Build debug version (faster compilation, full debug info)
debug:
	@echo "Building debug binary..."
	cargo build
	@echo "Build complete: target/debug/$(BINARY_NAME)"
	@ls -lh target/debug/$(BINARY_NAME)

# Build and run release binary (always rebuilds)
run:
	@echo "Building release binary with debug symbols..."
	RUSTFLAGS="-C debuginfo=2" cargo build --release
	@echo "Build complete, running $(BINARY)..."
	$(BINARY)

# Build and run debug binary (faster iteration)
run-debug:
	@echo "Building and running debug binary..."
	cargo run

# Clean build artifacts
clean:
	@echo "Cleaning build artifacts..."
	cargo clean
	@echo "Clean complete"

# Install binary to /usr/local/bin (requires sudo)
install: $(BINARY)
	@echo "Installing $(BINARY_NAME) to /usr/local/bin..."
	sudo cp $(BINARY) /usr/local/bin/$(BINARY_NAME)
	sudo chmod +x /usr/local/bin/$(BINARY_NAME)
	@echo "Installation complete"

# Uninstall binary from /usr/local/bin
uninstall:
	@echo "Uninstalling $(BINARY_NAME) from /usr/local/bin..."
	sudo rm -f /usr/local/bin/$(BINARY_NAME)
	@echo "Uninstallation complete"

# Run tests
test:
	@echo "Running tests..."
	cargo test

# Run clippy linter
clippy:
	@echo "Running clippy..."
	cargo clippy -- -D warnings

# Format code
fmt:
	@echo "Formatting code..."
	cargo fmt

# Check code (compile without building)
check:
	@echo "Checking code..."
	cargo check

# Show help
help:
	@echo "Available targets:"
	@echo "  make release   - Build release binary with debug symbols (default)"
	@echo "  make debug     - Build debug binary (faster, full debug info)"
	@echo "  make run       - Build and run release binary"
	@echo "  make run-debug - Build and run debug binary (faster iteration)"
	@echo "  make clean     - Clean build artifacts"
	@echo "  make install   - Install binary to /usr/local/bin (requires sudo)"
	@echo "  make uninstall - Remove binary from /usr/local/bin"
	@echo "  make test      - Run tests"
	@echo "  make clippy    - Run clippy linter"
	@echo "  make fmt       - Format code"
	@echo "  make check     - Check code (compile without building)"
	@echo "  make help      - Show this help message"
