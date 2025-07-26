# Just for local testing, at the moment it relies on .dylib (Mac), feel free to change below
build:
	cargo build --lib --release
	@mkdir -p build/generated/resources
	cp -f ../../target/release/libzen_uniffi.dylib build/generated/resources/libzen_uniffi.dylib

generate-java:
	uniffi-bindgen-java generate \
		--library build/generated/resources/libzen_uniffi.dylib \
		--out-dir build/generated/java

generate-kotlin:
	cargo run --bin uniffi-bindgen generate \
		--library build/generated/resources/libzen_uniffi.dylib \
		--language kotlin \
		--out-dir build/generated/kotlin

all: build generate-java generate-kotlin

.PHONY: all build generate-java generate-kotlin