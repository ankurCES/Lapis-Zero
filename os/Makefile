# os/ Makefile — Lapis-Zero OS image pipeline tasks.
# `deploy` runs the real pi-gen build (requires Docker); `lint` is offline.

REPO_PATH      ?=
PIGEN_BRANCH   ?= cardputerzero_v1.1
DEPLOY_DIR     ?= $(CURDIR)/deploy
LAPIS_UI_BIN   ?=
LAPIS_DAEMON_WHEEL ?=

.PHONY: deploy build lint manifest self-test clean

build: deploy

deploy:
	bash ./build-image.sh

lint:
	shellcheck build-image.sh \
		stage-lapis/files/*.sh \
		stage-lapis/files/st7789-firmware \
		stage-lapis/prerun.sh \
		stage-lapis/*/*.sh
	python3 generate-imager-manifest.py --self-test

manifest:
	LAPIS_IMG_XZ="$(DEPLOY_DIR)/lapis-zero-arm64.img.xz" \
	LAPIS_IMG_RAW="$(DEPLOY_DIR)/lapis-zero-arm64.img" \
		python3 generate-imager-manifest.py -o "$(DEPLOY_DIR)/os-list.json"

self-test:
	python3 generate-imager-manifest.py --self-test

clean:
	rm -rf build deploy
