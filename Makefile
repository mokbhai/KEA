# KEA - Tauri app commands

.PHONY: all build clean install dev test lint fmt-check hygiene pre-commit-check install-hooks reset-perms release-check release-package set-version help tauri-build tauri-dev tauri-install tauri-test tauri-lint check-tauri smoke

APP_NAME = KEA
BUNDLE_ID = ai.kea.desktop
DIST_DIR = dist
TAURI_CLI = cargo tauri
APP_INSTALL_PATH = /Applications/$(APP_NAME).app
# Extra flags forwarded to `cargo tauri build` (release packaging passes
# --features updater through here so the signing logic below still applies).
TAURI_BUILD_FLAGS ?=

# Cargo features chosen by the build HOST rather than baked into `default`.
# whisper.cpp's Metal backend links macOS-only frameworks, and `default` is
# shared with the Linux and Windows CI builds, so it cannot go there. The
# release workflow makes the same choice per runner (.github/workflows/release.yml).
# Override with `make build HOST_FEATURES=` to force a CPU build.
ifeq ($(shell uname -s),Darwin)
HOST_FEATURES ?= whisper-metal
else
HOST_FEATURES ?=
endif
ifeq ($(strip $(HOST_FEATURES)),)
HOST_FEATURE_FLAGS =
else
HOST_FEATURE_FLAGS = --features $(HOST_FEATURES)
endif

all: build

check-tauri:
	@cargo tauri --version >/dev/null 2>&1 || { \
		echo "Tauri CLI is not installed."; \
		echo "Install it with: cargo install tauri-cli --version '^2' --locked"; \
		exit 1; \
	}

build: tauri-build

# tauri.conf.json carries an updater pubkey, and the bundler REFUSES to build
# when it finds one without a matching private key ("A public key has been
# found, but no private key"). Local and CI bundle builds must not depend on the
# release signing key, so fall back to --no-sign when it is absent. The release
# workflow exports TAURI_SIGNING_PRIVATE_KEY and therefore takes the signed path,
# which is what emits the *.app.tar.gz.sig the updater manifest needs.
#
# Only TAURI_SIGNING_PRIVATE_KEY is checked: the bundler's own guard names that
# variable specifically, and setting TAURI_SIGNING_PRIVATE_KEY_PATH instead is
# NOT enough to get past it. Its value may be either the key contents or a path.
#
# macOS Apple Code Signing:
# APPLE_SIGNING_IDENTITY is resolved before invoking tauri build so the resulting
# .app has a stable designated requirement. This preserves macOS TCC grants
# (Accessibility) across rebuilds instead of revoking them on a cdhash signature.
tauri-build: check-tauri
	@if [ "$$(uname -s)" = "Darwin" ]; then \
		export APPLE_SIGNING_IDENTITY=$$("$(CURDIR)/scripts/macos_identity.sh"); \
		if [ $$? -ne 0 ]; then exit 1; fi; \
		echo "macOS build will sign using identity: $$APPLE_SIGNING_IDENTITY"; \
	fi; \
	if [ -n "$$TAURI_SIGNING_PRIVATE_KEY" ]; then \
		echo "Signing key present - building signed updater artifacts."; \
		$(TAURI_CLI) build $(HOST_FEATURE_FLAGS) $(TAURI_BUILD_FLAGS); \
	else \
		echo "No TAURI_SIGNING_PRIVATE_KEY - building with --no-sign (no updater signature)."; \
		$(TAURI_CLI) build --no-sign $(HOST_FEATURE_FLAGS) $(TAURI_BUILD_FLAGS); \
	fi

dev: tauri-dev

tauri-dev: check-tauri
	$(TAURI_CLI) dev $(HOST_FEATURE_FLAGS)

install: tauri-install

