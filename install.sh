#!/bin/sh
# Polyphony installer script
# https://github.com/penso/polyphony
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/penso/polyphony/main/install.sh | sh
#
# Or with options:
#   curl -fsSL ... | sh -s -- --no-homebrew
#   curl -fsSL ... | sh -s -- --method=binary
#   curl -fsSL ... | sh -s -- --version=20260315.01

set -e

GITHUB_REPO="penso/polyphony"
HOMEBREW_TAP="penso/polyphony"
BINARY_NAME="polyphony"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# Default options
USE_HOMEBREW=true
PREFERRED_METHOD=""
VERSION=""

# Colors (disabled if not a terminal)
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BLUE='\033[0;34m'
    BOLD='\033[1m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    BOLD=''
    NC=''
fi

info() {
    printf "${BLUE}==>${NC} ${BOLD}%s${NC}\n" "$1"
}

success() {
    printf "${GREEN}==>${NC} ${BOLD}%s${NC}\n" "$1"
}

warn() {
    printf "${YELLOW}Warning:${NC} %s\n" "$1" >&2
}

error() {
    printf "${RED}Error:${NC} %s\n" "$1" >&2
    exit 1
}

# Parse arguments
while [ $# -gt 0 ]; do
    case "$1" in
        --no-homebrew)
            USE_HOMEBREW=false
            ;;
        --method=*)
            PREFERRED_METHOD="${1#*=}"
            ;;
        --version=*)
            VERSION="${1#*=}"
            ;;
        -h|--help)
            cat <<EOF
Polyphony installer

Usage:
    install.sh [OPTIONS]

Options:
    --no-homebrew       Skip Homebrew even if available (macOS)
    --method=METHOD     Force installation method: homebrew, binary, source
    --version=VERSION   Install a specific version (default: latest)
    -h, --help          Show this help message

Environment variables:
    INSTALL_DIR         Binary installation directory (default: ~/.local/bin)

Examples:
    curl -fsSL https://raw.githubusercontent.com/penso/polyphony/main/install.sh | sh
    curl -fsSL ... | sh -s -- --method=binary
    curl -fsSL ... | sh -s -- --version=20260315.01
EOF
            exit 0
            ;;
        *)
            warn "Unknown option: $1"
            ;;
    esac
    shift
done

detect_os() {
    OS="$(uname -s)"
    case "$OS" in
        Darwin)
            echo "macos"
            ;;
        Linux)
            echo "linux"
            ;;
        MINGW*|MSYS*|CYGWIN*)
            echo "windows"
            ;;
        *)
            echo "unknown"
            ;;
    esac
}

detect_arch() {
    ARCH="$(uname -m)"
    case "$ARCH" in
        x86_64|amd64)
            echo "x86_64"
            ;;
        aarch64|arm64)
            echo "aarch64"
            ;;
        *)
            echo "$ARCH"
            ;;
    esac
}

command_exists() {
    command -v "$1" >/dev/null 2>&1
}

get_latest_version() {
    # Polyphony uses bare date tags (YYYYMMDD.NN), no "v" prefix
    if command_exists curl; then
        curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" \
            | grep '"tag_name":' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/'
    elif command_exists wget; then
        wget -qO- "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" \
            | grep '"tag_name":' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/'
    else
        error "Neither curl nor wget found. Please install one of them."
    fi
}

download() {
    url="$1"
    dest="$2"
    if command_exists curl; then
        curl -fsSL "$url" -o "$dest"
    elif command_exists wget; then
        wget -q "$url" -O "$dest"
    else
        error "Neither curl nor wget found. Please install one of them."
    fi
}

verify_checksum() {
    file="$1"
    checksums_file="$2"
    basename="$3"

    if [ ! -f "$checksums_file" ]; then
        warn "Checksum file not available, skipping verification"
        return 0
    fi

    expected_sha256=$(grep "$basename" "$checksums_file" | cut -d' ' -f1)
    if [ -z "$expected_sha256" ]; then
        warn "No checksum found for $basename, skipping verification"
        return 0
    fi

    if command_exists sha256sum; then
        actual=$(sha256sum "$file" | cut -d' ' -f1)
    elif command_exists shasum; then
        actual=$(shasum -a 256 "$file" | cut -d' ' -f1)
    else
        warn "Cannot verify checksum (sha256sum/shasum not found)"
        return 0
    fi

    if [ "$actual" != "$expected_sha256" ]; then
        error "Checksum verification failed!\nExpected: $expected_sha256\nActual:   $actual"
    fi
    info "Checksum verified"
}

