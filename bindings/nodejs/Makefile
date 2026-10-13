REGISTRY := "http://localhost:4873"
WASI_SDK ?= $(HOME)/.cache/wasi/wasi-sdk-27.0-arm64-macos

wasm:
	cargo clean -p napi-async-runtime --target wasm32-wasip1 --release
	RUSTC=$$(rustup which rustc) \
	CC_wasm32_wasip1=$(WASI_SDK)/bin/clang \
	AR_wasm32_wasip1=$(WASI_SDK)/bin/llvm-ar \
	CFLAGS_wasm32_wasip1="--target=wasm32-wasip1 --sysroot=$(WASI_SDK)/share/wasi-sysroot -mllvm -wasm-enable-sjlj" \
	CARGO_TARGET_WASM32_WASIP1_RUSTFLAGS="-C link-arg=$(WASI_SDK)/share/wasi-sysroot/lib/wasm32-wasip1/libsetjmp.a" \
	npx napi build --platform --release --target wasm32-wasip1
	mkdir -p npm/wasm32-wasip1
	cp zen-engine.wasm32-wasip1.wasm zen-engine.wasip1* npm/wasm32-wasip1/

darwinArm:
	yarn build -- --target aarch64-apple-darwin
	mkdir -p npm/darwin-arm64
	cp zen-engine.darwin-arm64.node npm/darwin-arm64/

prepareLocalPublish:
	rm -f zen-engine.wasm32-wasip1.*.mjs zen-engine.wasm32-wasip1.*.mts
	yarn napi prepublish --skip-optional-publish
	yarn createNpmDirs

publish:
	cd npm/wasm32-wasip1 && npm publish --registry $(REGISTRY) --ignore-scripts --tag latest
	cd npm/darwin-arm64 && npm publish --registry $(REGISTRY) --ignore-scripts --tag latest
	npm publish --registry $(REGISTRY) --ignore-scripts --tag latest

publishLocal: prepareLocalPublish wasm darwinArm publish
