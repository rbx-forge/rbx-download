# Security policy

`rbx-download` handles credentials: an Open Cloud API key for that backend,
and, on the public backend, a Roblox session cookie, a whole account session.
It also writes files whose names come from Roblox. That makes the threat model
worth stating.

## Reporting a vulnerability

Report it privately, through GitHub: on this repository, open the **Security**
tab and choose **Report a vulnerability**, or go straight to
<https://github.com/rbx-forge/rbx-download/security/advisories/new>.

Do not open a public issue for a vulnerability. Do not paste a real API key or
cookie into a report: a redacted reproduction is always enough.

What helps: the `rbx-download --version`, the command you ran, what you
expected, what happened, and why it is a security problem rather than a bug. A
proof of concept is welcome but never required.

**What to expect.** This is a solo-maintained project. An acknowledgement
within a week is the realistic commitment; a fix lands as fast as the severity
warrants. You will be credited in the advisory and the changelog unless you
ask otherwise. There is no bounty program.

## Supported versions

Only the latest release. At 0.x, fixes go out as a new release rather than
being backported.

## What counts

In scope:

- A credential (API key, cookie) written to disk, logged, printed, or sent to
  any host other than the ones the README names for it.
- The Studio cookie read without asking, or on a run that has nobody to ask.
- A file written outside the output directory, or an existing file replaced
  without `--force`, from a crafted asset name, type or id.
- Dependency vulnerabilities with a plausible path to any of the above.

Out of scope:

- That a downloaded asset is what its owner uploaded. This tool fetches bytes;
  what they contain is not its claim.
- An API key or cookie reaching what it is allowed to reach.
- Anything requiring an attacker who already has your credentials, or write
  access to your machine.
- Rate limits, quotas, and Roblox-side API behavior. Report those to Roblox.

## Verifying a download

Each release archive carries a build provenance attestation, a signed
statement of which workflow run and commit produced it:

```sh
gh attestation verify rbx-download-v0.0.0-x86_64-unknown-linux-gnu.zip --repo rbx-forge/rbx-download
```
