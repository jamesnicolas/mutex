#!/usr/bin/env sh
set -eu

# Uninstalls Mutex that was installed using the install.sh script

check_remaining_installations() {
    platform="$(uname -s)"
    if [ "$platform" = "Darwin" ]; then
        # Check for any Mutex variants in /Applications
        remaining=$(ls -d /Applications/Mutex*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    else
        # Check for any Mutex variants in ~/.local
        remaining=$(ls -d "$HOME/.local/mutex"*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    fi
}

prompt_remove_preferences() {
    printf "Do you want to keep your Mutex preferences? [Y/n] "
    read -r response
    case "$response" in
        [nN]|[nN][oO])
            rm -rf "$HOME/.config/mutex"
            echo "Preferences removed."
            ;;
        *)
            echo "Preferences kept."
            ;;
    esac
}

main() {
    platform="$(uname -s)"
    channel="${ZED_CHANNEL:-stable}"

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    "$platform"

    echo "Mutex has been uninstalled"
}

linux() {
    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    db_suffix="stable"
    case "$channel" in
      stable)
        appid="dev.mutex.Mutex"
        db_suffix="stable"
        ;;
      nightly)
        appid="dev.mutex.Mutex-Nightly"
        db_suffix="nightly"
        ;;
      preview)
        appid="dev.mutex.Mutex-Preview"
        db_suffix="preview"
        ;;
      dev)
        appid="dev.mutex.Mutex-Dev"
        db_suffix="dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="dev.mutex.Mutex"
        db_suffix="stable"
        ;;
    esac

    # Remove the app directory
    rm -rf "$HOME/.local/mutex$suffix.app"

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/mutex"

    # Remove the .desktop file
    rm -f "$HOME/.local/share/applications/${appid}.desktop"

    # Remove the database directory for this channel
    rm -rf "$HOME/.local/share/mutex/db/0-$db_suffix"

    # Remove socket file
    rm -f "$HOME/.local/share/mutex/mutex-$db_suffix.sock"

    # Remove the entire Mutex directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/.local/share/mutex"
        prompt_remove_preferences
    fi

    rm -rf "$HOME/.mutex_server"
}

macos() {
    app="Mutex.app"
    db_suffix="stable"
    app_id="dev.mutex.Mutex"
    case "$channel" in
      nightly)
        app="Mutex Nightly.app"
        db_suffix="nightly"
        app_id="dev.mutex.Mutex-Nightly"
        ;;
      preview)
        app="Mutex Preview.app"
        db_suffix="preview"
        app_id="dev.mutex.Mutex-Preview"
        ;;
      dev)
        app="Mutex Dev.app"
        db_suffix="dev"
        app_id="dev.mutex.Mutex-Dev"
        ;;
    esac

    # Remove the app bundle
    if [ -d "/Applications/$app" ]; then
        rm -rf "/Applications/$app"
    fi

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/mutex"

    # Remove the database directory for this channel
    rm -rf "$HOME/Library/Application Support/Mutex/db/0-$db_suffix"

    # Remove app-specific files and directories
    rm -rf "$HOME/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/$app_id.sfl"*
    rm -rf "$HOME/Library/Caches/$app_id"
    rm -rf "$HOME/Library/HTTPStorages/$app_id"
    rm -rf "$HOME/Library/Preferences/$app_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$app_id.savedState"

    # Remove the entire Mutex directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/Library/Application Support/Mutex"
        rm -rf "$HOME/Library/Logs/Mutex"

        prompt_remove_preferences
    fi

    rm -rf "$HOME/.mutex_server"
}

main "$@"
