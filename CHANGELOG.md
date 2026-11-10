# Changelog

## [Unreleased]

### Fixed

- An ids file takes the comments its help promised: `#` starts a comment that runs to the end of the line, on a line of its own or after an id. A comment with words in it used to fail on its first word. An id that does not parse is reported with its line number.
- A download no longer writes over a file that is already there. The same bytes are left alone and reported as `already there`; different bytes, such as another version of the same asset under the same name, refuse that asset with the file's path, and the run exits non-zero once the other ids are done. `--force` overwrites.
- A large asset on a slow link is no longer cut off after a minute. The one-minute cap covered the whole transfer, body included; it is now a 30 second connect timeout and a 60 second read timeout, which fire on a server that does not answer or stops sending, not on a download that is still moving.

### Security

- The API key no longer follows a redirect to another host. Redirects were followed with every header but `Cookie` and `Authorization`, so a redirect from the Open Cloud host sent `x-api-key` on to wherever it pointed. Redirects are now followed one hop at a time, and the key and the cookie go along only while every hop stays on the host they were given for.
- Release archives carry a build provenance attestation: `gh attestation verify <zip> --repo rbx-forge/rbx-download`. The build jobs run with read access only, and the publishing job alone may write.

## [0.1.0] - 2026-11-08

First release as a standalone tool.

- Downloads assets by id through the public `assetdelivery` endpoint, or through Open Cloud whenever an API key is available.
- `-a` is short for `--api-key`.
- `--asset-version <n>` pins a version on the Open Cloud backend. `--version` prints the tool's own version.
- The Studio cookie is used only after a yes: asked once on an interactive run, refused on a non-interactive one, `--auto-cookie` and `--no-auto-cookie` as the standing answers.
