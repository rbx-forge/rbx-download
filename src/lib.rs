//! Download Roblox assets by id.
//!
//! Two backends:
//! - public (default): Roblox's older `assetdelivery.roblox.com` endpoint,
//!   with an optional `.ROBLOSECURITY` cookie: `--cookie`, `RBX_COOKIE`, or a
//!   signed-in Studio once the user has said yes to it. Broad reach.
//! - cloud: the Open Cloud `asset-delivery-api` with `--api-key`. Supports
//!   `--asset-version <n>` to pin a specific asset version.
//!
//! "Older" is about Roblox's own surface, not about anything deprecated here:
//! the public endpoint is the **default** because it reaches more assets than
//! Open Cloud does. Neither is on the way out.
//!
//! Asset type → file extension is resolved from `economy.roblox.com` metadata,
//! unless `--type` is given (a numeric AssetTypeId or an alias), in which case
//! no economy lookup happens at all.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use colored::Colorize;

use rbx_core::api::{ApiBase, RetryPolicy};

use http::{Endpoint, Http};

#[derive(Parser, Debug)]
#[command(
    name = "rbx-download",
    version,
    about = "Download Roblox assets by id",
    long_about = "Download Roblox assets by id: images, audio, meshes, models, animations, \
                  places, and more. Uses the public assetdelivery endpoint by default, and \
                  Open Cloud whenever an API key is available."
)]
pub struct Cli {
    /// Asset ids to download.
    pub ids: Vec<u64>,

    /// Read additional asset ids from a file: separated by whitespace, commas
    /// or newlines, with `#` starting a comment that runs to the end of the
    /// line.
    #[arg(short = 'f', long)]
    pub file: Option<PathBuf>,

    /// Directory to write downloaded files into.
    #[arg(short = 'o', long, default_value = "downloads")]
    pub output: PathBuf,

    /// Force the public assetdelivery backend even when an API key is set.
    /// By default the Open Cloud backend is used whenever an --api-key /
    /// RBX_API_KEY is available, otherwise the public endpoint.
    #[arg(long, conflicts_with = "asset_version")]
    pub public: bool,

    /// Skip the economy metadata lookup by giving the asset type yourself:
    /// a numeric AssetTypeId, or an alias (image, audio, mesh, lua, place,
    /// model, animation, video, font).
    #[arg(long = "type")]
    pub type_spec: Option<String>,

    /// Pin a specific asset version (Open Cloud only; selects the cloud
    /// backend and requires an API key + exactly one id).
    ///
    /// Not `--version`, which prints this tool's own version, as every
    /// command-line tool's does.
    #[arg(long = "asset-version", value_name = "N")]
    pub asset_version: Option<u64>,

    /// Don't dereference Animation wrappers to their KeyframeSequence.
    #[arg(long)]
    pub raw: bool,

    /// Overwrite a file that is already there with different bytes.
    ///
    /// Without it such an asset is refused and the run exits non-zero: the
    /// filename does not carry the asset version, so the file on disk may be
    /// another version of the same asset.
    #[arg(long)]
    pub force: bool,

    /// Open Cloud API key. Selects the cloud backend.
    // `hide_env_values` is not cosmetic: without it clap prints
    // `[env: RBX_API_KEY=<the actual key>]` in `--help`, so a key leaks into any
    // pasted output, CI log or screenshot of the help page.
    #[arg(long, short = 'a', env = "RBX_API_KEY", hide_env_values = true)]
    pub api_key: Option<String>,

    /// Roblox session cookie (`.ROBLOSECURITY`), for the public backend.
    ///
    /// Extends its reach to assets your account can see. Defaults to the
    /// `RBX_COOKIE` env var, then to a signed-in Studio, which is asked about
    /// first unless `--auto-cookie` is set.
    #[arg(long, env = "RBX_COOKIE", hide_env_values = true)]
    pub cookie: Option<String>,

    /// Never look for a signed-in Studio: fetch strictly what is public.
    #[arg(long)]
    pub no_auto_cookie: bool,

    /// Use a signed-in Studio's session without asking first.
    ///
    /// A `.ROBLOSECURITY` cookie is a full-account credential, so finding one
    /// on the machine is not the same as being allowed to send it. Without this
    /// flag an interactive run asks once and a non-interactive one refuses.
    #[arg(long, conflicts_with = "no_auto_cookie")]
    pub auto_cookie: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The older assetdelivery endpoint (cookie auth, broad reach). Default.
    Public,
    /// Open Cloud asset-delivery-api (api-key auth, supports --asset-version).
    Cloud,
}

pub async fn run(cli: Cli) -> Result<()> {
    let mut ids = cli.ids.clone();
    if let Some(path) = &cli.file {
        ids.extend(read_ids_file(path)?);
    }
    if ids.is_empty() {
        bail!("No asset ids given. Pass ids positionally or via --file <path>.");
    }

    // Backend selection: --public forces public; otherwise an available API
    // key (or --asset-version) means the caller wants Open Cloud.
    let source = if cli.public {
        Source::Public
    } else if cli.asset_version.is_some() || cli.api_key.is_some() {
        Source::Cloud
    } else {
        Source::Public
    };

    if cli.asset_version.is_some() && ids.len() != 1 {
        bail!("--asset-version targets a single asset; pass exactly one id.");
    }

    // Resolve the type override once (shared by every id).
    let forced = cli.type_spec.as_deref().map(resolve_type).transpose()?;

    // Auth per backend. The cookie goes through the one resolution chain, so
    // the consent prompt, the refusals and the stderr notice are the shared
    // ones rather than a second copy of them.
    let http = Http::new(CONNECT_TIMEOUT, READ_TIMEOUT, RetryPolicy::default())?;
    let cookie_header = match source {
        Source::Public => rbx_core::env::resolve_cookie(
            cli.cookie.as_deref(),
            cli.no_auto_cookie,
            cli.auto_cookie,
        )
        .map(|c| format!(".ROBLOSECURITY={c}")),
        Source::Cloud => None,
    };
    let api_key =
        match source {
            Source::Cloud => Some(cli.api_key.clone().context(
                "the cloud backend needs an Open Cloud key (--api-key or RBX_API_KEY).",
            )?),
            Source::Public => None,
        };

    std::fs::create_dir_all(&cli.output)
        .with_context(|| format!("creating output dir {}", cli.output.display()))?;
    sweep_stale_parts(&cli.output, STALE_PART_AGE);

    let ctx = Ctx {
        http,
        hosts: Hosts::default(),
        cookie_header,
        api_key,
        source,
        output: cli.output,
        version: cli.asset_version,
        raw: cli.raw,
        force: cli.force,
        forced,
    };

    download_all(&ctx, &ids).await
}

/// Each id in turn, one line apiece. One that fails or is refused does not
/// stop the others; the run fails at the end if any did.
async fn download_all(ctx: &Ctx, ids: &[u64]) -> Result<()> {
    println!("Downloading {} asset(s) via {:?}...", ids.len(), ctx.source);
    let mut ok = 0usize;
    let mut already_there = 0usize;
    let mut refused = 0usize;
    let mut failed = 0usize;
    for &id in ids {
        match download_one(ctx, id).await {
            Ok(Saved::Written(name)) => {
                println!("  {} {}", "✓".green(), name);
                ok += 1;
            }
            Ok(Saved::AlreadyThere(name)) => {
                println!("  {} {} (already there)", "=".green(), name);
                already_there += 1;
            }
            Ok(Saved::Refused(path)) => {
                println!(
                    "  {} asset {}: {} is already there with different bytes \
                     (--force overwrites it)",
                    "✗".red(),
                    id,
                    path.display()
                );
                refused += 1;
            }
            Err(e) => {
                // `{:#}`: the whole chain on one line, so the status, the body
                // and a wait too long to sit out are shown with the sentence.
                println!("  {} asset {}: {:#}", "✗".red(), id, e);
                failed += 1;
            }
        }
    }

    let mut summary = format!("{ok} ok");
    if already_there > 0 {
        summary.push_str(&format!(", {already_there} already there"));
    }
    if refused > 0 {
        summary.push_str(&format!(", {refused} refused"));
    }
    summary.push_str(&format!(", {failed} failed"));
    println!("\n{} {}", "Done:".bold(), summary);

    match (failed, refused) {
        (0, 0) => Ok(()),
        (0, r) => bail!("{r} asset(s) refused: a different file is already there"),
        (f, 0) => bail!("{f} download(s) failed"),
        (f, r) => bail!("{f} download(s) failed, {r} refused"),
    }
}

