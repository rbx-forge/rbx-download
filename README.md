# rbx-download

Download Roblox assets by id from the command line: images, audio, meshes, models, animations, places, and more.

## Install

```bash
rokit add rbx-dev-tools/rbx-download
```

Or take a binary for your platform from the [releases page](https://github.com/rbx-dev-tools/rbx-download/releases), which ships a `SHA256SUMS` alongside the archives, or build from source:

```bash
cargo install --git https://github.com/rbx-dev-tools/rbx-download --locked
```

## Quick start

```bash
# Download one asset into ./downloads
rbx-download 123456789

# Several at once, into a chosen folder
rbx-download 123 456 789 -o assets

# From a file (whitespace/comma separated, # comments allowed)
rbx-download --file ids.txt

# A specific version, through Open Cloud
rbx-download 123456789 --asset-version 3 --api-key "$RBX_API_KEY"
```

## Options

- `-f, --file <path>`: read additional asset ids from a file
- `-o, --output <dir>`: output directory (default: `downloads`)
- `--public`: force the public assetdelivery backend even when an API key is set
- `--type <id|alias>`: skip the economy metadata lookup by giving the asset type yourself. Aliases: `image`, `audio`, `mesh`, `lua`, `place`, `model`, `animation`, `video`, `font`
- `--asset-version <n>`: pin a specific asset version (Open Cloud only; requires an API key and exactly one id)
- `--raw`: don't dereference Animation wrappers to their KeyframeSequence
- `--api-key <key>`, `-a`: Open Cloud API key, or `RBX_API_KEY`
- `--cookie <value>`: `.ROBLOSECURITY` cookie for the public backend, or `RBX_COOKIE`
- `--auto-cookie` / `--no-auto-cookie`: use a signed-in Studio's session without asking, or never look for one

## Backend selection

| Situation | Backend |
| --- | --- |
| No API key | Public `assetdelivery.roblox.com`, with an optional cookie |
| `--api-key` / `RBX_API_KEY` set | Open Cloud `asset-delivery-api` |
| `--asset-version <n>` given | Open Cloud (requires an API key) |
| `--public` given | Public, always |

The public backend fetches most publicly available assets without auth. A cookie extends its reach to assets your account can access. The Open Cloud backend uses your API key's permissions and is the only one supporting `--asset-version`.

## The cookie

A `.ROBLOSECURITY` cookie is a full-account credential: more powerful than any API key, and not revocable per tool. So it is only ever used on the public backend, and only in this order:

1. `--cookie` or `RBX_COOKIE`, a value you chose.
2. A signed-in Roblox Studio on this machine, **if you say yes**. An interactive run asks once; a run with nowhere to ask (CI, a pipe, a cron job) refuses and says so. `--auto-cookie` is the standing yes, `--no-auto-cookie` the standing no.

When the Studio session is used, a line on stderr says so. The cookie is sent to two hosts, `assetdelivery` for the bytes and `economy` for the asset type behind the filename, and never written to disk.

## Filenames

Files are saved as `<id>_<sanitized name>.<ext>` (or `<id>.<ext>` when the name is unavailable). The extension comes from the asset's `AssetTypeId` via the economy details endpoint, unless `--type` was given, in which case no metadata lookup happens at all. A type nobody has measured is saved as `.bin` rather than under a plausible guess.

## License

[MPL-2.0](LICENSE)
