#Requires -Version 5.1
<#
.SYNOPSIS
    Build script for Zen Engine .NET Bindings on Windows.

.DESCRIPTION
    This script builds the native Rust library and .NET bindings for the Zen Rules Engine.

.PARAMETER Test
    Run tests after building.

.PARAMETER Configuration
    Build configuration (Debug or Release). Default is Release.

.EXAMPLE
    .\build.ps1
    Build with default settings (Release configuration).

.EXAMPLE
    .\build.ps1 -Test
    Build and run tests.

.EXAMPLE
    .\build.ps1 -Configuration Debug -Test
    Build in Debug mode and run tests.
#>

param(
    [switch]$Test,
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Release"
)

$ErrorActionPreference = "Stop"

# Get script directories
$ScriptDir = $PSScriptRoot
$RootDir = Resolve-Path (Join-Path $ScriptDir "..\..") | Select-Object -ExpandProperty Path
$CBindingsDir = Join-Path $RootDir "bindings\c"
$DotNetDir = $ScriptDir

Write-Host "=== Building Zen Engine .NET Bindings ===" -ForegroundColor Cyan
Write-Host ""

# Detect architecture
$Arch = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture
if ($Arch -eq [System.Runtime.InteropServices.Architecture]::Arm64) {
    $Platform = "win-arm64"
} else {
    $Platform = "win-x64"
}
$LibName = "zen_ffi.dll"

Write-Host "Platform: $Platform" -ForegroundColor Green
Write-Host "Architecture: $Arch" -ForegroundColor Green
Write-Host "Library: $LibName" -ForegroundColor Green
Write-Host "Configuration: $Configuration" -ForegroundColor Green
Write-Host ""

# Step 1: Build Rust library
Write-Host "Step 1: Building Rust C bindings..." -ForegroundColor Yellow
Push-Location $CBindingsDir
try {
    cargo build --release --no-default-features
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo build failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}
Write-Host "Done." -ForegroundColor Green
Write-Host ""

# Step 2: Copy native library
Write-Host "Step 2: Copying native library..." -ForegroundColor Yellow
$RuntimeDir = Join-Path $DotNetDir "runtimes\$Platform\native"
if (-not (Test-Path $RuntimeDir)) {
    New-Item -ItemType Directory -Path $RuntimeDir -Force | Out-Null
}

$SourceLib = Join-Path $RootDir "target\release\$LibName"
if (Test-Path $SourceLib) {
    Copy-Item $SourceLib -Destination $RuntimeDir -Force
    Write-Host "Copied $LibName to $RuntimeDir\" -ForegroundColor Green
} else {
    Write-Host "ERROR: Library not found at $SourceLib" -ForegroundColor Red
    Write-Host "Make sure Cargo.toml has crate-type = [""cdylib""]" -ForegroundColor Red
    exit 1
}
Write-Host ""

# Step 3: Build .NET library
Write-Host "Step 3: Building .NET library..." -ForegroundColor Yellow
Push-Location $DotNetDir
try {
    dotnet build -c $Configuration
    if ($LASTEXITCODE -ne 0) {
        throw "dotnet build failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}
Write-Host "Done." -ForegroundColor Green
Write-Host ""

# Step 4: Run tests (optional)
if ($Test) {
    Write-Host "Step 4: Running tests..." -ForegroundColor Yellow
    Push-Location $DotNetDir
    try {
        dotnet test -c $Configuration
        if ($LASTEXITCODE -ne 0) {
            throw "dotnet test failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
    Write-Host ""
}

Write-Host "=== Build Complete ===" -ForegroundColor Cyan
Write-Host ""
Write-Host "Output:" -ForegroundColor White
Write-Host "  Library: $DotNetDir\bin\$Configuration\net10.0\GoRules.Zen.dll"
Write-Host "  Native:  $RuntimeDir\$LibName"
Write-Host ""
Write-Host "Supported platforms:" -ForegroundColor White
Write-Host "  - linux-x64"
Write-Host "  - linux-arm64"
Write-Host "  - osx-x64"
Write-Host "  - osx-arm64"
Write-Host "  - win-x64"
Write-Host "  - win-arm64"
Write-Host ""
Write-Host "To create NuGet package:" -ForegroundColor White
Write-Host "  cd $DotNetDir; dotnet pack -c $Configuration"
