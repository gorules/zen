BENCH_ITERS ?= 500
export BENCH_ITERS

ROOT := $(CURDIR)/..
VENV := $(CURDIR)/.venv
PY := $(VENV)/bin/python
BIN := $(ROOT)/target/release/zen-benchmark

.PHONY: all compare fixtures rust node python build-rust build-node build-python

all: rust node python

fixtures:
	@node gen.mjs

compare:
	@node compare.mjs

rust: build-rust
	@printf '\n=== rust ===\n'
	@$(BIN) manifest.json

node: build-node
	@printf '\n=== node ===\n'
	@node node/bench.mjs

python: build-python
	@printf '\n=== python ===\n'
	@$(PY) python/bench.py

build-rust:
	@cargo build --release -q -p zen-benchmark

build-node:
	@cd $(ROOT)/bindings/nodejs && yarn install --immutable --mode=skip-build && yarn build

build-python:
	@test -d $(VENV) || python3 -m venv $(VENV)
	@$(PY) -m pip install --quiet --upgrade pip maturin
	@rm -rf $(CURDIR)/.dist && $(PY) -m maturin build --release --out $(CURDIR)/.dist -m $(ROOT)/bindings/python/Cargo.toml
	@$(PY) -m pip install --quiet --force-reinstall $(CURDIR)/.dist/*.whl
