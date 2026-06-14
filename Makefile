# xinchao Makefile. Run `./install-prerequisites.sh` once to prepare the system,
# then `make` to build and `make install` to wire it into PAM. `make help` lists targets.

SHELL := bash
.SHELLFLAGS := -eu -o pipefail -c
.ONESHELL:
.DEFAULT_GOAL := build

# Install locations. Override on the command line, e.g. `make install PREFIX=/usr`.
PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
DESKTOPDIR ?= $(PREFIX)/share/applications
ICONDIR ?= $(PREFIX)/share/icons/hicolor/512x512/apps
DOCDIR ?= $(PREFIX)/share/doc/xinchao
SYSTEMDDIR ?= /etc/systemd/system
CONFDIR ?= /etc/xinchao
MODELDIR ?= $(CONFDIR)/models
# PAM service to wire face unlock into (lightdm, gdm-password, sddm, sudo, polkit-1).
SERVICE ?= lightdm
# Directory libpam loads modules from (next to the always-present pam_unix.so).
PAM_MODULE_DIR ?= $(shell find /lib /usr/lib -maxdepth 4 -name pam_unix.so 2>/dev/null | head -n1 | xargs -r dirname)

.PHONY: help build test lint clean install uninstall enable-pam disable-pam enable-autologin disable-autologin

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN { FS = ":.*?## " } { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }'

## --- Development ---

build: ## Build everything, then print how to run it (default)
	@echo ">> Building the workspace (xinchao: CLI + PAM module + lib, and the GUI)"
	cargo build --workspace
	echo
	echo "Build complete. Run from the project root:"
	echo "  GUI:  ./target/debug/xinchao-gui"
	echo "  CLI:  ./target/debug/xinchao --help"
	echo
	echo "For system-wide face auth (PAM): make install"

test: ## Run all tests
	cargo test --workspace

lint: ## Check formatting and run clippy with warnings denied
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

clean: ## Remove build artifacts
	cargo clean

## --- System (Ubuntu/Debian, sudo) ---

install: ## Install the CLI, GUI, PAM module, config, and models ('make install' or 'sudo make install')
	@pam_dir="$(PAM_MODULE_DIR)"
	if [ -z "$$pam_dir" ]; then
		echo "Could not locate the PAM module directory (pam_unix.so not found)." >&2
		echo "Set it, e.g. make install PAM_MODULE_DIR=/usr/lib/x86_64-linux-gnu/security" >&2
		exit 1
	fi
	echo ">> Building release binaries"
	if command -v cargo >/dev/null 2>&1; then
		cargo build --release --workspace
	elif [ "$$(id -u)" = 0 ] && [ -n "$${SUDO_USER:-}" ]; then
		echo "   building as $$SUDO_USER (cargo is installed per-user, not for root)"
		sudo -u "$$SUDO_USER" -H bash -c 'export PATH="$$HOME/.cargo/bin:$$PATH"; cd "$(CURDIR)" && cargo build --release --workspace'
	else
		echo "cargo not found; install the Rust toolchain from https://rustup.rs" >&2
		exit 1
	fi
	echo ">> Installing CLI to $(BINDIR)/xinchao"
	sudo install -D -m 0755 target/release/xinchao "$(BINDIR)/xinchao"
	echo ">> Installing GUI to $(BINDIR)/xinchao-gui"
	sudo install -D -m 0755 target/release/xinchao-gui "$(BINDIR)/xinchao-gui"
	echo ">> Installing desktop entry to $(DESKTOPDIR)/xinchao.desktop"
	sudo install -D -m 0644 packaging/xinchao.desktop "$(DESKTOPDIR)/xinchao.desktop"
	echo ">> Installing app icon to $(ICONDIR)/xinchao.png"
	sudo install -D -m 0644 crates/xinchao-gui/assets/icons/xinchao.png "$(ICONDIR)/xinchao.png"
	sudo gtk-update-icon-cache -q -f -t "$(PREFIX)/share/icons/hicolor" 2>/dev/null || true
	echo ">> Installing PAM module to $$pam_dir/pam_xinchao.so"
	sudo install -D -m 0644 target/release/libxinchao.so "$$pam_dir/pam_xinchao.so"
	echo ">> Ensuring config at $(CONFDIR)/config.toml"
	if [ -e "$(CONFDIR)/config.toml" ]; then
		echo "   exists; leaving it unchanged"
	else
		sudo install -D -m 0644 -o root -g root packaging/xinchao.toml "$(CONFDIR)/config.toml"
	fi
	echo ">> Installing models into $(MODELDIR) (downloads ~260MB on first run)"
	sudo install -d -m 0755 "$(MODELDIR)"
	sudo "$(BINDIR)/xinchao" install-models --dir "$(MODELDIR)"
	echo ">> Installing IR-emitter re-arm service to $(SYSTEMDDIR)/xinchao-ir-emitter.service"
	sudo install -D -m 0644 packaging/systemd/xinchao-ir-emitter.service "$(SYSTEMDDIR)/xinchao-ir-emitter.service"
	echo ">> Installing hands-free unlock service to $(SYSTEMDDIR)/xinchao-unlockd@.service"
	sudo install -D -m 0644 packaging/systemd/xinchao-unlockd@.service "$(SYSTEMDDIR)/xinchao-unlockd@.service"
	sudo install -D -m 0644 docs/HANDS_FREE_UNLOCK.md "$(DOCDIR)/HANDS_FREE_UNLOCK.md"
	sudo systemctl daemon-reload
	target_user="$${SUDO_USER:-$$USER}"
	echo ">> Enabling IR-emitter re-arm at boot"
	sudo systemctl enable --now xinchao-ir-emitter.service \
		|| echo "   enable later with: sudo systemctl enable --now xinchao-ir-emitter.service"
	echo ">> Enabling hands-free unlock for $$target_user"
	sudo systemctl enable --now "xinchao-unlockd@$$target_user" \
		|| echo "   enable later with: sudo systemctl enable --now xinchao-unlockd@<user>"
	echo
	echo "IMPORTANT: run 'sudo xinchao enable-ir --apply' once (looking at the camera)"
	echo "so the emitter activation is saved and re-armed automatically after each reboot."
	echo
	echo "Installed. Launch the app from your menu or run 'xinchao-gui'."
	echo "Lock-screen face unlock (xinchao-unlockd) is now active for $$target_user."
	echo "For no Enter at boot too, run: make enable-autologin (see docs/HANDS_FREE_UNLOCK.md)."
	echo "To accept your face at the login/lock-screen password prompt, run:"
	echo "  make enable-pam SERVICE=lightdm   (gdm-password/sddm/sudo/polkit-1 also work)"
	echo "Read docs/INSTALL.md first and keep a root shell open so a mistake can't lock you out."

