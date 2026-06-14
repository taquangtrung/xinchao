#!/usr/bin/env bash
#
# xinchao install-prerequisites.sh - prepare the system to build and run xinchao.
#
# Installs the build prerequisites and the linux-enable-ir-emitter tool, then
# (optionally) activates the IR illuminator, which on many laptops is dark until
# a vendor control sequence is sent. After this, run:
#
#     make           # build the workspace and the GUI
#     make install   # install the CLI, PAM module, config, and models (uses sudo)
#
# 'make install' or 'sudo make install' both work (the build runs as your user
# either way). Wiring PAM is a deliberately manual step; see docs/INSTALL.md.

set -euo pipefail

# Constants

# apt packages needed to build xinchao and diagnose the camera.
readonly APT_PACKAGES=(v4l-utils clang linux-libc-dev curl ca-certificates)
# GitHub repo shipping linux-enable-ir-emitter release tarballs.
readonly IR_EMITTER_REPO="EmixamPP/linux-enable-ir-emitter"
# Where a successful emitter activation persists its config.
readonly IR_EMITTER_CONFIG_DIR="/etc/linux-enable-ir-emitter"

# Recognition models, downloaded up front so the first enrollment is not stalled
# by a ~260 MB fetch. Each entry is "name|url|sha256" and mirrors the pinned spec
# in crates/xinchao; the Rust loader re-verifies and re-fetches on any drift,
# so these are a safe convenience copy.
readonly MODEL_DIR="/etc/xinchao/models"
readonly MODELS=(
	"arcfaceresnet100-8.onnx|https://github.com/onnx/models/raw/main/validated/vision/body_analysis/arcface/model/arcfaceresnet100-8.onnx|f3a6bc281e72f88862f5748b53be3d76b3b48f8f1ab1f4a537941bdc4e1b01da"
	"face_recognition_sface_2021dec.onnx|https://github.com/opencv/opencv_zoo/raw/main/models/face_recognition_sface/face_recognition_sface_2021dec.onnx|0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79"
	"version-RFB-320.onnx|https://github.com/onnx/models/raw/main/validated/vision/body_analysis/ultraface/models/version-RFB-320.onnx|34cd7e60aeff28744c657de7a3dc64e872d506741de66987f3426f2b79f88017"
)

# Whether to skip IR-emitter activation (set by --skip-emitter).
skip_emitter=0
# Whether to skip the model download (set by --skip-models).
skip_models=0

# Scratch directory for downloads and capture probes, removed on exit.
readonly WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

# Functions

log() {
	printf '>> %s\n' "$*"
}

err() {
	printf '%s\n' "$*" >&2
}

usage() {
	cat <<'EOF'
Usage: ./install-prerequisites.sh [--skip-emitter] [--skip-models]

Prepares the system for xinchao on the Ubuntu/Debian family: installs build
prerequisites and the linux-enable-ir-emitter tool, downloads the recognition
models, and optionally activates the IR illuminator.

Options:
  --skip-emitter   Do not activate the IR emitter.
  --skip-models    Do not download the recognition models (~260 MB).
  -h, --help       Show this help and exit.

After configuring:  make && make install
EOF
}

parse_args() {
	while [[ $# -gt 0 ]]; do
		case "$1" in
		--skip-emitter) skip_emitter=1 ;;
		--skip-models) skip_models=1 ;;
		-h | --help)
			usage
			exit 0
			;;
		*)
			err "Unknown option: $1"
			usage
			exit 1
			;;
		esac
		shift
	done
}

require_apt() {
	if ! command -v apt-get >/dev/null 2>&1; then
		err "This installer targets the Ubuntu/Debian family, but apt-get was not found."
		err "Install manually: ${APT_PACKAGES[*]} and linux-enable-ir-emitter, then run 'make'."
		exit 1
	fi
}

install_packages() {
	log "Installing apt packages: ${APT_PACKAGES[*]}"
	sudo apt-get update
	sudo apt-get install -y "${APT_PACKAGES[@]}"
}

install_ir_emitter() {
	if command -v linux-enable-ir-emitter >/dev/null 2>&1; then
		log "linux-enable-ir-emitter already installed ($(command -v linux-enable-ir-emitter))"
		return
	fi
	log "Resolving the latest ${IR_EMITTER_REPO} release"
	local url
	url=$(curl -fsSL "https://api.github.com/repos/${IR_EMITTER_REPO}/releases/latest" |
		grep -oE 'https://[^"]+\.tar\.gz' | head -n1)
	if [[ -z "$url" ]]; then
		err "Could not resolve a release tarball URL (GitHub API rate limit?)."
		exit 1
	fi
	log "Downloading $url"
	curl -fsSL "$url" -o "$WORKDIR/lie.tar.gz"
	log "Installing linux-enable-ir-emitter to / (requires sudo)"
	sudo tar -C / --no-same-owner -m -xzf "$WORKDIR/lie.tar.gz"
}

