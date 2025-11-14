#!/bin/bash
set -e

VERSION=${1:-"0.4.1"}
WRAPPER_VERSION="${VERSION}-SNAPSHOT"

echo "===================================="
echo "Building Zen Engine Java Bindings"
echo "Version: $VERSION"
echo "Wrapper Version: $WRAPPER_VERSION"
echo "===================================="

# Check prerequisites
echo ""
echo "Checking prerequisites..."

if ! command -v cargo &> /dev/null; then
    echo "❌ Error: Rust/cargo not found. Please install Rust."
    exit 1
fi

if ! command -v uniffi-bindgen-java &> /dev/null; then
    echo "❌ Error: uniffi-bindgen-java not found."
    echo "   Install it with: cargo install uniffi-bindgen-java"
    exit 1
fi

if [ -z "$DP_NEXUS_USER" ] || [ -z "$DP_NEXUS_PASS" ]; then
    echo "⚠️  Warning: DP_NEXUS_USER or DP_NEXUS_PASS not set."
    echo "   Publishing to Nexus may fail."
    read -p "Continue anyway? (y/n) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

echo "✓ All prerequisites met"

# Save current directory
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Navigate to uniffi directory
cd "$REPO_ROOT/bindings/uniffi"

echo ""
echo "[1/6] Building Rust library..."
make build

echo ""
echo "[2/6] Generating Java bindings..."
make generate-java

echo ""
echo "[3/6] Building zen-engine JAR..."
./gradlew clean generateJavaJar

echo ""
echo "[4/6] Publishing zen-engine to Nexus..."
./gradlew publishMavenJavaPublicationToMavenRepository

echo ""
echo "[5/6] Installing zen-engine to mavenLocal..."
./gradlew publishToMavenLocal

# Navigate to java wrapper directory
cd "$REPO_ROOT/bindings/java"

echo ""
echo "[6/6] Building and publishing zen-engine-java wrapper..."
./gradlew clean build publish

echo ""
echo "===================================="
echo "✓ Build Complete!"
echo "===================================="
echo ""
echo "Published artifacts:"
echo "  • io.gorules:zen-engine:$VERSION"
echo "  • cred.club.java-rules:zen-engine-java:$WRAPPER_VERSION"
echo ""
echo "Verify publication:"
echo "  curl -I https://nexus.infra.dreamplug.net/repository/maven-releases/io/gorules/zen-engine/$VERSION/zen-engine-$VERSION.jar"
echo "  curl -I https://nexus.infra.dreamplug.net/repository/maven-snapshots/cred/club/java-rules/zen-engine-java/$WRAPPER_VERSION/maven-metadata.xml"
echo ""
