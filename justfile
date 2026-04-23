# Update local main branch
new:
    git switch main && git pull --ff-only

# === Build ===

# Build
build:
    cargo build

# Release build
build-release:
    cargo build --release

# === Run ===

# Run the app
run:
    cargo run

# Run the app in release mode
run-release:
    cargo run --release

# === Code Quality ===

# Format code
fmt:
    cargo fmt

# Verify formatting (no changes)
fmt-check:
    cargo fmt -- --check

# Run clippy (-D warnings)
clippy:
    cargo clippy -- -D warnings

# Run clippy with auto-fix
clippy-fix:
    cargo clippy --fix --allow-dirty

# Quick pre-commit gate: fmt-check + clippy
check: fmt-check clippy

# === Testing ===

# Run all tests
test:
    cargo test

# Run a specific test by name
# Examples:
#   just test-one blend_rgba_at_zero_alpha_should_return_a
test-one name:
    cargo test {{name}}

# Run tests sequentially
test-seq:
    cargo test -- --test-threads=1

# === Docs ===

# Build documentation (no deps)
doc:
    cargo doc --no-deps

# === Misc ===

# Full CI pipeline: fmt-check + clippy + build + test
ci: fmt-check clippy build test

# Clean build artifacts
clean:
    cargo clean
