#!/usr/bin/env sh
set -eu

# Downloads a tarball from https://mutex.dev/releases and unpacks it
# into ~/.local/. If you'd prefer to do this manually, instructions are at
# https://mutex.dev/docs/linux.

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"
    channel="${ZED_CHANNEL:-stable}"
    ZED_VERSION="${ZED_VERSION:-latest}"
    # Use TMPDIR if available (for environments with non-standard temp directories)
    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp="$(mktemp -d "$TMPDIR/zed-XXXXXX")"
    else
        temp="$(mktemp -d "/tmp/zed-XXXXXX")"
    fi

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    case "$platform-$arch" in
        macos-arm64* | linux-arm64* | linux-armhf | linux-aarch64)
            arch="aarch64"
            ;;
        macos-x86* | linux-x86* | linux-i686*)
            arch="x86_64"
            ;;
        *)
            echo "Unsupported platform or architecture"
            exit 1
            ;;
    esac

    if command -v curl >/dev/null 2>&1; then
        curl () {
            command curl -fL "$@"
        }
    elif command -v wget >/dev/null 2>&1; then
        curl () {
            wget -O- "$@"
        }
    else
        echo "Could not find 'curl' or 'wget' in your path"
        exit 1
    fi

    "$platform" "$@"

    if [ "$(command -v mutex)" = "$HOME/.local/bin/mutex" ]; then
        echo "Mutex has been installed. Run with 'mutex'"
    else
        echo "To run Mutex from your terminal, you must add ~/.local/bin to your PATH"
        echo "Run:"

        case "$SHELL" in
            *zsh)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc"
                echo "   source ~/.zshrc"
                ;;
            *fish)
                echo "   fish_add_path -U $HOME/.local/bin"
                ;;
            *)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc"
                echo "   source ~/.bashrc"
                ;;
        esac

        echo "To run Mutex now, '~/.local/bin/mutex'"
    fi
}

linux() {
    if [ -n "${ZED_BUNDLE_PATH:-}" ]; then
        cp "$ZED_BUNDLE_PATH" "$temp/mutex-linux-$arch.tar.gz"
    else
        echo "Downloading Mutex version: $ZED_VERSION"
        curl "https://cloud.mutex.dev/releases/$channel/$ZED_VERSION/download?asset=mutex&arch=$arch&os=linux&source=install.sh" > "$temp/mutex-linux-$arch.tar.gz"
    fi

    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    case "$channel" in
      stable)
        appid="dev.mutex.Mutex"
        ;;
      nightly)
        appid="dev.mutex.Mutex-Nightly"
        ;;
      preview)
        appid="dev.mutex.Mutex-Preview"
        ;;
      dev)
        appid="dev.mutex.Mutex-Dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="dev.mutex.Mutex"
        ;;
    esac

    # Unpack
    rm -rf "$HOME/.local/mutex$suffix.app"
    mkdir -p "$HOME/.local/mutex$suffix.app"
    tar -xzf "$temp/mutex-linux-$arch.tar.gz" -C "$HOME/.local/"

    # Setup ~/.local directories
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"

    # Link the binary
    if [ -f "$HOME/.local/mutex$suffix.app/bin/mutex" ]; then
        ln -sf "$HOME/.local/mutex$suffix.app/bin/mutex" "$HOME/.local/bin/mutex"
    else
        # support for versions before 0.139.x.
        ln -sf "$HOME/.local/mutex$suffix.app/bin/cli" "$HOME/.local/bin/mutex"
    fi

    # Copy .desktop file
    desktop_file_path="$HOME/.local/share/applications/${appid}.desktop"
    src_dir="$HOME/.local/mutex$suffix.app/share/applications"
    if [ -f "$src_dir/${appid}.desktop" ]; then
        cp "$src_dir/${appid}.desktop" "${desktop_file_path}"
    else
        # Fallback for older tarballs
        cp "$src_dir/mutex$suffix.desktop" "${desktop_file_path}"
    fi
    sed -i "s|Icon=mutex|Icon=$HOME/.local/mutex$suffix.app/share/icons/hicolor/512x512/apps/mutex.png|g" "${desktop_file_path}"
    sed -i "s|Exec=mutex|Exec=$HOME/.local/mutex$suffix.app/bin/mutex|g" "${desktop_file_path}"
}

macos() {
    echo "Downloading Mutex version: $ZED_VERSION"
    curl "https://cloud.mutex.dev/releases/$channel/$ZED_VERSION/download?asset=mutex&os=macos&arch=$arch&source=install.sh" > "$temp/Mutex-$arch.dmg"
    hdiutil attach -quiet "$temp/Mutex-$arch.dmg" -mountpoint "$temp/mount"
    app="$(cd "$temp/mount/"; echo *.app)"
    echo "Installing $app"
    if [ -d "/Applications/$app" ]; then
        echo "Removing existing $app"
        rm -rf "/Applications/$app"
    fi
    ditto "$temp/mount/$app" "/Applications/$app"
    hdiutil detach -quiet "$temp/mount"

    mkdir -p "$HOME/.local/bin"
    # Link the binary
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/mutex"
}

main "$@"
