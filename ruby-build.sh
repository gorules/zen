#!/bin/bash
TARGETS=(
    "x86_64-unknown-linux-gnu"
    "aarch64-unknown-linux-gnu"
    "x86_64-unknown-linux-musl"
)

set -e

for target in "${TARGETS[@]}"; do
    echo "Building for $target..."
    
    case $target in
        "x86_64-unknown-linux-gnu")
            DOCKER_PLATFORM="linux/amd64"
            PACKAGES="build-essential"
            ;;
        "aarch64-unknown-linux-gnu")
            DOCKER_PLATFORM="linux/amd64"
            PACKAGES="gcc-aarch64-linux-gnu"
            ;;
        "x86_64-unknown-linux-musl")
            DOCKER_PLATFORM="linux/amd64"
            PACKAGES="musl-tools"
            ;;
        *)
            echo "Unknown target: $target"
            exit 1
            ;;
    esac

    docker run --rm --platform $DOCKER_PLATFORM \
        -v "$(pwd)":/workspace \
        -w /workspace/bindings/c \
        rust:1.73 bash -c "\
        apt-get update && \
        apt-get install -y $PACKAGES && \
        rustup target add $target && \
        QUICKJS_SYSTEM_MALLOC=1 \
        QUICKJS_DISABLE_ATOMICS=1 \
        cargo build --target $target --release --no-default-features"
done