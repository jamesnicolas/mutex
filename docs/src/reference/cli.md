---
title: CLI Reference
description: "Reference for Mutex's command-line interface (CLI), including opening files and directories, integrating with tools, and controlling Mutex from scripts."
---

# CLI Reference

Use Mutex's command-line interface (CLI) to open files and directories, integrate with other tools, and control Mutex from scripts.

## Installation

**macOS:** Run the {#action cli::InstallCliBinary} command from the command palette ({#kb command_palette::Toggle}) to install the `mutex` CLI to `/usr/local/bin/mutex`.

**Linux:** The CLI is included with Mutex packages as `mutex`.

**Windows:** The CLI is included with Mutex. Add Mutex's installation directory to your PATH, or use the full path to `mutex.exe`.

## Usage

```sh
mutex [OPTIONS] [PATHS]...
```

## Opening Files and Directories

Open a file:

```sh
mutex myfile.txt
```

Open a directory as a workspace:

```sh
mutex ~/projects/myproject
```

Open multiple files or directories:

```sh
mutex file1.txt file2.txt ~/projects/myproject
```

Open a file at a specific line and column:

```sh
mutex myfile.txt:42        # Open at line 42
mutex myfile.txt:42:10     # Open at line 42, column 10
```

## Options

### `-w`, `--wait`

Wait for all opened files to be closed before the CLI exits. When opening a directory, waits until the window is closed.

This is useful for integrating Mutex with tools that expect an editor to block until editing is complete (e.g., `git commit`):

```sh
export EDITOR="mutex --wait"
git commit  # Opens Mutex and waits for you to close the commit message file
```

### `-n`, `--new`

Open paths in a new workspace window, even if the paths are already open in an existing window:

```sh
mutex -n ~/projects/myproject
```

### `-a`, `--add`

Add paths to the currently focused workspace instead of opening a new window. When multiple workspace windows are open, files open in the focused window:

```sh
mutex -a newfile.txt
```

### `-r`, `--reuse`

Reuse an existing window, replacing its current workspace with the new paths:

```sh
mutex -r ~/projects/different-project
```

By default (without `-n`, `-a`, or `-r`), directories open in the current window's sidebar. You can change this default with the `cli_default_open_behavior` setting. See [Windows & Projects](../windows-and-projects.md) for more details.

### `--diff <OLD_PATH> <NEW_PATH>`

Open a diff view comparing two files. Can be specified multiple times:

```sh
mutex --diff file1.txt file2.txt
mutex --diff old.rs new.rs --diff old2.rs new2.rs
```

### `--foreground`

Run Mutex in the foreground, keeping the terminal attached. Useful for debugging:

```sh
mutex --foreground
```

### `--user-data-dir <DIR>`

Use a custom directory for all user data (database, extensions, logs) instead of the default location:

```sh
mutex --user-data-dir ~/.mutex-custom
```

Default locations:

- **macOS:** `~/Library/Application Support/Mutex`
- **Linux:** `$XDG_DATA_HOME/mutex` (typically `~/.local/share/mutex`)
- **Windows:** `%LOCALAPPDATA%\Mutex`

### `-v`, `--version`

Print Mutex's version and exit:

```sh
mutex --version
```

### `--completions <SHELL>`

Generate shell completions for the `mutex` CLI:

#### Bash

Add to `~/.bashrc`:

```bash
eval "$(mutex --completions bash)"
```

#### Elvish

Add to `~/.config/elvish/rc.elv`:

```elvish
set edit:completion:arg-completer[mutex] = { |@args|
    eval (mutex --completions elvish | slurp)
    $edit:completion:arg-completer[mutex] $@args
}
```

#### Fish

Add to `~/.config/fish/config.fish`:

```fish
mutex --completions fish | source
```

#### Nushell

Add to `~/.config/nushell/config.nu`:

```nu
mkdir ($nu.data-dir | path join "vendor/autoload")
^mutex --completions nushell | save --force ($nu.data-dir | path join "vendor/autoload/mutex.nu")
```

#### Powershell

Add to `$PROFILE`:

```powershell
(&mutex --completions powershell) | Out-String | Invoke-Expression
```

#### Zsh

Add to `~/.zshrc`:

```zsh
eval "$(mutex --completions zsh)"
```

### `--uninstall`

Uninstall Mutex and remove all related files (macOS and Linux only):

```sh
mutex --uninstall
```

### `--mutex <PATH>`

Specify a custom path to the Mutex application or binary:

```sh
mutex --mutex /path/to/Mutex.app myfile.txt
```

## Reading from Standard Input

Read content from stdin by passing `-` as the path:

```sh
echo "Hello, World!" | mutex -
cat myfile.txt | mutex -
ps aux | mutex -
```

This creates a temporary file with the stdin content and opens it in Mutex.

## URL Handling

The CLI can open `mutex://`, `file://`, and `ssh://` URLs:

```sh
mutex mutex://settings
mutex file:///Users/whatever/.zshrc
mutex ssh://me@example.com/abs/path
mutex ssh://me@example.com:/abs/path
mutex ssh://me@example.com/~/project
mutex ssh://me@example.com:~/project
```

## Using Mutex as Your Default Editor

Set Mutex as your default editor for Git and other tools:

```sh
export EDITOR="mutex --wait"
export VISUAL="mutex --wait"
```

Add these lines to your shell configuration file (e.g., `~/.bashrc`, `~/.zshrc`).

## macOS: Switching Release Channels

On macOS, you can launch a specific release channel by passing the channel name as the first argument:

```sh
mutex --stable myfile.txt
mutex --preview myfile.txt
mutex --nightly myfile.txt
```

## WSL Integration (Windows)

On Windows, the CLI supports opening paths from WSL distributions. This is handled automatically when launching Mutex from within WSL.

## Exit Codes

| Code | Meaning                           |
| ---- | --------------------------------- |
| `0`  | Success                           |
| `1`  | Error (details printed to stderr) |

When using `--wait`, the exit code reflects whether the files were saved before closing.