ensure_install_dir() {
    if [ ! -d "$INSTALL_DIR" ]; then
        mkdir -p "$INSTALL_DIR"
    fi
}

add_to_path_instructions() {
    shell_name=$(basename "$SHELL")
    case "$shell_name" in
        bash)  rc_file="$HOME/.bashrc" ;;
        zsh)   rc_file="$HOME/.zshrc" ;;
        fish)  rc_file="$HOME/.config/fish/config.fish" ;;
        *)     rc_file="$HOME/.profile" ;;
    esac

    # Check if already in PATH
    case ":$PATH:" in
        *":$INSTALL_DIR:"*)
            return
            ;;
    esac

    printf "\n"
    warn "$INSTALL_DIR is not in your PATH."
    printf "Add it by running:\n\n"
    if [ "$shell_name" = "fish" ]; then
        printf "  ${BOLD}fish_add_path %s${NC}\n\n" "$INSTALL_DIR"
    else
        printf "  ${BOLD}echo 'export PATH=\"%s:\$PATH\"' >> %s${NC}\n\n" "$INSTALL_DIR" "$rc_file"
    fi
    printf "Then restart your shell or run:\n"
    printf "  ${BOLD}source %s${NC}\n" "$rc_file"
}

# --- Installation methods ---

install_homebrew() {
    info "Installing via Homebrew..."
    if ! command_exists brew; then
        error "Homebrew not found. Install it from https://brew.sh/"
    fi
    brew install "$HOMEBREW_TAP/$BINARY_NAME"
    success "Polyphony installed via Homebrew"
}

install_binary() {
    os="$1"
    arch="$2"
    version="$3"

    # Determine target triple and archive format
    case "$os" in
        macos)
            # macOS ships a universal binary (arm64 + x86_64)
            target="universal2-apple-darwin"
            ext="tar.gz"
            ;;
        linux)
            target="${arch}-unknown-linux-gnu"
            ext="tar.gz"
            ;;
        windows)
            target="x86_64-pc-windows-msvc"
            ext="zip"
            ;;
        *)
            error "Unsupported OS for binary installation: $os"
            ;;
    esac

    archive="${BINARY_NAME}-${version}-${target}.${ext}"
    url="https://github.com/${GITHUB_REPO}/releases/download/${version}/${archive}"
    checksums_url="https://github.com/${GITHUB_REPO}/releases/download/${version}/SHA256SUMS.txt"

    info "Downloading ${BINARY_NAME} ${version} for ${target}..."

    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT

    download "$url" "$tmpdir/$archive" \
        || error "Failed to download $archive.\nCheck https://github.com/${GITHUB_REPO}/releases for available platforms."

    # Verify checksum from SHA256SUMS.txt
    if download "$checksums_url" "$tmpdir/SHA256SUMS.txt" 2>/dev/null; then
        verify_checksum "$tmpdir/$archive" "$tmpdir/SHA256SUMS.txt" "$archive"
    else
        warn "Could not download checksums, skipping verification"
    fi

    # Extract
    case "$ext" in
        tar.gz)
            tar -xzf "$tmpdir/$archive" -C "$tmpdir"
            ;;
        zip)
            unzip -q "$tmpdir/$archive" -d "$tmpdir"
            ;;
    esac

    ensure_install_dir

    if [ "$os" = "windows" ]; then
        mv "$tmpdir/${BINARY_NAME}.exe" "$INSTALL_DIR/${BINARY_NAME}.exe"
        success "Polyphony installed to $INSTALL_DIR/${BINARY_NAME}.exe"
    else
        mv "$tmpdir/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
        chmod +x "$INSTALL_DIR/$BINARY_NAME"
        success "Polyphony installed to $INSTALL_DIR/$BINARY_NAME"
    fi

    add_to_path_instructions
}

