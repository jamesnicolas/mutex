---
title: Installing Extensions
description: "Browse, install, and manage extensions from the Mutex Extension Gallery."
---

# Installing Extensions {#installing-extensions}

Extensions add functionality to Mutex, including languages, themes, and AI tools. Browse and install them from the Extension Gallery.

Open the Extension Gallery with {#kb zed::Extensions}, or select "Mutex > Extensions" from the menu bar.

## Installation Location

- On macOS, extensions are installed in `~/Library/Application Support/Mutex/extensions`.
- On Linux, they are installed in either `$XDG_DATA_HOME/zed/extensions` or `~/.local/share/zed/extensions`.
- On Windows, the directory is `%LOCALAPPDATA%\Mutex\extensions`.

This directory contains two subdirectories:

- `installed`, which contains the source code for each extension.
- `work` which contains files created by the extension itself, such as downloaded language servers.

## Auto-installing

To automate extension installation/uninstallation see the docs for [auto_install_extensions](../reference/all-settings.md#auto-install-extensions).