/// What became of an asset's bytes on disk.
#[derive(Debug, PartialEq, Eq)]
enum Saved {
    /// Written to a new file, or over a different one under `--force`.
    Written(String),
    /// The file was already there with these same bytes, and is left alone.
    AlreadyThere(String),
    /// A file with different bytes is in the way and `--force` was not given.
    Refused(PathBuf),
}

/// The three hosts this tool reaches, one field each.
///
/// Injectable so the request shaping can run against a mock server.
struct Hosts {
    /// `apis.roblox.com`: Open Cloud asset delivery, needs an api key.
    cloud: ApiBase,
    /// `assetdelivery.roblox.com`: the public endpoint, cookie-optional.
    delivery: ApiBase,
    /// `economy.roblox.com`: asset name and type.
    economy: ApiBase,
}

impl Default for Hosts {
    fn default() -> Self {
        Self {
            cloud: ApiBase::default(),
            delivery: ApiBase::new(DELIVERY_HOST),
            economy: ApiBase::new(ECONOMY_HOST),
        }
    }
}

const DELIVERY_HOST: &str = "https://assetdelivery.roblox.com";
const ECONOMY_HOST: &str = "https://economy.roblox.com";

/// How long opening a connection may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the server may go without sending anything, waiting for the
/// response or in the middle of its body.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Everything that goes out to the network, and everything read back.
///
/// A module so the boundary is enforced, not remembered: the client sits in a
/// private field of [`Http`], and a response only leaves as an [`Answer`]
/// whose reads are the ones below. A reqwest error carries the full URL of its
/// request and every error ends up printed, while a CDN location is a signed
/// link anyone holding it can download with. So the URL comes off every error
/// here, request and body read alike, and a call site added later has no way
/// to send or read around that.
mod http {
    use std::cell::Cell;
    use std::fmt;
    use std::time::Duration;

    use anyhow::{bail, Context, Result};
    use rbx_core::api::{execute_with_retry_policy, ApiError, RetryPolicy};
    use reqwest::StatusCode;

    /// As many redirects as one request may follow: reqwest's own default.
    const MAX_REDIRECTS: usize = 10;

    /// How much of a refusal's body is shown, in characters.
    const SHOWN_BODY_CHARS: usize = 300;

    /// How one endpoint's refusals are told.
    #[derive(Debug)]
    pub(super) struct Endpoint {
        /// The sentence a refusal opens with.
        pub(super) failed: &'static str,
        /// Whether a refusal's body may be shown. Never for a CDN: an
        /// S3-style refusal echoes the signature and the string it signed.
        pub(super) body_is_safe: bool,
        /// The endpoint's own sentence read out of a refusal's body. It then
        /// stands in for `failed`, and the body is not shown a second time.
        pub(super) sentence_in_body: fn(&str) -> Option<String>,
    }

    impl Endpoint {
        /// The sentence when the refusal came from wherever a redirect led,
        /// which is not this endpoint and whose body is not safe either.
        const REDIRECTED: &'static str = "the download it redirected to failed";

        fn tell(&self, error: anyhow::Error, answered_elsewhere: bool) -> anyhow::Error {
            let Some(answer) = error.downcast_ref::<ApiError>() else {
                return error;
            };
            let (sentence, body) = if answered_elsewhere {
                (Self::REDIRECTED.to_string(), None)
            } else if !self.body_is_safe {
                (self.failed.to_string(), None)
            } else if let Some(sentence) = (self.sentence_in_body)(answer.body()) {
                (sentence, None)
            } else {
                (self.failed.to_string(), shown_body(answer.body()))
            };
            let refused = Refused {
                status: answer.status(),
                body,
            };
            // The retry layer puts its own sentence on top of the answer when
            // it has one, a wait too long to sit out.
            let retry_said = (error.chain().count() > 1).then(|| error.to_string());
            let mut told = anyhow::Error::new(refused);
            if let Some(said) = retry_said {
                told = told.context(said);
            }
            told.context(sentence)
        }
    }

    /// A body as a refusal shows it: on one line, and cut short.
    fn shown_body(body: &str) -> Option<String> {
        let one_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
        if one_line.is_empty() {
            return None;
        }
        if one_line.chars().count() <= SHOWN_BODY_CHARS {
            return Some(one_line);
        }
        let cut: String = one_line.chars().take(SHOWN_BODY_CHARS).collect();
        Some(format!("{cut}..."))
    }

    /// The status of a refusal, and as much of its body as may be shown.
    #[derive(Debug)]
    struct Refused {
        status: StatusCode,
        body: Option<String>,
    }

    impl fmt::Display for Refused {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match &self.body {
                Some(body) => write!(f, "{}: {body}", self.status),
                None => write!(f, "{}", self.status),
            }
        }
    }

    impl std::error::Error for Refused {}

    /// The client every request goes through, and how it retries.
    #[derive(Debug)]
    pub(super) struct Http {
        client: reqwest::Client,
        retry: RetryPolicy,
    }

    /// A response that succeeded, readable only through methods that keep the
    /// URL out of their errors.
    #[derive(Debug)]
    pub(super) struct Answer(reqwest::Response);

    impl Answer {
        pub(super) fn headers(&self) -> &reqwest::header::HeaderMap {
            self.0.headers()
        }

        /// reqwest 0.12 puts no URL on a body error. That is its choice to
        /// change, not a promise, so the URL is stripped here as well.
        pub(super) async fn bytes(self) -> Result<Vec<u8>> {
            Ok(self
                .0
                .bytes()
                .await
                .map_err(reqwest::Error::without_url)?
                .to_vec())
        }

        pub(super) async fn json(self) -> Result<serde_json::Value> {
            Ok(self.0.json().await.map_err(reqwest::Error::without_url)?)
        }
    }

    impl Http {
        /// No total timeout: one would cap the whole transfer, body included,
        /// and cut a large place on a slow link off while its bytes are still
        /// arriving. A connect timeout and a read timeout catch a dead server
        /// and a stalled one, and leave a slow download alone for as long as
        /// it keeps moving.
        ///
        /// The client follows no redirect itself: [`Http::follow`] does, and
        /// decides at each hop whether the credential goes along.
        pub(super) fn new(connect: Duration, read: Duration, retry: RetryPolicy) -> Result<Self> {
            let client = reqwest::Client::builder()
                .gzip(true)
                .connect_timeout(connect)
                .read_timeout(read)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("building the HTTP client")?;
            Ok(Self { client, retry })
        }

        /// GET `url` through [`Http::follow`], retrying what is worth retrying.
        ///
        /// Every request here is a GET that changes nothing, so sending it
        /// again is always safe: a 429, a 5xx, a timeout or a failed
        /// connection is retried with backoff, as the retry layer does. Only
        /// the request is retried, up to its response headers; a body that
        /// breaks off halfway fails that asset.
        ///
        /// A failure that came with an answer is told as the endpoint's
        /// sentence, then what the retry layer said on top of the answer when
        /// it said anything (a "this limit reopens in 300s, run the same
        /// command again after that"), then the status, and the body only
        /// where [`Endpoint`] allows it.
        pub(super) async fn get(
            &self,
            url: &str,
            credential: Option<(&str, &str)>,
            endpoint: &Endpoint,
        ) -> Result<Answer> {
            let answered_elsewhere = Cell::new(false);
            execute_with_retry_policy(
                || self.follow(url, credential, &answered_elsewhere),
                &self.retry,
            )
            .await
            .map(Answer)
            .map_err(|error| endpoint.tell(error, answered_elsewhere.get()))
        }

        /// GET `url`, with a credential header if one is given, following
        /// redirects.
        ///
        /// The delivery endpoints redirect to a CDN. reqwest drops
        /// `Authorization` and `Cookie` when a redirect leaves the host, but
        /// forwards every other header, `x-api-key` included, so the client
        /// follows nothing and this does it instead. The credential goes with
        /// a hop only while every hop so far has stayed on the origin it was
        /// given for (scheme, host and port), and once dropped it stays
        /// dropped.
        ///
        /// `answered_elsewhere` says whether the response that ends the chain
        /// came from another origin than `url`'s: a CDN, after a redirect.
        async fn follow(
            &self,
            url: &str,
            credential: Option<(&str, &str)>,
            answered_elsewhere: &Cell<bool>,
        ) -> Result<reqwest::Response> {
            // Not `{url}` in the message: a download location is a signed URL.
            let mut url =
                reqwest::Url::parse(url).context("the address to download from is not a URL")?;
            let origin = url.origin();
            let mut credential = credential;
            for _ in 0..=MAX_REDIRECTS {
                let mut request = self.client.get(url.clone());
                if let Some((name, value)) = credential {
                    request = request.header(name, value);
                }
                let response = request.send().await.map_err(reqwest::Error::without_url)?;
                answered_elsewhere.set(url.origin() != origin);
                if !response.status().is_redirection() {
                    return Ok(response);
                }
                let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                    return Ok(response);
                };
                let next = location
                    .to_str()
                    .ok()
                    .and_then(|location| url.join(location).ok())
                    .with_context(|| {
                        format!(
                            "{} redirected to an unreadable location",
                            url.host_str().unwrap_or("the server")
                        )
                    })?;
                if next.origin() != origin {
                    credential = None;
                }
                url = next;
            }
            bail!("more than {MAX_REDIRECTS} redirects")
        }
    }
}