# Asks a yes/no question, defaulting to yes; returns non-zero for no or no TTY.
prompt_yes_no() {
	local reply
	if [[ ! -t 0 ]]; then
		return 1
	fi
	read -r -p ">> $1 [Y/n] " reply
	[[ -z "$reply" || "$reply" =~ ^[Yy] ]]
}

# Prints the greyscale-only V4L2 node (the IR sensor), or returns non-zero.
find_ir_node() {
	local dev formats
	for dev in /dev/video*; do
		[[ -e "$dev" ]] || continue
		formats=$(v4l2-ctl -d "$dev" --list-formats 2>/dev/null) || continue
		if grep -qE "'GREY'|'Y8 '" <<<"$formats" &&
			! grep -qiE "MJPG|YUYV|YUV|RGB" <<<"$formats"; then
			echo "$dev"
			return 0
		fi
	done
	return 1
}

# True if a short burst from the IR node is substantially lit (emitter firing).
ir_emitter_active() {
	local node="$1" raw="$WORKDIR/probe.raw" total nonzero
	v4l2-ctl -d "$node" --set-fmt-video=pixelformat=GREY --stream-mmap \
		--stream-count=24 --stream-to="$raw" >/dev/null 2>&1 || return 1
	total=$(wc -c <"$raw")
	[[ "$total" -gt 0 ]] || return 1
	nonzero=$(tr -d '\000' <"$raw" | wc -c)
	# Emitter on => most pixels illuminated (~70-80% non-zero); off => a few percent.
	((nonzero * 100 / total >= 30))
}

activate_ir_emitter() {
	if [[ "$skip_emitter" -eq 1 ]]; then
		log "Skipping IR-emitter activation (--skip-emitter)."
		return
	fi
	if compgen -G "$IR_EMITTER_CONFIG_DIR/*" >/dev/null 2>&1; then
		log "IR emitter already configured ($IR_EMITTER_CONFIG_DIR); leaving it as is."
		log "Re-run 'sudo linux-enable-ir-emitter configure' to redo it."
		return
	fi
	local ir_node
	if ir_node=$(find_ir_node); then
		log "Checking the IR emitter on $ir_node ..."
		if ir_emitter_active "$ir_node"; then
			log "IR emitter is already producing lit frames; nothing to activate."
			return
		fi
	fi
	echo
	log "The IR illuminator is off until activated. This step is interactive and"
	log "captures from the camera while it sweeps vendor control payloads."
	if ! prompt_yes_no "Activate the IR emitter now? (needs sudo)"; then
		log "Skipped. Run 'sudo linux-enable-ir-emitter configure' when ready."
		return
	fi
	if sudo linux-enable-ir-emitter configure; then
		log "IR emitter configured."
		return
	fi
	# linux-enable-ir-emitter exits non-zero both when it saves nothing because the
	# emitter is "already working" (fine) and on a genuine failure, so don't claim
	# failure outright.
	echo
	log "linux-enable-ir-emitter saved no new configuration."
	log "  - If it reported the emitter is 'already working', it is on; you are set."
	log "  - If it reported a genuine failure, retry:"
	log "      sudo linux-enable-ir-emitter configure --manual"
}

# Downloads each pinned model into MODEL_DIR (root-owned), skipping any already
# present with a matching checksum. Verified before install, so a corrupt or
# substituted download is never kept.
download_models() {
	if [[ "$skip_models" -eq 1 ]]; then
		log "Skipping model download (--skip-models)."
		return
	fi
	log "Downloading recognition models into $MODEL_DIR (~260 MB on first run)"
	sudo install -d -m 0755 "$MODEL_DIR"
	local tmp="$WORKDIR/model.part"
	local entry name url sha dest
	for entry in "${MODELS[@]}"; do
		IFS='|' read -r name url sha <<<"$entry"
		dest="$MODEL_DIR/$name"
		if [[ -f "$dest" ]] && echo "$sha  $dest" | sha256sum -c --status 2>/dev/null; then
			log "  $name already present and verified"
			continue
		fi
		log "  fetching $name"
		curl -fsSL "$url" -o "$tmp"
		if ! echo "$sha  $tmp" | sha256sum -c --status; then
			err "Checksum mismatch for $name; refusing to install it."
			exit 1
		fi
		sudo install -D -m 0644 "$tmp" "$dest"
	done
}

print_next_steps() {
	cat <<'EOF'

Configuration complete. Next steps:
  make                         # build the workspace and the GUI
  make install                 # install CLI, GUI, PAM module, config, models (sudo)
  xinchao-gui                  # launch the GUI (or find it in your app menu) and enroll

Tip: re-run 'make install' whenever you rebuild, so the system CLI the GUI and
sudo use (in /usr/local/bin) stays up to date.

Wiring PAM (so sudo/login accept your face) is a careful manual step.
Read docs/INSTALL.md and keep a root shell open while editing /etc/pam.d.
EOF
}

main() {
	parse_args "$@"
	require_apt
	install_packages
	install_ir_emitter
	download_models
	activate_ir_emitter
	print_next_steps
}

main "$@"
