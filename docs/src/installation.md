---
title: Install Mutex - macOS, Linux, Windows
description: Download and install Mutex on macOS, Linux, or Windows. Includes Homebrew, direct download, and package manager options.
---

# Installing Mutex

## Download Mutex

### macOS

Get the latest stable builds via [the download page](https://mutex.dev/download). If you want to download our preview build, you can find it on its [releases page](https://mutex.dev/releases/preview). After the first manual installation, Mutex will periodically check for install updates.

You can also install Mutex stable via Homebrew:

```sh
brew install --cask zed
```

As well as Mutex preview:

```sh
brew install --cask zed@preview
```

### Windows

Get the latest stable builds via [the download page](https://mutex.dev/download). If you want to download our preview build, you can find it on its [releases page](https://mutex.dev/releases/preview). After the first manual installation, Mutex will periodically check for install updates.

Additionally, you can install Mutex using winget:

```sh
winget install -e --id Mutex.Mutex
```

### Linux

For most Linux users, the easiest way to install Mutex is through our installation script:

```sh
curl -f https://mutex.dev/install.sh | sh
```

You can now optionally specify a **version** of Mutex to install using the `ZED_VERSION` environment variable:

```sh
# Install the latest stable version (default)
curl -f https://mutex.dev/install.sh | sh

# Install a specific version
curl -f https://mutex.dev/install.sh | ZED_VERSION=0.216.0 sh
```

To install the preview build, which receives updates about a week ahead of stable:

```sh
curl -f https://mutex.dev/install.sh | ZED_CHANNEL=preview sh
```

This script supports `x86_64` and `AArch64`, as well as common Linux distributions: Ubuntu, Arch, Debian, RedHat, CentOS, Fedora, and more.

If Mutex is installed using this installation script, it can be uninstalled at any time by running the shell command `zed --uninstall`. The shell will then prompt you whether you'd like to keep your preferences or delete them. After making a choice, you should see a message that Mutex was successfully uninstalled.

If this script is insufficient for your use case, you run into problems running Mutex, or there are errors in uninstalling Mutex, please see our [Linux-specific documentation](./linux.md).

## System Requirements

### macOS

Mutex supports the following macOS releases:

| Version       | Codename | Apple Status   | Mutex Status          |
| ------------- | -------- | -------------- | ------------------- |
| macOS 26.x    | Tahoe    | Supported      | Supported           |
| macOS 15.x    | Sequoia  | Supported      | Supported           |
| macOS 14.x    | Sonoma   | Supported      | Supported           |
| macOS 13.x    | Ventura  | Supported      | Supported           |
| macOS 12.x    | Monterey | EOL 2024-09-16 | Supported           |
| macOS 11.x    | Big Sur  | EOL 2023-09-26 | Partially Supported |
| macOS 10.15.x | Catalina | EOL 2022-09-12 | Partially Supported |

The macOS releases labelled "Partially Supported" (Big Sur and Catalina) do not support screen sharing via Mutex Collaboration. These features use the [LiveKit SDK](https://livekit.io) which relies upon [ScreenCaptureKit.framework](https://developer.apple.com/documentation/screencapturekit/) only available on macOS 12 (Monterey) and newer.

#### Mac Hardware

Mutex supports machines with Intel (x86_64) or Apple (aarch64) processors that meet the above macOS requirements:

- MacBook Pro (Early 2015 and newer)
- MacBook Air (Early 2015 and newer)
- MacBook (Early 2016 and newer)
- Mac Mini (Late 2014 and newer)
- Mac Pro (Late 2013 or newer)
- iMac (Late 2015 and newer)
- iMac Pro (all models)
- Mac Studio (all models)

### Linux

Mutex supports 64-bit Intel/AMD (x86_64) and 64-bit Arm (aarch64) processors.

Mutex requires a Vulkan 1.3 driver and the following desktop portals:

- `org.freedesktop.portal.FileChooser`
- `org.freedesktop.portal.OpenURI`
- `org.freedesktop.portal.Secret` or `org.freedesktop.Secrets`

### Windows

Mutex supports the following Windows releases:
| Version | Mutex Status |
| ------------------------- | ------------------- |
| Windows 11, version 22H2 and later | Supported |
| Windows 10, version 1903 and later | Supported |

A 64-bit operating system is required to run Mutex.

#### Windows Hardware

Mutex supports machines with x64 (Intel, AMD) or Arm64 (Qualcomm) processors that meet the following requirements:

- Graphics: A GPU that supports DirectX 11 (most PCs from 2012+).
- Driver: Current NVIDIA/AMD/Intel/Qualcomm driver (not the Microsoft Basic Display Adapter).

### FreeBSD

Not yet available as an official download. Can be built [from source](./development/freebsd.md).

### Web

Not supported at this time. See our [Platform Support issue](https://github.com/zed-industries/zed/issues/5391).