struct Ctx {
    http: Http,
    hosts: Hosts,
    cookie_header: Option<String>,
    api_key: Option<String>,
    source: Source,
    output: PathBuf,
    version: Option<u64>,
    raw: bool,
    /// `--force`: overwrite a file that is there with different bytes.
    force: bool,
    /// `(extension, type_id)` from --type, when given.
    forced: Option<(String, Option<i64>)>,
}

async fn download_one(ctx: &Ctx, id: u64) -> Result<Saved> {
    // Determine name + extension + type id, fetching economy metadata only if
    // --type wasn't supplied.
    let (name, ext, type_id) = match &ctx.forced {
        Some((ext, type_id)) => (None, ext.clone(), *type_id),
        None => {
            let (name, type_id) = fetch_details(ctx, id).await?;
            let ext = type_id.map(ext_for_type_id).unwrap_or("bin").to_string();
            (name, ext, type_id)
        }
    };

    let bytes = download_bytes(ctx, id, ctx.version).await?;

    // Animation wrapper → dereference to the referenced KeyframeSequence.
    if !ctx.raw && type_id.map(is_animation_type).unwrap_or(false) && is_animation_wrapper(&bytes) {
        if let Some(kf_id) = extract_rbxassetid(&bytes) {
            println!("    ↪ animation wrapper → KeyframeSequence {}", kf_id);
            // KeyframeSequences are not versioned the same way; fetch latest.
            let kf_bytes = download_bytes(ctx, kf_id, None).await?;
            return save(ctx, id, &name, "rbxm", &kf_bytes);
        }
    }

    save(ctx, id, &name, &ext, &bytes)
}

/// GET the asset bytes from the selected backend. Errors on HTTP failure or a
/// JSON error body (the public endpoint returns 200 + JSON on some failures).
async fn download_bytes(ctx: &Ctx, id: u64, version: Option<u64>) -> Result<Vec<u8>> {
    match ctx.source {
        Source::Cloud => {
            let key = ctx.api_key.as_deref().expect("cloud source has api key");
            let cloud = &ctx.hosts.cloud;
            let url = match version {
                Some(v) => cloud.join(&format!("/asset-delivery-api/v1/assetId/{id}/version/{v}")),
                None => cloud.join(&format!("/asset-delivery-api/v1/assetId/{id}")),
            };
            let resp = ctx
                .http
                .get(&url, Some(("x-api-key", key)), &ASSET_DELIVERY)
                .await?;
            let meta = resp.json().await?;
            let location = meta
                .get("location")
                .and_then(|v| v.as_str())
                .context("no download location in asset-delivery response")?;
            // No key: the location carries its own authorisation, and is a
            // host this code did not choose.
            let dl = ctx.http.get(location, None, &CDN).await?;
            dl.bytes().await
        }
        Source::Public => {
            let delivery = &ctx.hosts.delivery;
            let url = delivery.join(&format!("/v1/asset/?id={id}"));
            let resp = ctx.http.get(&url, cookie(ctx), &PUBLIC_DELIVERY).await?;
            let is_json = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.contains("application/json"))
                .unwrap_or(false);
            let bytes = resp.bytes().await?;
            if is_json {
                // Body is an error payload, not the asset.
                bail!(
                    "{}",
                    public_refusal(&bytes).unwrap_or_else(|| NOT_AVAILABLE.to_string())
                );
            }
            Ok(bytes)
        }
    }
}

/// The Open Cloud lookup that names the download location.
const ASSET_DELIVERY: Endpoint = Endpoint {
    failed: "the asset-delivery lookup failed",
    body_is_safe: true,
    sentence_in_body: no_sentence,
};

/// The download location the lookup names: signed, and so is its refusal.
const CDN: Endpoint = Endpoint {
    failed: "the CDN download failed",
    body_is_safe: false,
    sentence_in_body: no_sentence,
};

/// The public endpoint, whose refusals are JSON with a sentence inside.
const PUBLIC_DELIVERY: Endpoint = Endpoint {
    failed: "assetdelivery refused the download",
    body_is_safe: true,
    sentence_in_body: public_sentence,
};

const ECONOMY_DETAILS: Endpoint = Endpoint {
    failed: "the economy details lookup failed (tip: pass --type to skip it)",
    body_is_safe: true,
    sentence_in_body: no_sentence,
};

fn no_sentence(_body: &str) -> Option<String> {
    None
}

fn public_sentence(body: &str) -> Option<String> {
    public_refusal(body.as_bytes())
}

/// What the public endpoint's JSON error body says, when the body is JSON.
fn public_refusal(body: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let message = value
        .get("errors")
        .and_then(|e| e.get(0))
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map_or(NOT_AVAILABLE, |m| m);
    Some(message.to_string())
}

/// The public endpoint's refusal when its JSON body names no reason.
const NOT_AVAILABLE: &str = "asset not available (may be private or moderated)";

/// The `Cookie` header for [`Http::get`], when the run has a session.
fn cookie(ctx: &Ctx) -> Option<(&str, &str)> {
    ctx.cookie_header.as_deref().map(|value| ("cookie", value))
}

/// Fetch `(Name, AssetTypeId)` from the economy details endpoint.
async fn fetch_details(ctx: &Ctx, id: u64) -> Result<(Option<String>, Option<i64>)> {
    let economy = &ctx.hosts.economy;
    let url = economy.join(&format!("/v2/assets/{id}/details"));
    let resp = ctx.http.get(&url, cookie(ctx), &ECONOMY_DETAILS).await?;
    let v = resp.json().await?;
    let name = v.get("Name").and_then(|n| n.as_str()).map(str::to_string);
    let type_id = v.get("AssetTypeId").and_then(|t| t.as_i64());
    Ok((name, type_id))
}

