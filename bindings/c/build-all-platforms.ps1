# build-all-platforms.ps1
# Windows PowerShell 脚本 - 使用 cross 或原生工具链构建
# 前提: cargo install cross (需要 Docker)

$ErrorActionPreference = "Stop"

# 定义目标平台
$targets = @(
    @{ target = "x86_64-pc-windows-msvc"; lib = "zen_ffi.dll"; runtime = "win-x64" }
    @{ target = "x86_64-pc-windows-gnu"; lib = "zen_ffi.dll"; runtime = "win-x64-gnu" }
)

# 如果有 cross 和 Docker，可以添加 Linux 目标
# @{ target = "x86_64-unknown-linux-gnu"; lib = "libzen_ffi.so"; runtime = "linux-x64" }

$outputDir = "target/native-libs"
New-Item -ItemType Directory -Force -Path $outputDir | Out-Null

foreach ($t in $targets) {
    Write-Host "Building for $($t.target)..." -ForegroundColor Cyan
    
    # 添加目标工具链
    rustup target add $t.target
    
    # 构建
    cargo build --release -p zen-ffi --no-default-features --target $t.target
    
    # 复制产物
    $src = "target/$($t.target)/release/$($t.lib)"
    $dst = "$outputDir/$($t.runtime)"
    New-Item -ItemType Directory -Force -Path $dst | Out-Null
    Copy-Item $src "$dst/$($t.lib)" -Force
    
    Write-Host "  -> $dst/$($t.lib)" -ForegroundColor Green
}

Write-Host "`nAll builds complete!" -ForegroundColor Green
Get-ChildItem -Recurse $outputDir | Format-Table Name, Length
