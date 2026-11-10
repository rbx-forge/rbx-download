# Changelog

## [Unreleased]

### Fixed

- An ids file takes the comments its help describes: `#` starts a comment that runs to the end of the line, on a line of its own or after an id, words and all. An id that does not parse is reported with its line number.
- A download does not write over a file that is already there. The same bytes are left alone and reported as `already there`. Different bytes, such as another version of the same asset under the same name, refuse that asset with the file's path, and the run exits non-zero once the other ids are done. `--force` overwrites.
- A large asset on a slow link is not cut off while its bytes keep arriving: a connection gets 30 seconds to open and the server 60 seconds of silence, with no cap on the transfer as a whole.
- A rate limit, a server error, a timeout or a failed connection does not fail an asset on the first try. The request is sent again up to three times with backoff, or after as long as a rate limit asks for. A rate limit that asks for more than two minutes fails the asset with how long it asked for.
- A failed asset's line shows the whole error: the endpoint's sentence, then the status and body Roblox answered with.

### Security

- The API key and the cookie go only to the host they were given for. Redirects are followed one hop at a time, and the credential goes along only while every hop stays on that host, so a redirect to a CDN or anywhere else is followed without it.
- An error does not print the URL it was requesting. A CDN download location is a signed link, usable by anyone who reads it in a terminal or a CI log.
- Release archives carry a build provenance attestation: `gh attestation verify <zip> --repo rbx-forge/rbx-download`. The build jobs run with read access only, and the publishing job alone may write.

## [0.1.0] - 2026-11-08

First release as a standalone tool.

- Downloads assets by id through the public `assetdelivery` endpoint, or through Open Cloud whenever an API key is available.
- `-a` is short for `--api-key`.
- `--asset-version <n>` pins a version on the Open Cloud backend. `--version` prints the tool's own version.
- The Studio cookie is used only after a yes: asked once on an interactive run, refused on a non-interactive one, `--auto-cookie` and `--no-auto-cookie` as the standing answers.