/// Write the bytes under the asset's filename, unless something is in the way.
///
/// The filename does not carry the asset version, so a file already there may
/// be another version of the same asset, or a file the user edited. The same
/// bytes are left alone, with or without `--force`; different ones are refused
/// unless `--force`.
///
/// The bytes are written to a [`PartFile`] beside the target and moved into
/// place whole, so the target is never a truncated file: not after a write
/// that failed, not after a Ctrl-C, and not under `--force`, where the user's
/// file stays as it was until the new one is complete.
fn save(ctx: &Ctx, id: u64, name: &Option<String>, ext: &str, bytes: &[u8]) -> Result<Saved> {
    let filename = match name.as_deref().map(rbx_core::fs_name::safe_component) {
        Some(safe) if !safe.is_empty() => format!("{id}_{safe}.{ext}"),
        _ => format!("{id}.{ext}"),
    };
    let path = ctx.output.join(&filename);
    let reading = || format!("reading {}", path.display());
    let writing = || format!("writing {}", path.display());

    // What is there decides first, so a refused asset costs no write.
    match same_contents(&path, bytes) {
        Ok(true) => return Ok(Saved::AlreadyThere(filename)),
        Ok(false) if !ctx.force => return Ok(Saved::Refused(path)),
        Ok(false) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(reading),
    }

    let part = PartFile::write(&ctx.output, &filename, bytes).with_context(writing)?;
    if ctx.force {
        part.replace(&path).with_context(writing)?;
        return Ok(Saved::Written(filename));
    }
    match part.place_new(&path) {
        Ok(()) => Ok(Saved::Written(filename)),
        // Something took the name since the check above.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if same_contents(&path, bytes).with_context(reading)? {
                Ok(Saved::AlreadyThere(filename))
            } else {
                Ok(Saved::Refused(path))
            }
        }
        Err(e) => Err(e).with_context(writing),
    }
}

/// A download written beside its final name as `.<name>.<pid>.part`, then
/// moved onto it whole.
///
/// The process id keeps two runs in one directory out of each other's way.
/// Dropping one removes it, which covers every error on the way; a run that is
/// killed leaves it behind, and [`sweep_stale_parts`] clears it on a later run.
#[derive(Debug)]
struct PartFile {
    path: PathBuf,
}

impl PartFile {
    fn write(dir: &std::path::Path, filename: &str, bytes: &[u8]) -> std::io::Result<Self> {
        use std::io::Write as _;

        let path = dir.join(format!(".{filename}.{}.part", std::process::id()));
        // Something at this name is a part an earlier run under the same
        // process id left behind, or a link someone put there. Removing it
        // removes the link and never what it points to, and `create_new` then
        // makes a file of this run's own: nothing is ever written through a
        // hard link or a symlink found at the name.
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        // Ours from here, so dropping it on an error below removes it.
        let part = Self { path };
        file.write_all(bytes)?;
        // On disk before it takes the name: a crash just after the move must
        // not leave the name on bytes that were still in a cache.
        file.sync_all()?;
        Ok(part)
    }

    /// Onto `target`, over whatever is there. A rename replaces its target in
    /// one step, so `target` is the old file or the new one, never half of
    /// either.
    fn replace(self, target: &std::path::Path) -> std::io::Result<()> {
        std::fs::rename(&self.path, target)
    }

