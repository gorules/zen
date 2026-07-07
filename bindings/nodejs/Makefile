REGISTRY := "http://localhost:4873"

wasm:
	yarn build -- --target wasm32-wasip1-threads
	mkdir -p npm/wasm32-wasi
	cp zen-engine.wasm32-wasi.wasm zen-engine.wasi.cjs zen-engine.wasi-browser.js wasi-worker.mjs wasi-worker-browser.mjs npm/wasm32-wasi/

darwinArm:
	yarn build -- --target aarch64-apple-darwin
	mkdir -p npm/darwin-arm64
	cp zen-engine.darwin-arm64.node npm/darwin-arm64/

prepareLocalPublish:
	yarn napi prepublish --skip-optional-publish
	yarn createNpmDirs

publish:
	cd npm/wasm32-wasi && npm publish --registry $(REGISTRY) --ignore-scripts --tag latest
	cd npm/darwin-arm64 && npm publish --registry $(REGISTRY) --ignore-scripts --tag latest
	npm publish --registry $(REGISTRY) --ignore-scripts --tag latest

publishLocal: prepareLocalPublish wasm darwinArm publish