# Deliberately does NOT reset the TCC prompts. It used to, and that made every
# install silently break hold-to-talk: `tccutil reset Accessibility` revokes the
# grant, and an untrusted process still creates its CGEventTap successfully —
# the system just kills it on every keystroke. The symptom is a chord that does
# nothing while Cmd+Shift+D keeps working, which reads like a code bug and is
# not one. `make reset-perms` exists for when clearing them is what you meant.
tauri-install: tauri-build
	@APP_PATH="$$(find "$(CURDIR)/target/release/bundle/macos" "$(CURDIR)/src-tauri/target/release/bundle/macos" -maxdepth 1 -name "$(APP_NAME).app" -type d 2>/dev/null | head -n 1)"; \
	if [ -z "$$APP_PATH" ]; then \
		echo "Expected Tauri app bundle named $(APP_NAME).app under target/release/bundle/macos or src-tauri/target/release/bundle/macos"; \
		exit 1; \
	fi; \
	rm -rf "$(APP_INSTALL_PATH)"; \
	ditto "$$APP_PATH" "$(APP_INSTALL_PATH)"; \
	if [ "$$(uname -s)" = "Darwin" ]; then \
		IDENTITY=$$("$(CURDIR)/scripts/macos_identity.sh"); \
		if [ -n "$$IDENTITY" ]; then \
			codesign --force --deep --sign "$$IDENTITY" "$(APP_INSTALL_PATH)"; \
		else \
			codesign --force --deep --sign - "$(APP_INSTALL_PATH)"; \
		fi; \
	fi; \
	echo "Installed $$APP_PATH to $(APP_INSTALL_PATH)"; \
	echo "The first certificate-signed install may require one Accessibility re-grant; later installs preserve it."

test: tauri-test

tauri-test:
	npm --prefix ui run build
	cargo test --workspace

lint: tauri-lint

tauri-lint:
	npm --prefix ui run typecheck
	cargo check --workspace
	./scripts/check_kea_hygiene.sh

fmt-check:
	cargo fmt --all -- --check

hygiene:
	./scripts/check_kea_hygiene.sh

pre-commit-check: release-check

install-hooks:
	git config core.hooksPath .githooks
	chmod +x .githooks/pre-commit scripts/check_kea_hygiene.sh
	@echo "Installed git hooks from .githooks"

reset-perms:
	@./scripts/reset_permissions.sh

clean:
	rm -rf target src-tauri/target ui/dist "$(DIST_DIR)"

release-check: lint test build

# package_release.sh drives `cargo tauri build` itself rather than calling back
# into `make build`: the release matrix runs it on a Windows runner too, where
# GNU make is not guaranteed to exist. The signed/--no-sign selection that used
# to live only in tauri-build is duplicated there for the same reason.
release-package:
	./scripts/package_release.sh

set-version:
	@./scripts/set_version.sh $(VERSION)

smoke:
	./scripts/smoke_launch.sh

help:
	@echo "KEA - Tauri rewrite and speech utility"
	@echo ""
	@echo "Targets:"
	@echo "  build          - Build the Tauri app bundle"
	@echo "  dev            - Run the Tauri development app"
	@echo "  install        - Build and install KEA.app to /Applications"
	@echo "  test           - Build the UI and run Rust workspace tests"
	@echo "  lint           - Run TypeScript, Rust compile, and active-doc hygiene checks"
	@echo "  fmt-check      - Run rustfmt check across the workspace"
	@echo "  clean          - Remove Tauri, UI, and release artifacts"
	@echo "  install-hooks  - Install pre-commit hooks for this checkout"
	@echo "  reset-perms    - Reset macOS TCC permissions for KEA"
	@echo "  release-check  - Run lint, tests, and a Tauri build"
	@echo "  release-package - Build release artifacts into dist/"
	@echo "  smoke          - Launch built binary and verify it does not panic/exist early (CI guardrail)"
	@echo "  set-version    - Set Tauri/Cargo app version (make set-version VERSION=x.y.z)"
	@echo ""
	@echo "Prerequisite:"
	@echo "  cargo install tauri-cli --version '^2' --locked"