install_from_source() {
    version="$1"
    warn "Building from source. This requires Rust nightly and may take several minutes..."

    if ! command_exists cargo; then
        info "Rust not found. Installing via rustup..."
        if command_exists curl; then
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        else
            wget -qO- https://sh.rustup.rs | sh -s -- -y
        fi
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env"
    fi

    # Polyphony requires nightly
    if ! rustup run nightly rustc --version >/dev/null 2>&1; then
        info "Installing Rust nightly toolchain..."
        rustup install nightly
    fi

    if ! command_exists git; then
        error "Git is required to build from source. Please install it first."
    fi

    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT

    info "Cloning repository..."
    git clone --depth 1 --branch "$version" "https://github.com/${GITHUB_REPO}.git" "$tmpdir/polyphony"
    cd "$tmpdir/polyphony"

    info "Building release binary..."
    cargo +nightly build --locked --release -p polyphony-cli

    ensure_install_dir
    cp "target/release/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
    chmod +x "$INSTALL_DIR/$BINARY_NAME"

    success "Polyphony built and installed to $INSTALL_DIR/$BINARY_NAME"
    add_to_path_instructions
}

# --- Main ---

main() {
    printf "\n"
    printf "  ${BOLD}Polyphony Installer${NC}\n"
    printf "  Orchestrate AI agents from your terminal\n"
    printf "\n"

    OS=$(detect_os)
    ARCH=$(detect_arch)

    info "Detected: $OS ($ARCH)"

    if [ "$OS" = "windows" ]; then
        error "Windows is not supported by this installer.\nDownload the .zip from: https://github.com/${GITHUB_REPO}/releases"
    fi

    if [ "$OS" = "unknown" ]; then
        error "Unsupported operating system: $(uname -s)"
    fi

    # Get version
    if [ -z "$VERSION" ]; then
        info "Fetching latest version..."
        VERSION=$(get_latest_version)
        if [ -z "$VERSION" ]; then
            error "Failed to determine latest version. Specify one with --version=YYYYMMDD.NN"
        fi
    fi
    info "Version: $VERSION"

    # Determine installation method
    if [ -n "$PREFERRED_METHOD" ]; then
        case "$PREFERRED_METHOD" in
            homebrew) install_homebrew ;;
            binary)   install_binary "$OS" "$ARCH" "$VERSION" ;;
            source)   install_from_source "$VERSION" ;;
            *)        error "Unknown method: $PREFERRED_METHOD (available: homebrew, binary, source)" ;;
        esac
    elif [ "$OS" = "macos" ]; then
        if [ "$USE_HOMEBREW" = true ] && command_exists brew; then
            install_homebrew
        else
            install_binary "$OS" "$ARCH" "$VERSION"
        fi
    elif [ "$OS" = "linux" ]; then
        if [ "$ARCH" = "x86_64" ] || [ "$ARCH" = "aarch64" ]; then
            install_binary "$OS" "$ARCH" "$VERSION"
        else
            warn "No pre-built binary for $ARCH. Building from source..."
            install_from_source "$VERSION"
        fi
    fi

    # Verify installation
    if command_exists "$BINARY_NAME"; then
        installed_version=$("$BINARY_NAME" --version 2>/dev/null | head -1 || echo "unknown")
        printf "\n"
        success "Installation complete!"
        printf "  ${BOLD}%s${NC}\n" "$installed_version"
        printf "\n"
        printf "Get started:\n"
        printf "  ${BOLD}polyphony${NC}          # Launch the TUI dashboard\n"
        printf "  ${BOLD}polyphony --help${NC}   # Show help\n"
        printf "\n"
        printf "Documentation: ${BLUE}https://github.com/${GITHUB_REPO}${NC}\n"
    elif [ -x "$INSTALL_DIR/$BINARY_NAME" ]; then
        printf "\n"
        success "Installation complete!"
        printf "\n"
        add_to_path_instructions
    fi
}

main
