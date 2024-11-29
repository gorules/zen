#!/bin/bash
TARGETS=(
    "x86_64-unknown-linux-gnu"
    "aarch64-unknown-linux-gnu"
    "x86_64-unknown-linux-musl"
)

set -e

for target in "${TARGETS[@]}"; do
    echo "Building for $target..."
    
    # Different package requirements for different targets
    case $target in
        "x86_64-unknown-linux-gnu")
            PACKAGES="gcc-x86-64-linux-gnu"
            CC="x86_64-linux-gnu-gcc"
            ;;
        "aarch64-unknown-linux-gnu")
            PACKAGES="gcc-aarch64-linux-gnu"
            CC="aarch64-linux-gnu-gcc"
            ;;
        "x86_64-unknown-linux-musl")
            PACKAGES="musl-tools"
            CC="musl-gcc"
            ;;
        *)
            echo "Unknown target: $target"
            exit 1
            ;;
    esac

    docker run --rm -v "$(pwd)":/workspace -w /workspace/bindings/c rust:latest bash -c "\
        apt-get update && \
        apt-get install -y $PACKAGES && \
        rustup target add $target && \
        CC=$CC cargo build --target $target --release --no-default-features"
done