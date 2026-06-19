# Justfile for ttyd-rs
# Run with `just <command>`

# Default recipe (list all recipes)
default:
    @just --list

# Build the project
build:
    cargo build

# Build with release optimizations
build-release:
    cargo build --release

# Run the project
run *ARGS:
    cargo run -- {{ARGS}}

# Run tests
test:
    cargo test

# Run a specific test
test-one TEST:
    cargo test {{TEST}}

# Check code without building
check:
    cargo check

# Format code
fmt:
    cargo fmt

# Check formatting
fmt-check:
    cargo fmt -- --check

# Run clippy lints
clippy:
    cargo clippy -- -D warnings

# Run all quality checks (must pass before commit)
qa: fmt-check clippy test
    @echo "✅ All quality checks passed!"

# Clean build artifacts
clean:
    cargo clean

# Update dependencies
update:
    cargo update

# Show outdated dependencies
outdated:
    cargo outdated

# Run the server with default settings
serve:
    cargo run

# Run with authentication
serve-auth USERNAME PASSWORD:
    cargo run -- --auth --username {{USERNAME}} --password {{PASSWORD}}

# Generate documentation
doc:
    cargo doc --open

# Watch and rebuild on changes (requires cargo-watch)
watch:
    cargo watch -x check -x test

# Install cargo tools
install-tools:
    cargo install cargo-watch cargo-outdated cargo-audit
