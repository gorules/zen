#!/bin/bash

# Dreamplug Nexus Publishing Script
# Usage: ./publish.sh

# Check if credentials are set
if [ -z "$DP_NEXUS_USER" ] || [ -z "$DP_NEXUS_PASS" ]; then
    echo "❌ Error: Nexus credentials not set!"
    echo ""
    echo "Please set the following environment variables:"
    echo "  export DP_NEXUS_USER=\"your-username\""
    echo "  export DP_NEXUS_PASS=\"your-password\""
    echo ""
    echo "Or add them to your ~/.zshrc or ~/.bashrc:"
    echo "  echo 'export DP_NEXUS_USER=\"your-username\"' >> ~/.zshrc"
    echo "  echo 'export DP_NEXUS_PASS=\"your-password\"' >> ~/.zshrc"
    echo "  source ~/.zshrc"
    exit 1
fi

echo "✅ Credentials found"
echo "   User: $DP_NEXUS_USER"
echo ""

# Read version from gradle.properties
VERSION=$(grep "global.version" gradle.properties | cut -d'=' -f2)
echo "📦 Publishing version: $VERSION"
echo ""

# Determine repository type
if [[ $VERSION == *"-SNAPSHOT"* ]]; then
    REPO="maven-snapshots"
else
    REPO="maven-releases"
fi

echo "📍 Repository: https://nexus.infra.dreamplug.net/repository/$REPO/"
echo ""

# Clean and build
echo "🔨 Building project..."
./gradlew clean build

if [ $? -ne 0 ]; then
    echo "❌ Build failed!"
    exit 1
fi

echo ""
echo "📤 Publishing to Dreamplug Nexus..."
./gradlew publish

if [ $? -eq 0 ]; then
    echo ""
    echo "✅ Published successfully!"
    echo ""
    echo "📍 Artifact location:"
    echo "   https://nexus.infra.dreamplug.net/repository/$REPO/cred/club/java-rules/zen-engine-java/$VERSION/"
    echo ""
    echo "📦 To use this artifact in another project, add:"
    echo ""
    echo "   repositories {"
    echo "       maven {"
    echo "           url \"https://nexus.infra.dreamplug.net/repository/maven-public\""
    echo "       }"
    echo "   }"
    echo ""
    echo "   dependencies {"
    echo "       implementation 'io.github.java-rules:zen-engine-java:$VERSION'"
    echo "   }"
else
    echo ""
    echo "❌ Publishing failed!"
    exit 1
fi