    /// Onto `target`, only if nothing is there: `AlreadyExists` otherwise.
    ///
    /// A hard link refuses an existing target in one step, the promise
    /// `create_new` makes, and the part is then dropped. A file system without
    /// hard links (FAT, some network shares) gets a check and a rename instead,
    /// which leaves a moment between the two.
    fn place_new(self, target: &std::path::Path) -> std::io::Result<()> {
        match std::fs::hard_link(&self.path, target) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
            Err(_) if target.try_exists()? => Err(std::io::ErrorKind::AlreadyExists.into()),
            Err(_) => std::fs::rename(&self.path, target),
        }
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        // Gone already after a rename, which is the usual case.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// How old a part file must be before a run treats it as left behind.
///
/// A run writes each part in one go and moves it at once, so one still there a
/// minute later belongs to a run that was killed.
const STALE_PART_AGE: Duration = Duration::from_secs(60);

/// Remove the part files a killed run left in `dir`, older than `age`.
///
/// Only names of the shape [`PartFile`] makes, `.<name>.<digits>.part`, so a
/// file of the user's that happens to end in `.part` is left alone.
fn sweep_stale_parts(dir: &std::path::Path, age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_part = name
            .strip_prefix('.')
            .and_then(|rest| rest.strip_suffix(".part"))
            .and_then(|rest| rest.rsplit_once('.'))
            .is_some_and(|(stem, pid)| {
                !stem.is_empty() && !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
            });
        let is_stale = entry
            .metadata()
            .ok()
            .filter(std::fs::Metadata::is_file)
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|elapsed| elapsed > age);
        if is_part && is_stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Whether the file at `path` holds exactly `bytes`.
fn same_contents(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<bool> {
    let len = std::fs::metadata(path)?.len();
    same_contents_as(len, || std::fs::read(path), bytes)
}

/// [`same_contents`] with the file's size and its reading passed in. A file
/// of another size is different without reading it: a place file can run to
/// hundreds of megabytes, and a different size is the common case for another
/// version.
fn same_contents_as(
    len: u64,
    read: impl FnOnce() -> std::io::Result<Vec<u8>>,
    bytes: &[u8],
) -> std::io::Result<bool> {
    if len != bytes.len() as u64 {
        return Ok(false);
    }
    Ok(read()? == bytes)
}

fn read_ids_file(path: &PathBuf) -> Result<Vec<u64>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_ids(&text).with_context(|| format!("reading ids from {}", path.display()))
}

/// The ids in an ids file: separated by whitespace, commas or newlines, with
/// `#` opening a comment that runs to the end of its line.
///
/// The comment is cut off per line before anything is split. Splitting first
/// and skipping the words that start with `#` drops the `#` and keeps the rest
/// of the sentence, so `# hats for the lobby` failed on `hats`.
fn parse_ids(text: &str) -> Result<Vec<u64>> {
    let mut ids = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let code = line.split_once('#').map_or(line, |(code, _comment)| code);
        for token in code.split(|c: char| c.is_whitespace() || c == ',') {
            if token.is_empty() {
                continue;
            }
            let id: u64 = token
                .parse()
                .with_context(|| format!("invalid asset id on line {}: '{token}'", number + 1))?;
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Resolve a `--type` spec to `(extension, optional AssetTypeId)`. The type id
/// is only needed so animation wrappers can still be dereferenced.
fn resolve_type(spec: &str) -> Result<(String, Option<i64>)> {
    if let Ok(id) = spec.parse::<i64>() {
        return Ok((ext_for_type_id(id).to_string(), Some(id)));
    }
    let (ext, type_id): (&str, Option<i64>) = match spec.to_lowercase().as_str() {
        "image" | "png" | "decal" => ("png", None),
        "audio" | "ogg" | "sound" => ("ogg", None),
        "mesh" => ("mesh", None),
        "lua" | "luau" | "script" => ("lua", None),
        "place" | "rbxl" => ("rbxl", None),
        "model" | "rbxm" => ("rbxm", None),
        "animation" | "anim" => ("rbxm", Some(24)),
        "video" | "mp4" => ("mp4", None),
        "font" | "ttf" => ("ttf", None),
        other => bail!(
            "unknown --type '{}'. Use a numeric AssetTypeId or one of: \
             image, audio, mesh, lua, place, model, animation, video, font",
            other
        ),
    };
    Ok((ext.to_string(), type_id))
}

/// AssetTypeId → file extension (matching the Roblox enum). Unknown → "bin".
///
/// Every arm here is a claim about what the bytes are, so an unmeasured guess
/// belongs in `bin` rather than in a plausible-looking extension. `bin` says
/// "we do not know"; a wrong extension tells the file manager, the browser and
/// the next person something false.
fn ext_for_type_id(id: i64) -> &'static str {
    match id {
        1 | 2 | 11 | 12 | 13 | 18 | 21 | 34 => "png",
        3 => "ogg",
        4 => "mesh",
        5 => "lua",
        9 => "rbxl",
        62 => "mp4",
        73 => "ttf",
        // `GamePreviewVideo`, the clip that plays in an experience's carousel.
        // Not a video file: the asset is an HLS **master playlist**, opening
        // `#EXTM3U` and listing five vp9/opus renditions from 1280x720 at 60fps
        // down to 160x96 at 5fps. Measured on a live public asset, which even
        // labels itself with `RBX-VIDEOTYPE="GamePreviewVideo"`.
        //
        // The obvious guess would have been `mp4`, by analogy with `Video =
        // 62` above, and it would have been wrong: what lands on disk is a
        // 1.1 KB text manifest that names its segments somewhere else.
        //
        // `AdsVideo = 81` and `StorePreviewVideo = 85` are almost certainly the
        // same shape and are deliberately left in `bin`, because "almost
        // certainly" is the reasoning this function does not accept.
        86 => "m3u8",
        8 | 10 | 17 | 19 | 24 | 27..=32 | 38 | 40..=58 | 61 | 64..=72 | 76..=79 | 88..=90 => "rbxm",
        _ => "bin",
    }
}

fn is_animation_type(id: i64) -> bool {
    matches!(id, 24 | 48..=56 | 61 | 78)
}

fn is_animation_wrapper(content: &[u8]) -> bool {
    contains(content, b"Animation")
        && contains(content, b"AnimationId")
        && !contains(content, b"KeyframeSequence")
}

/// Find the first `rbxassetid://<digits>` reference and return the id.
fn extract_rbxassetid(content: &[u8]) -> Option<u64> {
    let needle = b"rbxassetid://";
    let start = content.windows(needle.len()).position(|w| w == needle)? + needle.len();
    let digits: Vec<u8> = content[start..]
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    std::str::from_utf8(&digits).ok()?.parse().ok()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use clap::CommandFactory;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Every host on one server: a test wants one place answering whatever
    /// the code asks for.
    fn ctx(server: &MockServer, source: Source, api_key: Option<&str>) -> Ctx {
        ctx_at(&server.uri(), source, api_key)
    }

    /// [`ctx`] against any address, for the servers that are not mocks.
    fn ctx_at(base: &str, source: Source, api_key: Option<&str>) -> Ctx {
        Ctx {
            http: Http::new(CONNECT_TIMEOUT, READ_TIMEOUT, NO_WAIT).unwrap(),
            hosts: Hosts {
                cloud: ApiBase::new(base),
                delivery: ApiBase::new(base),
                economy: ApiBase::new(base),
            },
            cookie_header: None,
            api_key: api_key.map(str::to_string),
            source,
            output: PathBuf::from("."),
            version: None,
            raw: false,
            force: false,
            forced: None,
        }
    }

    /// The default number of retries, without the backoff between them.
    const NO_WAIT: RetryPolicy = RetryPolicy {
        max_retries: 3,
        base_backoff_secs: 0,
    };

    /// An output directory of its own, and a `--type` so no economy lookup
    /// is needed: what a test of the files on disk wants.
    fn ctx_writing_to(server: &MockServer, dir: &tempfile::TempDir) -> Ctx {
        Ctx {
            output: dir.path().to_path_buf(),
            forced: Some(("rbxm".to_string(), None)),
            ..ctx(server, Source::Public, None)
        }
    }

    async fn serve_public(server: &MockServer, id: u64, body: &[u8]) {
        Mock::given(method("GET"))
            .and(path("/v1/asset/"))
            .and(query_param("id", id.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .mount(server)
            .await;
    }

    /// The same bytes twice is not a conflict: the second run says so and
    /// leaves the file alone.
    #[tokio::test]
    async fn the_same_bytes_already_on_disk_are_left_alone() {
        let server = MockServer::start().await;
        serve_public(&server, 42, b"v1").await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("42.rbxm"), b"v1").unwrap();

        let saved = download_one(&ctx_writing_to(&server, &dir), 42)
            .await
            .unwrap();
        assert_eq!(saved, Saved::AlreadyThere("42.rbxm".to_string()));
    }

    /// The filename carries no version, so another version of the asset has
    /// the same name. It is refused, the file keeps its bytes, the other ids
    /// still download, and the run fails at the end.
    #[tokio::test]
    async fn different_bytes_on_disk_are_refused_and_the_others_still_download() {
        let server = MockServer::start().await;
        serve_public(&server, 42, b"v2").await;
        serve_public(&server, 43, b"new").await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("42.rbxm"), b"v1").unwrap();

        let result = download_all(&ctx_writing_to(&server, &dir), &[42, 43]).await;

        assert!(result.is_err(), "a refused asset fails the run");
        assert_eq!(std::fs::read(dir.path().join("42.rbxm")).unwrap(), b"v1");
        assert_eq!(std::fs::read(dir.path().join("43.rbxm")).unwrap(), b"new");
    }

    #[tokio::test]
    async fn force_overwrites_different_bytes() {
        let server = MockServer::start().await;
        serve_public(&server, 42, b"v2").await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("42.rbxm"), b"v1").unwrap();
        let ctx = Ctx {
            force: true,
            ..ctx_writing_to(&server, &dir)
        };

        download_all(&ctx, &[42]).await.unwrap();
        assert_eq!(std::fs::read(dir.path().join("42.rbxm")).unwrap(), b"v2");
    }

    /// A file of another size is different, and is not read to find out.
    #[test]
    fn a_file_of_another_size_is_not_read() {
        let read = || -> std::io::Result<Vec<u8>> { panic!("read a file of another size") };
        assert!(!same_contents_as(3, read, b"four").unwrap());
        assert!(same_contents_as(4, || Ok(b"four".to_vec()), b"four").unwrap());
        assert!(!same_contents_as(4, || Ok(b"five".to_vec()), b"four").unwrap());
    }

    /// `--force` overwrites what is different. What is the same is still
    /// already there.
    #[tokio::test]
    async fn force_still_leaves_the_same_bytes_alone() {
        let server = MockServer::start().await;
        serve_public(&server, 42, b"v1").await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("42.rbxm"), b"v1").unwrap();
        let ctx = Ctx {
            force: true,
            ..ctx_writing_to(&server, &dir)
        };

        let saved = download_one(&ctx, 42).await.unwrap();
        assert_eq!(saved, Saved::AlreadyThere("42.rbxm".to_string()));
    }

    fn names_in(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// Under `--force`, a write that fails leaves the user's file as it was.
    /// The part file's name is taken by a directory here, so the write fails
    /// before anything reaches the target; writing the target in place would
    /// have truncated it first.
    #[tokio::test]
    async fn a_failed_write_under_force_leaves_the_existing_file_whole() {
        let server = MockServer::start().await;
        serve_public(&server, 42, b"v2").await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("42.rbxm"), b"v1").unwrap();
        let part = dir
            .path()
            .join(format!(".42.rbxm.{}.part", std::process::id()));
        std::fs::create_dir(&part).unwrap();
        let ctx = Ctx {
            force: true,
            ..ctx_writing_to(&server, &dir)
        };

        assert!(download_one(&ctx, 42).await.is_err());
        assert_eq!(std::fs::read(dir.path().join("42.rbxm")).unwrap(), b"v1");
    }

