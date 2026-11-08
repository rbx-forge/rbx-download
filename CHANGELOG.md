# Changelog

## [Unreleased]

First release as a standalone tool.

- Downloads assets by id through the public `assetdelivery` endpoint, or through Open Cloud whenever an API key is available.
- `--asset-version <n>` pins a version on the Open Cloud backend. `--version` prints the tool's own version.
- The Studio cookie is used only after a yes: asked once on an interactive run, refused on a non-interactive one, `--auto-cookie` and `--no-auto-cookie` as the standing answers.
