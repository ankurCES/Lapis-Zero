# Lapis-Zero OS — top-level tasks.
# ui/        : Rust lapis-ui (Phase 0: canvas, fonts, SDL sim, --dump-frame)
# daemon/    : Python lapis-daemon via uv (Phase 0: skeleton + --mock)
# os/        : pi-gen image pipeline (Phase 5; targets stubbed until then)

SIM_W ?= 960
SIM_H ?= 510

CARGO     ?= cargo
DUMPS     ?= dumps
VIEWS     ?= companion ticker menu approval prompt setup

.PHONY: help sim test lint fmt fmt-check dump-frames cross-ui build-os manifest clean

help:
	@echo "make sim         - run lapis-ui SDL2 desktop simulator (3x scale, $(SIM_W)x$(SIM_H))"
	@echo "make test        - cargo test + uv run pytest"
	@echo "make lint        - cargo clippy -D warnings + ruff check"
	@echo "make fmt-check   - cargo fmt --check + ruff format --check"
	@echo "make dump-frames - headless 320x170 PNGs of all views into $(DUMPS)/"
	@echo "make cross-ui    - aarch64 release cross-compile (Phase 4)"
	@echo "make build-os    - flashable lapis-zero-arm64.img.xz (Phase 5)"
	@echo "make manifest    - M5 Imager os-list.json (Phase 5)"

sim:
	$(CARGO) run --manifest-path ui/Cargo.toml --release -- --sim

test:
	$(CARGO) test --manifest-path ui/Cargo.toml
	cd daemon && uv run pytest

lint:
	$(CARGO) clippy --manifest-path ui/Cargo.toml --all-targets -- -D warnings
	cd daemon && uv run ruff check .

fmt-check:
	$(CARGO) fmt --manifest-path ui/Cargo.toml --check
	cd daemon && uv run ruff format --check .

dump-frames:
	mkdir -p $(DUMPS)
	@set -e; for v in $(VIEWS); do \
	  $(CARGO) run --manifest-path ui/Cargo.toml --release --no-default-features -- \
	    --dump-frame $(DUMPS)/$$v.png --view $$v; \
	done
	@echo "wrote $(DUMPS)/{companion,ticker,menu,approval,prompt,setup}.png"

# --- Later phases (stubs until their phase starts) ---------------------------
cross-ui:
	@echo "cross-ui: lands in Phase 4 (aarch64-unknown-linux-gnu via zigbuild/cross)"

build-os:
	@echo "build-os: lands in Phase 5 (pi-gen + CardputerZero BSP in Docker)"

manifest:
	@echo "manifest: lands in Phase 5 (os/generate-imager-manifest.py)"

clean:
	$(CARGO) clean --manifest-path ui/Cargo.toml 2>/dev/null || true
	rm -rf daemon/.venv daemon/.pytest_cache daemon/src/lapis_daemon.egg-info
	rm -rf $(DUMPS)
