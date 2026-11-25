# Lapis-Zero OS — top-level tasks.
# ui/        : Rust lapis-ui (320x170 renderer, SDL sim, --dump-frame, CM0 backends)
# daemon/    : Python lapis-daemon via uv (agent loop, control plane, BLE provisioning)
# android/   : native companion app (Gradle)
# os/        : pi-gen image pipeline

SIM_W ?= 960
SIM_H ?= 510

CARGO     ?= cargo
DUMPS     ?= dumps
VIEWS     ?= companion ticker menu approval prompt setup pairing_pin pairing_qr

.PHONY: help sim mock test test-android lint fmt fmt-check dump-frames cross-ui build-os manifest \
	vectors apk deploy-android clean

help:
	@echo "make sim         - run lapis-ui SDL2 desktop simulator (3x scale, $(SIM_W)x$(SIM_H))"
	@echo "make test        - cargo test + uv run pytest"
	@echo "make lint        - cargo clippy -D warnings + ruff check"
	@echo "make fmt-check   - cargo fmt --check + ruff format --check"
	@echo "make dump-frames - headless 320x170 PNGs of all views into $(DUMPS)/"
	@echo "make cross-ui    - aarch64 release cross-compile (Phase 4)"
	@echo "make build-os    - flashable lapis-zero-arm64.img.xz (Phase 5)"
	@echo "make manifest    - M5 Imager os-list.json (Phase 5)"
	@echo "make mock        - lapis-daemon --mock with a dev pairing token (port 8421)"
	@echo "make test-android- Android JVM unit tests + lint"
	@echo "make apk         - Android debug APK"
	@echo "make deploy-android - adb install -r the debug APK and launch it"
	@echo "make vectors     - regenerate tests/vectors/ble_crypto_vectors.json"

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
	@echo "wrote $(DUMPS)/{companion,ticker,menu,approval,prompt,setup,pairing_pin,pairing_qr}.png"

# Cross-compile the CM0 binary: no SDL (`sim` off) so it links fbdev/evdev only.
# Zig provides the aarch64-unknown-linux-gnu toolchain + libc, so no Docker/cross
# image is needed. Output is stripped to keep the flash footprint small.
TARGET_TRIPLE ?= aarch64-unknown-linux-gnu
ZIGBUILD     ?= cargo-zigbuild
OUT_BIN      ?= ui/target/$(TARGET_TRIPLE)/release/lapis-ui
# Strip with Rust's own llvm-strip (installed via `rustup component add llvm-tools-preview`);
# it reads/writes aarch64 ELF on any host, so no GNU binutils is needed.
LLVM_STRIP   ?= $(shell ls $(shell rustc --print sysroot)/lib/rustlib/*/bin/llvm-strip 2>/dev/null | head -1)

cross-ui:
	rustup target add $(TARGET_TRIPLE) >/dev/null
	$(ZIGBUILD) build --manifest-path ui/Cargo.toml \
		--release --no-default-features \
		--target $(TARGET_TRIPLE)
	@if [ -n "$(LLVM_STRIP)" ] && [ -f "$(LLVM_STRIP)" ]; then \
		"$(LLVM_STRIP)" --strip-all "$(OUT_BIN)"; \
		echo "stripped $(OUT_BIN)"; \
	else \
		echo "llvm-strip not found; leaving $(OUT_BIN) unstripped"; \
	fi
	@test -f $(OUT_BIN) && ls -l $(OUT_BIN)

build-os:
	$(MAKE) -C os deploy

# Development daemon for the simulator and the Android app (emulator: 10.0.2.2:8421,
# phone over USB: `adb reverse tcp:8421 tcp:8421`, then Dev connect to 127.0.0.1:8421).
mock:
	cd daemon && uv run lapis-daemon --mock --dev-pair-token dev-local-token

test-android:
	cd android && ./gradlew testDebugUnitTest lintDebug

apk:
	cd android && ./gradlew assembleDebug
	@ls -l android/app/build/outputs/apk/debug/app-debug.apk

deploy-android:
	./scripts/deploy_android.sh

# Frozen cross-language BLE vectors (Python and Kotlin both test against the file).
vectors:
	cd daemon && uv run python ../scripts/gen_ble_vectors.py

manifest:
	$(MAKE) -C os manifest

clean:
	$(CARGO) clean --manifest-path ui/Cargo.toml 2>/dev/null || true
	rm -rf daemon/.venv daemon/.pytest_cache daemon/src/lapis_daemon.egg-info
	rm -rf $(DUMPS)
