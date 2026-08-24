TARGET_DIR ?= $(shell cargo metadata --format-version=1 --no-deps 2>/dev/null | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

UNAME_S := $(shell uname -s)
UNAME_M := $(shell uname -m)
ifeq ($(UNAME_S),Darwin)
  LIB_NAME := libzen_uniffi.dylib
  ifeq ($(UNAME_M),arm64)
    PLATFORM_DIR := darwin-aarch64
  else
    PLATFORM_DIR := darwin-x86-64
  endif
else
  LIB_NAME := libzen_uniffi.so
  ifeq ($(UNAME_M),aarch64)
    PLATFORM_DIR := linux-aarch64
  else
    PLATFORM_DIR := linux-x86-64
  endif
endif

DEV_DIR := $(shell xcode-select -p 2>/dev/null)
ifeq ($(DEV_DIR),/Library/Developer/CommandLineTools)
  TESTING_FRAMEWORKS := $(DEV_DIR)/Library/Developer/Frameworks
  SWIFT_TEST_FLAGS := -Xswiftc -F$(TESTING_FRAMEWORKS) \
    -Xlinker -F$(TESTING_FRAMEWORKS) \
    -Xlinker -rpath -Xlinker $(TESTING_FRAMEWORKS) \
    -Xlinker -rpath -Xlinker $(DEV_DIR)/Library/Developer/usr/lib
endif

build:
	cargo build --lib --release
	@mkdir -p build/generated/resources/$(PLATFORM_DIR)
	cp -f $(TARGET_DIR)/release/$(LIB_NAME) build/generated/resources/$(LIB_NAME)
	cp -f $(TARGET_DIR)/release/$(LIB_NAME) build/generated/resources/$(PLATFORM_DIR)/$(LIB_NAME)

generate-java:
	uniffi-bindgen-java generate \
		--library build/generated/resources/$(LIB_NAME) \
		--out-dir build/generated/java

generate-kotlin:
	cargo run --bin uniffi-bindgen generate \
		--library build/generated/resources/$(LIB_NAME) \
		--language kotlin \
		--out-dir build/generated/kotlin

generate-csharp:
	uniffi-bindgen-cs \
		--library build/generated/resources/$(LIB_NAME) \
		--config uniffi.toml \
		--out-dir build/generated/csharp

generate-swift:
	cargo run --bin uniffi-bindgen generate \
		--library build/generated/resources/$(LIB_NAME) \
		--language swift \
		--out-dir build/generated/swift

test-java:
	./gradlew testJava

test-kotlin:
	./gradlew testKotlin

test-csharp:
	dotnet test tests/csharp

test-swift:
	@mkdir -p tests/swift/Sources/ZenUniffi tests/swift/Sources/zen_uniffiFFI
	cp -f build/generated/swift/zen_uniffi.swift tests/swift/Sources/ZenUniffi/
	cp -f build/generated/swift/zen_uniffiFFI.h tests/swift/Sources/zen_uniffiFFI/
	cp -f build/generated/swift/zen_uniffiFFI.modulemap tests/swift/Sources/zen_uniffiFFI/module.modulemap
	cd tests/swift && swift test $(SWIFT_TEST_FLAGS) -Xlinker $(abspath $(TARGET_DIR))/release/libzen_uniffi.a

all: build generate-java generate-kotlin generate-csharp generate-swift

test: test-java test-kotlin test-csharp test-swift

.PHONY: all build generate-java generate-kotlin generate-csharp generate-swift test test-java test-kotlin test-csharp test-swift
