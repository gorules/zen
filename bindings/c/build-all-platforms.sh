#!/bin/bash
# build-all-platforms.sh
# 使用 cross 工具交叉编译所有平台
# 安装: cargo install cross

set -e

TARGETS=(
    "x86_64-pc-windows-gnu"      # Windows x64
    "x86_64-unknown-linux-gnu"   # Linux x64
    # macOS 交叉编译需要额外配置，通常在 CI 中原生构建
)

OUTPUT_DIR="target/native-libs"
mkdir -p "$OUTPUT_DIR"

for target in "${TARGETS[@]}"; do
    echo "Building for $target..."
    cross build --release -p zen-ffi --no-default-features --target "$target"
    
    # 复制产物
    case "$target" in
        *windows*)
            cp "target/$target/release/zen_ffi.dll" "$OUTPUT_DIR/zen_ffi-$target.dll"
            ;;
        *linux*)
            cp "target/$target/release/libzen_ffi.so" "$OUTPUT_DIR/libzen_ffi-$target.so"
            ;;
        *darwin*)
            cp "target/$target/release/libzen_ffi.dylib" "$OUTPUT_DIR/libzen_ffi-$target.dylib"
            ;;
    esac
done

echo "All builds complete! Output in $OUTPUT_DIR"
ls -la "$OUTPUT_DIR"
