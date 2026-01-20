#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
C_BINDINGS_DIR="$ROOT_DIR/bindings/c"
DOTNET_DIR="$SCRIPT_DIR"

echo "=== Building Zen Engine .NET Bindings ==="
echo ""

# Detect platform and architecture
ARCH=$(uname -m)
if [[ "$OSTYPE" == "linux-gnu"* ]]; then
    if [[ "$ARCH" == "aarch64" ]] || [[ "$ARCH" == "arm64" ]]; then
        PLATFORM="linux-arm64"
    else
        PLATFORM="linux-x64"
    fi
    LIB_NAME="libzen_ffi.so"
elif [[ "$OSTYPE" == "darwin"* ]]; then
    if [[ "$ARCH" == "arm64" ]]; then
        PLATFORM="osx-arm64"
    else
        PLATFORM="osx-x64"
    fi
    LIB_NAME="libzen_ffi.dylib"
elif [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]] || [[ "$OSTYPE" == "win32" ]]; then
    if [[ "$ARCH" == "aarch64" ]] || [[ "$ARCH" == "arm64" ]]; then
        PLATFORM="win-arm64"
    else
        PLATFORM="win-x64"
    fi
    LIB_NAME="zen_ffi.dll"
else
    echo "Unsupported platform: $OSTYPE"
    exit 1
fi

echo "Platform: $PLATFORM"
echo "Architecture: $ARCH"
echo "Library: $LIB_NAME"
echo ""

# Step 1: Build Rust library
echo "Step 1: Building Rust C bindings..."
cd "$C_BINDINGS_DIR"
cargo build --release --no-default-features
echo "Done."
echo ""

# Step 2: Copy native library
echo "Step 2: Copying native library..."
RUNTIME_DIR="$DOTNET_DIR/runtimes/$PLATFORM/native"
mkdir -p "$RUNTIME_DIR"

SOURCE_LIB="$ROOT_DIR/target/release/$LIB_NAME"
if [[ -f "$SOURCE_LIB" ]]; then
    cp "$SOURCE_LIB" "$RUNTIME_DIR/"
    echo "Copied $LIB_NAME to $RUNTIME_DIR/"
else
    echo "ERROR: Library not found at $SOURCE_LIB"
    echo "Make sure Cargo.toml has crate-type = [\"cdylib\"]"
    exit 1
fi
echo ""

# Step 3: Build .NET library
echo "Step 3: Building .NET library..."
cd "$DOTNET_DIR"
dotnet build -c Release
echo "Done."
echo ""

# Step 4: Run tests (optional)
if [[ "$1" == "--test" ]]; then
    echo "Step 4: Running tests..."
    dotnet test -c Release
    echo ""
fi

echo "=== Build Complete ==="
echo ""
echo "Output:"
echo "  Library: $DOTNET_DIR/bin/Release/net10.0/GoRules.Zen.dll"
echo "  Native:  $RUNTIME_DIR/$LIB_NAME"
echo ""
echo "Supported platforms:"
echo "  - linux-x64"
echo "  - linux-arm64"
echo "  - osx-x64"
echo "  - osx-arm64"
echo "  - win-x64"
echo "  - win-arm64"
echo ""
echo "To create NuGet package:"
echo "  cd $DOTNET_DIR && dotnet pack -c Release"