uninstall: ## Remove the CLI, GUI, unlock service, and PAM module (leaves config, models, enrollments)
	@pam_dir="$(PAM_MODULE_DIR)"
	target_user="$${SUDO_USER:-$$USER}"
	echo ">> Disabling and removing the hands-free unlock and IR-emitter services"
	sudo systemctl disable --now "xinchao-unlockd@$$target_user" 2>/dev/null || true
	sudo systemctl disable --now xinchao-ir-emitter.service 2>/dev/null || true
	sudo rm -f "$(SYSTEMDDIR)/xinchao-unlockd@.service" "$(SYSTEMDDIR)/xinchao-ir-emitter.service"
	sudo systemctl daemon-reload || true
	echo ">> Removing the CLI, GUI, desktop entry, icon, docs, and PAM module"
	sudo rm -f "$$pam_dir/pam_xinchao.so" "$(BINDIR)/xinchao" "$(BINDIR)/xinchao-gui" \
		"$(DESKTOPDIR)/xinchao.desktop" "$(ICONDIR)/xinchao.png" "$(DOCDIR)/HANDS_FREE_UNLOCK.md"
	echo "Left $(CONFDIR) in place. First remove any pam_xinchao.so lines from /etc/pam.d/*."
	echo "Autologin (if enabled) stays; remove it with: make disable-autologin."

enable-pam: ## Wire face unlock into a PAM service ('make enable-pam SERVICE=lightdm'); keep a root shell open
	@echo ">> Enabling face unlock for PAM service '$(SERVICE)'"
	echo "   WARNING: a bad /etc/pam.d edit can lock you out. Keep a root shell open"
	echo "   (e.g. another terminal logged in as root) and test before relying on it."
	sudo "$(BINDIR)/xinchao" pam enable --service "$(SERVICE)"
	echo "Done. Verify with: xinchao pam status. Undo with: make disable-pam SERVICE=$(SERVICE)"

disable-pam: ## Remove face unlock from a PAM service ('make disable-pam SERVICE=lightdm')
	@echo ">> Disabling face unlock for PAM service '$(SERVICE)'"
	sudo "$(BINDIR)/xinchao" pam disable --service "$(SERVICE)"

enable-autologin: ## Skip the boot login prompt: LightDM autologin for your user (security trade-off)
	@user="$${SUDO_USER:-$$USER}"
	echo ">> Enabling LightDM autologin for $$user"
	sudo install -d -m 0755 /etc/lightdm/lightdm.conf.d
	printf '[Seat:*]\nautologin-user=%s\nautologin-user-timeout=0\n' "$$user" \
		| sudo tee /etc/lightdm/lightdm.conf.d/50-xinchao-autologin.conf >/dev/null
	sudo groupadd -f autologin
	sudo gpasswd -a "$$user" autologin >/dev/null || true
	echo "Autologin enabled for $$user. Reboot to test; undo with: make disable-autologin"
	echo "Security note: anyone who powers on this machine reaches your desktop. The"
	echo "lock screen (xinchao-unlockd) is then the face gate; lock it when you step away."

disable-autologin: ## Remove the LightDM autologin drop-in (restores the boot login prompt)
	@echo ">> Removing LightDM autologin drop-in"
	sudo rm -f /etc/lightdm/lightdm.conf.d/50-xinchao-autologin.conf
	echo "Autologin removed. Reboot to return to the login screen."
