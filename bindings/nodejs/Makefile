wasm:
	yarn exec napi build --target wasm32-wasip1-threads --output-dir npm/wasm32-wasi --release
	mv npm/wasm32-wasi/browser.js .
	mv npm/wasm32-wasi/zen-engine.wasm npm/wasm32-wasi/zen-engine.wasm32-wasi.wasm