    /// A finished download leaves its file and nothing beside it, with or
    /// without `--force`.
    #[tokio::test]
    async fn a_download_leaves_no_part_file_behind() {
        let server = MockServer::start().await;
        serve_public(&server, 42, b"v2").await;
        serve_public(&server, 43, b"new").await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("42.rbxm"), b"v1").unwrap();
        let ctx = Ctx {
            force: true,
            ..ctx_writing_to(&server, &dir)
        };
        download_all(&ctx, &[42]).await.unwrap();
        download_all(&ctx_writing_to(&server, &dir), &[43])
            .await
            .unwrap();

        assert_eq!(names_in(dir.path()), ["42.rbxm", "43.rbxm"]);
    }

    /// A link found at the part's name is replaced, not written through: the
    /// file it points to keeps its bytes.
    #[test]
    fn a_part_is_never_written_through_a_link_at_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"keep").unwrap();
        let name = format!(".42.rbxm.{}.part", std::process::id());
        std::fs::hard_link(&victim, dir.path().join(&name)).unwrap();

        let part = PartFile::write(dir.path(), "42.rbxm", b"new").unwrap();

        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert_eq!(std::fs::read(&part.path).unwrap(), b"new");
    }

    /// The name taken between the check and the move is not overwritten, and
    /// the part goes.
    #[test]
    fn a_part_is_not_placed_over_a_file_that_appeared() {
        let dir = tempfile::tempdir().unwrap();
        let part = PartFile::write(dir.path(), "42.rbxm", b"new").unwrap();
        let target = dir.path().join("42.rbxm");
        std::fs::write(&target, b"theirs").unwrap();

        let error = part.place_new(&target).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&target).unwrap(), b"theirs");
        assert_eq!(names_in(dir.path()), ["42.rbxm"]);
    }

    /// A killed run's part file goes once it is old enough. A fresh one may be
    /// another run's, and a name of another shape is the user's.
    #[test]
    fn stale_part_files_are_swept_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str, age: Duration| {
            let path = dir.path().join(name);
            std::fs::write(&path, b"x").unwrap();
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(std::time::SystemTime::now() - age)
                .unwrap();
        };
        let old = Duration::from_secs(120);
        make(".42.rbxm.1234.part", old);
        make(".43.rbxm.1234.part", Duration::ZERO);
        make(".notes.part", old);
        make("draft.1234.part", old);
        make(".44.rbxm.12a4.part", old);

        sweep_stale_parts(dir.path(), STALE_PART_AGE);

        assert_eq!(
            names_in(dir.path()),
            [
                ".43.rbxm.1234.part",
                ".44.rbxm.12a4.part",
                ".notes.part",
                "draft.1234.part"
            ]
        );
    }

    /// A server that answers one request with `body`, sent in `pieces` parts
    /// with `gap` before each. Raw TCP because the mock server sends a body in
    /// one go, and a timeout on the body needs one that arrives slowly.
    fn trickling_server(body: &'static [u8], pieces: usize, gap: Duration) -> String {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.flush();
            // Write errors are the client giving up, which is what some tests
            // are waiting for.
            for piece in body.chunks(body.len().div_ceil(pieces)) {
                std::thread::sleep(gap);
                if stream
                    .write_all(piece)
                    .and_then(|()| stream.flush())
                    .is_err()
                {
                    return;
                }
            }
        });
        format!("http://{address}")
    }

    fn ctx_against(base: &str, read_timeout: Duration) -> Ctx {
        Ctx {
            http: Http::new(CONNECT_TIMEOUT, read_timeout, NO_WAIT).unwrap(),
            ..ctx_at(base, Source::Public, None)
        }
    }

    /// A body that keeps arriving is not a timeout however long it takes in
    /// all. With a total timeout equal to the read timeout, this download
    /// fails halfway.
    #[tokio::test]
    async fn a_slow_body_that_keeps_arriving_is_not_cut_off() {
        let base = trickling_server(b"0123456789", 5, Duration::from_millis(150));
        let ctx = ctx_against(&base, Duration::from_millis(500));

        let bytes = download_bytes(&ctx, 42, None).await.unwrap();
        assert_eq!(bytes, b"0123456789");
    }

    /// A body that stops arriving is.
    #[tokio::test]
    async fn a_body_that_stalls_is_cut_off() {
        let base = trickling_server(b"0123456789", 1, Duration::from_secs(3));
        let ctx = ctx_against(&base, Duration::from_millis(300));

        let started = std::time::Instant::now();
        assert!(download_bytes(&ctx, 42, None).await.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Every request `server` received for `path`.
    async fn requests_for(server: &MockServer, path: &str) -> Vec<wiremock::Request> {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == path)
            .collect()
    }

    fn redirect_to(location: String) -> ResponseTemplate {
        ResponseTemplate::new(302).insert_header("location", location)
    }

    /// The key is for the API host. A redirect elsewhere is followed without
    /// it, and reqwest's own redirect handling would have forwarded it.
    #[tokio::test]
    async fn the_api_key_does_not_follow_a_redirect_to_another_host() {
        let api = MockServer::start().await;
        let elsewhere = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/asset-delivery-api/v1/assetId/42"))
            .and(header("x-api-key", "k"))
            .respond_with(redirect_to(format!("{}/moved", elsewhere.uri())))
            .mount(&api)
            .await;
        Mock::given(method("GET"))
            .and(path("/moved"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "location": format!("{}/cdn/blob", elsewhere.uri())
            })))
            .mount(&elsewhere)
            .await;
        Mock::given(method("GET"))
            .and(path("/cdn/blob"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bytes".to_vec()))
            .mount(&elsewhere)
            .await;

        let bytes = download_bytes(&ctx(&api, Source::Cloud, Some("k")), 42, None)
            .await
            .unwrap();
        assert_eq!(bytes, b"bytes");
        for path in ["/moved", "/cdn/blob"] {
            for request in requests_for(&elsewhere, path).await {
                assert!(
                    request.headers.get("x-api-key").is_none(),
                    "the key reached {path} on another host"
                );
            }
        }
    }

    /// The public endpoint answers with a redirect to the CDN, and the cookie,
    /// a whole-account credential, stays behind.
    #[tokio::test]
    async fn the_cookie_does_not_follow_a_redirect_to_another_host() {
        let delivery = MockServer::start().await;
        let cdn = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/asset/"))
            .and(header("cookie", ".ROBLOSECURITY=c"))
            .respond_with(redirect_to(format!("{}/blob", cdn.uri())))
            .mount(&delivery)
            .await;
        Mock::given(method("GET"))
            .and(path("/blob"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bytes".to_vec()))
            .mount(&cdn)
            .await;
        let ctx = Ctx {
            cookie_header: Some(".ROBLOSECURITY=c".to_string()),
            ..ctx(&delivery, Source::Public, None)
        };

        assert_eq!(download_bytes(&ctx, 42, None).await.unwrap(), b"bytes");
        let [request] = &requests_for(&cdn, "/blob").await[..] else {
            panic!("the CDN was asked once");
        };
        assert!(request.headers.get("cookie").is_none());
    }

    /// A redirect that stays on the host keeps the credential, so a moved
    /// path still reaches what the account can see. The location is relative,
    /// as a server is free to send it.
    #[tokio::test]
    async fn a_redirect_on_the_same_host_keeps_the_credential() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/asset/"))
            .respond_with(redirect_to("/v2/asset/?id=42".to_string()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v2/asset/"))
            .and(header("cookie", ".ROBLOSECURITY=c"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bytes".to_vec()))
            .mount(&server)
            .await;
        let ctx = Ctx {
            cookie_header: Some(".ROBLOSECURITY=c".to_string()),
            ..ctx(&server, Source::Public, None)
        };

        assert_eq!(download_bytes(&ctx, 42, None).await.unwrap(), b"bytes");
    }

    /// Once the credential has been left behind, a hop back to its host does
    /// not pick it up again: the way back was chosen by the other host.
    #[tokio::test]
    async fn a_credential_left_behind_is_not_picked_up_again() {
        let delivery = MockServer::start().await;
        let elsewhere = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/asset/"))
            .respond_with(redirect_to(format!("{}/bounce", elsewhere.uri())))
            .mount(&delivery)
            .await;
        Mock::given(method("GET"))
            .and(path("/bounce"))
            .respond_with(redirect_to(format!("{}/back", delivery.uri())))
            .mount(&elsewhere)
            .await;
        Mock::given(method("GET"))
            .and(path("/back"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bytes".to_vec()))
            .mount(&delivery)
            .await;
        let ctx = Ctx {
            cookie_header: Some(".ROBLOSECURITY=c".to_string()),
            ..ctx(&delivery, Source::Public, None)
        };

        assert_eq!(download_bytes(&ctx, 42, None).await.unwrap(), b"bytes");
        let [back] = &requests_for(&delivery, "/back").await[..] else {
            panic!("/back was asked once");
        };
        assert!(back.headers.get("cookie").is_none());
    }

    /// Answers `status` to the first request for `path` only, ahead of
    /// whatever else is mounted for it.
    async fn fail_once(server: &MockServer, path_: &str, status: u16) {
        Mock::given(method("GET"))
            .and(path(path_))
            .respond_with(ResponseTemplate::new(status))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(server)
            .await;
    }

    /// A download is a GET that changes nothing, so a 5xx is worth another
    /// try rather than a failed asset.
    #[tokio::test]
    async fn a_server_error_on_the_public_endpoint_is_retried() {
        let server = MockServer::start().await;
        fail_once(&server, "/v1/asset/", 503).await;
        serve_public(&server, 42, b"bytes").await;

        let bytes = download_bytes(&ctx(&server, Source::Public, None), 42, None)
            .await
            .unwrap();
        assert_eq!(bytes, b"bytes");
        assert_eq!(requests_for(&server, "/v1/asset/").await.len(), 2);
    }

    /// Both halves of the cloud path: a rate limit on the lookup and a server
    /// error from the CDN.
    #[tokio::test]
    async fn the_cloud_lookup_and_the_cdn_are_both_retried() {
        let server = MockServer::start().await;
        fail_once(&server, "/asset-delivery-api/v1/assetId/42", 429).await;
        fail_once(&server, "/cdn/blob", 500).await;
        Mock::given(method("GET"))
            .and(path("/asset-delivery-api/v1/assetId/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "location": format!("{}/cdn/blob", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cdn/blob"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bytes".to_vec()))
            .mount(&server)
            .await;

        let bytes = download_bytes(&ctx(&server, Source::Cloud, Some("k")), 42, None)
            .await
            .unwrap();
        assert_eq!(bytes, b"bytes");
    }

    #[tokio::test]
    async fn a_server_error_on_the_details_is_retried() {
        let server = MockServer::start().await;
        fail_once(&server, "/v2/assets/42/details", 502).await;
        Mock::given(method("GET"))
            .and(path("/v2/assets/42/details"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"AssetTypeId": 8})),
            )
            .mount(&server)
            .await;

        let (_, type_id) = fetch_details(&ctx(&server, Source::Public, None), 42)
            .await
            .unwrap();
        assert_eq!(type_id, Some(8));
    }

    /// A 404 is an answer, and asking again gets the same one.
    #[tokio::test]
    async fn a_missing_asset_is_asked_for_once() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/asset/"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let error = download_bytes(&ctx(&server, Source::Public, None), 42, None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("404"), "got: {error:#}");
        assert_eq!(requests_for(&server, "/v1/asset/").await.len(), 1);
    }

    /// A rate limit too long to wait out is reported with how long it is,
    /// under the endpoint's own sentence rather than replaced by it.
    #[tokio::test]
    async fn a_rate_limit_too_long_to_wait_says_how_long() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/asset/"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "300"))
            .mount(&server)
            .await;

        let error = download_bytes(&ctx(&server, Source::Public, None), 42, None)
            .await
            .unwrap_err();
        let shown = format!("{error:#}");
        assert!(shown.contains("assetdelivery refused"), "got: {shown}");
        assert!(shown.contains("reopens in 300s"), "got: {shown}");
    }

    /// A server whose first connection hears the request and never answers,
    /// and whose later ones answer `body` straight away.
    fn server_that_stalls_once(body: &'static [u8]) -> String {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { return };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf);
                    if n == 0 {
                        std::thread::sleep(Duration::from_secs(3));
                        return;
                    }
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(body);
                });
            }
        });
        format!("http://{address}")
    }

    /// A server that accepts every connection and closes it unanswered.
    fn server_that_hangs_up() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });
        format!("http://{address}")
    }

    /// The signature that makes a CDN location a working download link.
    const SIGNATURE: &str = "sig=aaaaBBBBccccDDDD";

    /// The CDN location is a signed link, and a failed request to it is
    /// printed with its whole chain: the link must not be in it.
    #[tokio::test]
    async fn a_failed_cdn_request_does_not_print_its_signed_url() {
        let api = MockServer::start().await;
        let cdn = server_that_hangs_up();
        Mock::given(method("GET"))
            .and(path("/asset-delivery-api/v1/assetId/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "location": format!("{cdn}/blob?{SIGNATURE}")
            })))
            .mount(&api)
            .await;

        let error = download_bytes(&ctx(&api, Source::Cloud, Some("k")), 42, None)
            .await
            .unwrap_err();
        let shown = format!("{error:?}");
        assert!(!shown.contains(SIGNATURE), "got: {shown}");
    }

    /// The same for a body that breaks off after a redirect, where the
    /// response's own URL is the signed one. reqwest 0.12 leaves the URL off
    /// a body error by itself; this holds that whatever reqwest does.
    #[tokio::test]
    async fn a_body_that_breaks_off_after_a_redirect_does_not_print_its_signed_url() {
        let delivery = MockServer::start().await;
        let cdn = trickling_server(b"0123456789", 1, Duration::from_secs(3));
        Mock::given(method("GET"))
            .and(path("/v1/asset/"))
            .respond_with(redirect_to(format!("{cdn}/blob?{SIGNATURE}")))
            .mount(&delivery)
            .await;
        let ctx = Ctx {
            http: Http::new(CONNECT_TIMEOUT, Duration::from_millis(300), NO_WAIT).unwrap(),
            ..ctx(&delivery, Source::Public, None)
        };

        let error = download_bytes(&ctx, 42, None).await.unwrap_err();
        let shown = format!("{error:?}");
        assert!(!shown.contains(SIGNATURE), "got: {shown}");
    }

    /// An S3-style refusal from a CDN, which echoes what was signed.
    fn cdn_refusal() -> ResponseTemplate {
        ResponseTemplate::new(403).set_body_string(format!(
            "<Error><Code>SignatureDoesNotMatch</Code><StringToSign>{SIGNATURE}</StringToSign></Error>"
        ))
    }

    fn assert_no_cdn_body(shown: &str) {
        assert!(!shown.contains("SignatureDoesNotMatch"), "got: {shown}");
        assert!(!shown.contains(SIGNATURE), "got: {shown}");
    }

    /// The CDN's refusal is told by its status alone.
    #[tokio::test]
    async fn a_cdn_refusal_shows_its_status_and_not_its_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/asset-delivery-api/v1/assetId/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "location": format!("{}/cdn/blob", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cdn/blob"))
            .respond_with(cdn_refusal())
            .mount(&server)
            .await;

        let error = download_bytes(&ctx(&server, Source::Cloud, Some("k")), 42, None)
            .await
            .unwrap_err();
        let shown = format!("{error:#}");
        assert!(shown.contains("the CDN download failed"), "got: {shown}");
        assert!(shown.contains("403"), "got: {shown}");
        assert_no_cdn_body(&shown);
    }

    /// A refusal from where the public endpoint redirected is the CDN's, not
    /// the endpoint's, and its body is the CDN's too.
    #[tokio::test]
    async fn a_refusal_after_a_redirect_is_not_the_endpoints() {
        let delivery = MockServer::start().await;
        let cdn = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/asset/"))
            .respond_with(redirect_to(format!("{}/blob", cdn.uri())))
            .mount(&delivery)
            .await;
        Mock::given(method("GET"))
            .and(path("/blob"))
            .respond_with(cdn_refusal())
            .mount(&cdn)
            .await;

        let error = download_bytes(&ctx(&delivery, Source::Public, None), 42, None)
            .await
            .unwrap_err();
        let shown = format!("{error:#}");
        assert!(shown.contains("redirected to failed"), "got: {shown}");
        assert!(!shown.contains("assetdelivery"), "got: {shown}");
        assert_no_cdn_body(&shown);
    }

    /// The public endpoint's sentence is read from its body, so the body is
    /// not printed again underneath it.
    #[tokio::test]
    async fn the_public_endpoints_sentence_is_shown_once() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/asset/"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "errors": [{"code": 0, "message": "Request asset was not found"}]
            })))
            .mount(&server)
            .await;

        let error = download_bytes(&ctx(&server, Source::Public, None), 42, None)
            .await
            .unwrap_err();
        let shown = format!("{error:#}");
        assert_eq!(
            shown.matches("Request asset was not found").count(),
            1,
            "got: {shown}"
        );
        assert!(shown.contains("403"), "got: {shown}");
    }

    /// A body that is shown is cut short and kept on one line.
    #[tokio::test]
    async fn a_long_body_is_cut_short() {
        let server = MockServer::start().await;
        let body = format!("<html>\n{}\n</html>", "x".repeat(5000));
        Mock::given(method("GET"))
            .and(path("/v2/assets/42/details"))
            .respond_with(ResponseTemplate::new(400).set_body_string(body))
            .mount(&server)
            .await;

        let error = fetch_details(&ctx(&server, Source::Public, None), 42)
            .await
            .unwrap_err();
        let shown = format!("{error:#}");
        assert!(shown.contains("<html> xxx"), "got: {shown}");
        assert!(shown.ends_with("..."), "got: {shown}");
        assert!(shown.len() < 500, "{} characters", shown.len());
    }

    /// A request that times out waiting for its answer is sent again.
    #[tokio::test]
    async fn a_request_that_times_out_is_retried() {
        let base = server_that_stalls_once(b"bytes");
        let ctx = ctx_against(&base, Duration::from_millis(300));

        let started = std::time::Instant::now();
        assert_eq!(download_bytes(&ctx, 42, None).await.unwrap(), b"bytes");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// clap checks a command's definition only when it is built, and a clash
    /// such as two arguments both answering to `--version` panics at that
    /// moment: on a user's machine, at the first run. This builds it here.
    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// `--version` is the tool's own, and the asset's has a name of its own.
    #[test]
    fn version_prints_the_tool_and_asset_version_pins_the_asset() {
        let err = Cli::try_parse_from(["rbx-download", "--version"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);

        let cli = Cli::try_parse_from(["rbx-download", "42", "--asset-version", "3"]).unwrap();
        assert_eq!(cli.asset_version, Some(3));
    }

    /// The two cookie flags are opposites, and asking for both is refused
    /// rather than resolved by whichever is read last.
    #[test]
    fn the_cookie_flags_cannot_both_be_given() {
        assert!(
            Cli::try_parse_from(["rbx-download", "42", "--auto-cookie", "--no-auto-cookie"])
                .is_err()
        );
    }

    /// The format the help and the README describe: comments on a line of
    /// their own and after an id, blank lines, and every separator mixed.
    #[test]
    fn an_ids_file_takes_comments_blank_lines_and_any_separator() {
        let text = "# hats for the lobby\n\
                    1, 2 3\n\
                    \n\
                    \t4,5 # the one with the feather, 6\n\
                    7\r\n\
                    #8\n";
        assert_eq!(parse_ids(text).unwrap(), [1, 2, 3, 4, 5, 7]);
    }

    /// A word outside a comment is still a mistake, and the error says where.
    #[test]
    fn a_word_outside_a_comment_is_refused_with_its_line() {
        let error = parse_ids("1\n2 hats\n").unwrap_err();
        assert!(error.to_string().contains("line 2"), "got: {error}");
        assert!(error.to_string().contains("'hats'"), "got: {error}");
    }

    /// A carousel preview video is an HLS manifest, not a video file, and the
    /// analogy with `Video = 62` next door is exactly the trap: guessing `mp4`
    /// would name a 1.1 KB text playlist after the thing it points at.
    #[test]
    fn a_game_preview_video_is_a_playlist_and_not_an_mp4() {
        assert_eq!(ext_for_type_id(86), "m3u8");
        assert_eq!(ext_for_type_id(62), "mp4");
    }

    /// The types nobody has downloaded stay `bin`, including the two that sit
    /// either side of 86 and look like they must be the same thing. `bin` is
    /// the answer that does not claim anything, and an unmeasured arm here
    /// would be a claim.
    #[test]
    fn the_video_types_nobody_measured_are_still_unknown() {
        for id in [81, 85] {
            assert_eq!(ext_for_type_id(id), "bin", "AssetTypeId {id}");
        }
    }

    /// The cloud path is two requests: the delivery API names a `location`,
    /// and the bytes come from there. Returning the first response's body
    /// would save JSON to disk under an `.rbxm` name.
    #[tokio::test]
    async fn the_cloud_source_follows_the_location_it_is_given() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/asset-delivery-api/v1/assetId/42"))
            .and(header("x-api-key", "k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "location": format!("{}/cdn/blob", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cdn/blob"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bytes".to_vec()))
            .mount(&server)
            .await;

        let bytes = download_bytes(&ctx(&server, Source::Cloud, Some("k")), 42, None)
            .await
            .unwrap();
        assert_eq!(bytes, b"bytes");
    }

    /// A version is a different path, not a query parameter. Sent wrong, the
    /// API answers with the current version and the download silently gives
    /// you the wrong one.
    #[tokio::test]
    async fn a_version_goes_into_the_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/asset-delivery-api/v1/assetId/42/version/3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "location": format!("{}/cdn/v3", server.uri())
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cdn/v3"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"v3".to_vec()))
            .mount(&server)
            .await;

        let bytes = download_bytes(&ctx(&server, Source::Cloud, Some("k")), 42, Some(3))
            .await
            .unwrap();
        assert_eq!(bytes, b"v3");
    }

    #[tokio::test]
    async fn a_refused_delivery_lookup_names_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/asset-delivery-api/v1/assetId/42"))
            .respond_with(ResponseTemplate::new(403).set_body_string("denied"))
            .mount(&server)
            .await;

        let error = download_bytes(&ctx(&server, Source::Cloud, Some("k")), 42, None)
            .await
            .unwrap_err();
        let shown = format!("{error:#}");
        assert!(
            shown.contains("asset-delivery lookup failed"),
            "got: {shown}"
        );
        assert!(shown.contains("403"), "got: {shown}");
        assert!(shown.contains("denied"), "got: {shown}");
    }

    /// The public endpoint takes the id as a query parameter, unlike the cloud
    /// one which puts it in the path.
    #[tokio::test]
    async fn the_public_source_asks_by_query_parameter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/asset/"))
            .and(query_param("id", "42"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"public".to_vec()))
            .mount(&server)
            .await;

        let bytes = download_bytes(&ctx(&server, Source::Public, None), 42, None)
            .await
            .unwrap();
        assert_eq!(bytes, b"public");
    }

    #[tokio::test]
    async fn details_come_from_the_economy_host() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/assets/42/details"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Name": "A Hat",
                "AssetTypeId": 8
            })))
            .mount(&server)
            .await;

        let (name, type_id) = fetch_details(&ctx(&server, Source::Public, None), 42)
            .await
            .unwrap();
        assert_eq!(name.as_deref(), Some("A Hat"));
        assert_eq!(type_id, Some(8));
    }
}
