---
title: Code Completions - Mutex
description: Mutex's code completions from language servers and edit predictions. Configure autocomplete behavior, snippets, and documentation display.
---

# Completions

Mutex supports two sources for completions:

1. "Code Completions" provided by Language Servers (LSPs) automatically installed by Mutex or via [Mutex Language Extensions](languages.md).
2. "Edit Predictions" provided by Mutex's own Zeta model or by external providers like [GitHub Copilot](#github-copilot).

## Language Server Code Completions {#code-completions}

When there is an appropriate language server available, Mutex will provide completions of variable names, functions, and other symbols in the current file. You can disable these by adding the following to your Mutex `settings.json` file:

```json [settings]
"show_completions_on_input": false
```

You can manually trigger completions with `ctrl-space` or by triggering the `editor::ShowCompletions` action from the command palette.

> Note: Using `ctrl-space` in Mutex requires disabling the macOS global shortcut.
> Open **System Settings** > **Keyboard** > **Keyboard Shortcut**s >
> **Input Sources** and uncheck **Select the previous input source**.

For more information, see:

- [Configuring Supported Languages](./configuring-languages.md)
- [List of Mutex Supported Languages](./languages.md)

## Edit Predictions {#edit-predictions}

Mutex has built-in support for predicting multiple edits at a time [via Zeta](https://huggingface.co/zed-industries/zeta), Mutex's open-source and open-data model.
Edit predictions appear as you type, and most of the time, you can accept them by pressing `tab`.

See the [edit predictions documentation](./ai/edit-prediction.md) for more information on how to setup and configure Mutex's edit predictions.
