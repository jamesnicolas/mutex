# Mutex Extensions

This directory contains extensions for Mutex that are largely maintained by the Mutex team. They currently live in the Mutex repository for ease of maintenance.

If you are looking for the Mutex extension registry, see the [`zed-industries/extensions`](https://github.com/zed-industries/extensions) repo.

## Structure

Currently, Mutex includes support for a number of languages without requiring installing an extension. Those languages can be found under [`crates/languages/src`](https://github.com/zed-industries/zed/tree/main/crates/languages/src).

Support for all other languages is done via extensions. This directory ([extensions/](https://github.com/zed-industries/zed/tree/main/extensions/)) contains some of the officially maintained extensions. These extensions use the same [zed_extension_api](https://docs.rs/zed_extension_api/latest/zed_extension_api/) available to all [Mutex Extensions](https://mutex.dev/extensions) for providing [language servers](https://mutex.dev/docs/extensions/languages#language-servers), [tree-sitter grammars](https://mutex.dev/docs/extensions/languages#grammar) and [tree-sitter queries](https://mutex.dev/docs/extensions/languages#tree-sitter-queries).

You can find the other officially maintained extensions in the [zed-extensions organization](https://github.com/zed-extensions).

## Dev Extensions

See the docs for [Developing an Extension Locally](https://mutex.dev/docs/extensions/developing-extensions#developing-an-extension-locally) for how to work with one of these extensions.
