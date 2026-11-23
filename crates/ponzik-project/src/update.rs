//! Self-update without a TLS stack of our own: the release list comes from the
//! GitHub Releases API, fetched by the operating system's own tools (`curl`,
//! `wget`, or PowerShell on Windows). This module holds everything that does not
//! need a window: SHA-256, build versions and their order, the release JSON, the
//! checksum list, the download, and the swap of the running program's file.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ponzik_i18n::{tr, tr_fmt};

use crate::json::{self, Json};

/// `owner/name` of the repository whose releases are followed.
pub const REPO: &str = "nokogoat/ponzik";
/// Page of the releases, for people who download by hand.
pub const RELEASES_PAGE: &str = "https://github.com/nokogoat/ponzik/releases";
/// Tag of the rolling pre-release rebuilt on every push.
pub const NIGHTLY_TAG: &str = "nightly";

/// Release assets, the same names on every release.
pub const ASSET_APPIMAGE: &str = "Ponzik-x86_64.AppImage";
pub const ASSET_WINDOWS_EXE: &str = "Ponzik-windows-x86_64.exe";
pub const ASSET_WINDOWS_ZIP: &str = "Ponzik-windows-x86_64.zip";
pub const ASSET_SUMS: &str = "SHA256SUMS";

/// Marker of the release notes that carries the exact build: `ponzik-build: 0.1.0+1234.abc1234`.
pub const BUILD_MARKER: &str = "ponzik-build:";

// --- SHA-256 ------------------------------------------------------------------

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 fed piece by piece (FIPS 180-4).
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19],
            block: [0; 64],
            filled: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        while !data.is_empty() {
            let n = (64 - self.filled).min(data.len());
            self.block[self.filled..self.filled + n].copy_from_slice(&data[..n]);
            self.filled += n;
            data = &data[n..];
            if self.filled == 64 {
                let block = self.block;
                self.compress(&block);
                self.filled = 0;
            }
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        let mut pad = [0u8; 72];
        pad[0] = 0x80;
        let zeros = (119 - self.filled) % 64;
        pad[1 + zeros..9 + zeros].copy_from_slice(&bits.to_be_bytes());
        let total = self.total;
        self.update(&pad[..9 + zeros]);
        self.total = total;
        debug_assert_eq!(self.filled, 0);
        let mut out = [0u8; 32];
        for (o, s) in out.chunks_exact_mut(4).zip(self.state) {
            o.copy_from_slice(&s.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finish()
}

/// SHA-256 of a file, read in pieces.
pub fn sha256_file(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(h.finish());
        }
        h.update(&buf[..n]);
    }
}

/// Lowercase hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// --- Versions -------------------------------------------------------------------

/// A build of Ponzik: the Cargo version, the number of commits it was built
/// from (it only grows on the main branch) and the short commit hash.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BuildVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// Commit count; 0 when unknown (a build outside git).
    pub build: u64,
    pub hash: String,
}

impl BuildVersion {
    /// Reads `0.1.0`, `v0.1.0`, `0.1.0+1234` or `0.1.0+1234.abc1234`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        let (core, meta) = s.split_once('+').unwrap_or((s, ""));
        // A pre-release label (`-nightly`) does not change the order here.
        let core = core.split_once('-').map_or(core, |(c, _)| c);
        let mut parts = core.split('.');
        let mut num = || parts.next().and_then(|p| p.parse::<u32>().ok());
        let (major, minor, patch) = (num()?, num()?, num()?);
        if parts.next().is_some() {
            return None;
        }
        let (build, hash) = match meta.split_once('.') {
            Some((b, h)) => (b.parse().ok()?, h.to_string()),
            None if meta.is_empty() => (0, String::new()),
            None => (meta.parse().ok()?, String::new()),
        };
        if !hash.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        Some(Self { major, minor, patch, build, hash })
    }

    /// `0.1.0`.
    pub fn semver(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }

    /// `0.1.0+1234.abc1234`, the form [`BuildVersion::parse`] reads back.
    pub fn id(&self) -> String {
        match (self.build, self.hash.is_empty()) {
            (0, true) => self.semver(),
            (b, true) => format!("{}+{b}", self.semver()),
            (b, false) => format!("{}+{b}.{}", self.semver(), self.hash),
        }
    }

    /// This build is more recent than `other`: a higher version, or the same
    /// version built from more commits. A build of unknown count (0) on either
    /// side only compares by version.
    pub fn is_newer_than(&self, other: &BuildVersion) -> bool {
        let v = |b: &BuildVersion| (b.major, b.minor, b.patch);
        match v(self).cmp(&v(other)) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => self.build > 0 && self.build > other.build,
        }
    }
}

// --- Releases --------------------------------------------------------------------

/// Update channel: every push (nightly) or tagged versions only (stable).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Channel {
    #[default]
    Nightly,
    Stable,
}

impl Channel {
    pub fn id(self) -> &'static str {
        match self {
            Channel::Nightly => "nightly",
            Channel::Stable => "stable",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "nightly" => Some(Channel::Nightly),
            "stable" => Some(Channel::Stable),
            _ => None,
        }
    }

    /// API address of the release this channel follows ("latest" skips pre-releases).
    pub fn api_url(self) -> String {
        match self {
            Channel::Nightly => format!("https://api.github.com/repos/{REPO}/releases/tags/{NIGHTLY_TAG}"),
            Channel::Stable => format!("https://api.github.com/repos/{REPO}/releases/latest"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    /// Public download address (works without a token on a public repository).
    pub download_url: String,
    /// API address of the asset (with a token and `Accept: application/octet-stream`).
    pub api_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Release {
    pub tag: String,
    pub name: String,
    pub body: String,
    pub html_url: String,
    pub published_at: String,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

impl Release {
    /// The exact build, from the marker of the notes, else from the tag (`v0.2.0`).
    pub fn version(&self) -> Option<BuildVersion> {
        let marked = self.body.lines().find_map(|l| {
            let at = l.find(BUILD_MARKER)?;
            let rest = &l[at + BUILD_MARKER.len()..];
            BuildVersion::parse(rest.trim().trim_end_matches("-->").trim())
        });
        marked.or_else(|| BuildVersion::parse(&self.tag))
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// The release notes without the build marker, for display.
    pub fn notes(&self) -> String {
        self.body.lines().filter(|l| !l.contains(BUILD_MARKER)).collect::<Vec<_>>().join("\n").trim().to_string()
    }
}

/// Reads one release of the GitHub API (`GET /repos/{owner}/{repo}/releases/…`).
pub fn parse_release(text: &str) -> Result<Release, String> {
    let j = json::parse(text)?;
    let s = |j: &Json, k: &str| j.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let tag = s(&j, "tag_name");
    if tag.is_empty() {
        let msg = s(&j, "message");
        return Err(if msg.is_empty() { tr("not a release").into() } else { msg });
    }
    let assets = j
        .get("assets")
        .and_then(Json::as_arr)
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| {
            let name = s(a, "name");
            let size = a.get("size").and_then(Json::as_f64).filter(|v| v.is_finite() && *v >= 0.0)? as u64;
            (!name.is_empty()).then(|| Asset { name, size, download_url: s(a, "browser_download_url"), api_url: s(a, "url") })
        })
        .collect();
    Ok(Release {
        tag,
        name: s(&j, "name"),
        body: s(&j, "body"),
        html_url: s(&j, "html_url"),
        published_at: s(&j, "published_at"),
        prerelease: j.get("prerelease").and_then(Json::as_bool).unwrap_or(false),
        assets,
    })
}

/// Lines of a `SHA256SUMS` file (`<hash>  <name>`, or `<hash> *<name>`): (hash, name).
pub fn parse_sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (hash, name) = l.trim().split_once(char::is_whitespace)?;
            let name = name.trim_start().trim_start_matches('*').trim();
            let ok = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) && !name.is_empty();
            ok.then(|| (hash.to_ascii_lowercase(), name.to_string()))
        })
        .collect()
}

/// The asset this machine installs, if updates are built for it.
pub fn platform_asset() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some(ASSET_APPIMAGE),
        ("windows", "x86_64") => Some(ASSET_WINDOWS_EXE),
        _ => None,
    }
}

// --- Fetching ---------------------------------------------------------------------

/// Why a fetch failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// Neither curl, wget nor PowerShell could be started.
    NoTool,
    /// No connection (no network, DNS, timeout, TLS): the message of the tool.
    Offline(String),
    /// 404: no such release (not published yet, or a private repository without a token).
    NotFound,
    /// 401 or 403: bad token, or the API's hourly limit.
    Denied(u16),
    Http(u16),
    /// A local problem: temporary file, size or checksum.
    Local(String),
}

impl FetchError {
    pub fn message(&self) -> String {
        match self {
            FetchError::NoTool => tr("curl was not found: install it to get updates").into(),
            FetchError::Offline(_) => tr("No connection: updates will be checked next time").into(),
            FetchError::NotFound => tr("Couldn't reach GitHub: no release found").into(),
            FetchError::Denied(code) => tr_fmt!("GitHub refused the request ({}): check the token, or try again in an hour", *code),
            FetchError::Http(code) => tr_fmt!("GitHub answered with an error ({})", *code),
            FetchError::Local(e) => e.clone(),
        }
    }
}

/// A request: address, `Accept` header and optional token.
pub struct Request<'a> {
    pub url: &'a str,
    pub accept: &'a str,
    pub token: Option<&'a str>,
    /// Whole-transfer limit, in seconds.
    pub timeout: u32,
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW: no console flashing up from a windowed program.
    cmd.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_console(_: &mut Command) {}

/// HTTP status from the output of a tool, or the failure of the tool.
enum Tool {
    Status(u16),
    Missing,
    Failed(String),
}

fn status_result(status: u16) -> Result<(), FetchError> {
    match status {
        200..=299 => Ok(()),
        404 => Err(FetchError::NotFound),
        401 | 403 => Err(FetchError::Denied(status)),
        0 => Err(FetchError::Offline(String::new())),
        s => Err(FetchError::Http(s)),
    }
}

/// Downloads `req.url` into `out` with the first tool available.
pub fn fetch_to_file(req: &Request, out: &Path) -> Result<(), FetchError> {
    // wget only exists on Unix, PowerShell only on Windows: the other answers "missing".
    let tools: [fn(&Request, &Path) -> Tool; 3] = [curl, wget, powershell];
    let mut last = FetchError::NoTool;
    for tool in tools {
        match tool(req, out) {
            Tool::Status(s) => return status_result(s),
            Tool::Missing => continue,
            Tool::Failed(msg) => last = FetchError::Offline(msg),
        }
    }
    let _ = std::fs::remove_file(out);
    Err(last)
}

/// Downloads `req.url` as text.
pub fn fetch_text(req: &Request, scratch: &Path) -> Result<String, FetchError> {
    fetch_to_file(req, scratch)?;
    let text = std::fs::read_to_string(scratch).map_err(|e| FetchError::Local(e.to_string()));
    let _ = std::fs::remove_file(scratch);
    text
}

fn curl(req: &Request, out: &Path) -> Tool {
    let mut cmd = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    cmd.args(["--silent", "--show-error", "--location", "--connect-timeout", "15"])
        .args(["--max-time", &req.timeout.to_string()])
        .args(["--write-out", "%{http_code}", "--output"])
        .arg(out)
        .args(["-H", &format!("Accept: {}", req.accept), "-H", "User-Agent: Ponzik-updater", "-H", "X-GitHub-Api-Version: 2022-11-28"]);
    // The token goes through standard input, never on the command line.
    if req.token.is_some() {
        cmd.args(["-H", "@-"]);
    }
    cmd.arg(req.url);
    run_tool(cmd, req.token.map(|t| format!("Authorization: Bearer {t}\n")), |code, stdout, stderr| {
        let status = stdout.trim().parse::<u16>().unwrap_or(0);
        if code == Some(0) || status != 0 { Tool::Status(status) } else { Tool::Failed(stderr.trim().to_string()) }
    })
}

#[cfg(not(windows))]
fn wget(req: &Request, out: &Path) -> Tool {
    let mut cmd = Command::new("wget");
    cmd.args(["--quiet", "--server-response", "--tries=1", "--connect-timeout=15"])
        .arg(format!("--timeout={}", req.timeout))
        .arg(format!("--header=Accept: {}", req.accept))
        .args(["--header=User-Agent: Ponzik-updater", "--content-on-error", "-O"])
        .arg(out);
    if let Some(t) = req.token {
        cmd.arg(format!("--header=Authorization: Bearer {t}"));
    }
    cmd.arg(req.url);
    run_tool(cmd, None, |code, _, stderr| {
        // The last status line of the server's answers (redirects come first).
        let status = stderr
            .lines()
            .filter_map(|l| l.trim().strip_prefix("HTTP/")?.split_whitespace().nth(1)?.parse::<u16>().ok())
            .next_back();
        match (code, status) {
            (_, Some(s)) => Tool::Status(s),
            (Some(0), None) => Tool::Status(200),
            _ => Tool::Failed(stderr.trim().to_string()),
        }
    })
}

#[cfg(windows)]
fn wget(_: &Request, _: &Path) -> Tool {
    Tool::Missing
}

#[cfg(windows)]
fn powershell(req: &Request, out: &Path) -> Tool {
    // Values pass through the environment: no quoting to get wrong.
    const SCRIPT: &str = "$ProgressPreference='SilentlyContinue';\
        [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12;\
        $h=@{'Accept'=$env:PZ_ACCEPT;'User-Agent'='Ponzik-updater'};\
        if($env:PZ_TOKEN){$h['Authorization']='Bearer '+$env:PZ_TOKEN};\
        try{Invoke-WebRequest -UseBasicParsing -Uri $env:PZ_URL -OutFile $env:PZ_OUT -Headers $h -TimeoutSec ([int]$env:PZ_TIMEOUT);Write-Output 200}\
        catch{$r=$_.Exception.Response;if($r){Write-Output ([int]$r.StatusCode)}else{Write-Error $_;exit 2}}";
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT])
        .env("PZ_URL", req.url)
        .env("PZ_OUT", out)
        .env("PZ_ACCEPT", req.accept)
        .env("PZ_TIMEOUT", req.timeout.to_string())
        .env("PZ_TOKEN", req.token.unwrap_or(""));
    run_tool(cmd, None, |code, stdout, stderr| match stdout.trim().lines().last().and_then(|l| l.trim().parse::<u16>().ok()) {
        Some(s) => Tool::Status(s),
        None if code == Some(0) => Tool::Status(200),
        None => Tool::Failed(stderr.trim().to_string()),
    })
}

#[cfg(not(windows))]
fn powershell(_: &Request, _: &Path) -> Tool {
    Tool::Missing
}

fn run_tool(mut cmd: Command, stdin: Option<String>, read: impl Fn(Option<i32>, &str, &str) -> Tool) -> Tool {
    hide_console(&mut cmd);
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Tool::Missing,
        Err(e) => return Tool::Failed(e.to_string()),
    };
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes());
    }
    match child.wait_with_output() {
        Ok(o) => read(o.status.code(), &String::from_utf8_lossy(&o.stdout), &String::from_utf8_lossy(&o.stderr)),
        Err(e) => Tool::Failed(e.to_string()),
    }
}

/// The release a channel follows.
pub fn fetch_release(channel: Channel, token: Option<&str>, scratch: &Path) -> Result<Release, FetchError> {
    let url = channel.api_url();
    let req = Request { url: &url, accept: "application/vnd.github+json", token, timeout: 30 };
    let text = fetch_text(&req, scratch)?;
    parse_release(&text).map_err(FetchError::Local)
}

/// Request for one asset: the API address with a token (private repositories),
/// the public address without.
fn asset_request<'a>(asset: &'a Asset, token: Option<&'a str>, timeout: u32) -> Request<'a> {
    match token {
        Some(t) if !asset.api_url.is_empty() => Request { url: &asset.api_url, accept: "application/octet-stream", token: Some(t), timeout },
        _ => Request { url: &asset.download_url, accept: "application/octet-stream", token: None, timeout },
    }
}

/// Downloads `name` of the release into `dest`, then checks its size and its
/// SHA-256 against the release's `SHA256SUMS`. The file is removed if it does not match.
pub fn download_verified(release: &Release, name: &str, token: Option<&str>, dest: &Path) -> Result<(), FetchError> {
    let asset = release.asset(name).ok_or_else(|| FetchError::Local(tr_fmt!("{} is missing from the release", name)))?;
    let sums = release.asset(ASSET_SUMS).ok_or_else(|| FetchError::Local(tr("The release has no checksums: update refused").into()))?;
    let sums_tmp = dest.with_extension("sums");
    let sums_text = fetch_text(&asset_request(sums, token, 60), &sums_tmp)?;
    let expected = parse_sums(&sums_text)
        .into_iter()
        .find(|(_, n)| n == name)
        .map(|(h, _)| h)
        .ok_or_else(|| FetchError::Local(tr_fmt!("No checksum for {}: update refused", name)))?;
    fetch_to_file(&asset_request(asset, token, 1800), dest)?;
    verify_file(dest, asset.size, &expected).inspect_err(|_| {
        let _ = std::fs::remove_file(dest);
    })
}

/// The downloaded file has the announced size and checksum.
pub fn verify_file(path: &Path, size: u64, sha256_hex: &str) -> Result<(), FetchError> {
    let local = |e: io::Error| FetchError::Local(e.to_string());
    let len = std::fs::metadata(path).map_err(local)?.len();
    if len != size {
        return Err(FetchError::Local(tr_fmt!("Download incomplete ({} of {} bytes)", len, size)));
    }
    if hex(&sha256_file(path).map_err(local)?) != sha256_hex.to_ascii_lowercase() {
        return Err(FetchError::Local(tr("The download is damaged (checksum mismatch)").into()));
    }
    Ok(())
}

// --- Installing -------------------------------------------------------------------

/// Where a replaced program waits to be deleted: `Ponzik.exe` → `Ponzik.exe.old`.
pub fn old_path(target: &Path) -> PathBuf {
    sibling(target, ".old")
}

fn sibling(target: &Path, suffix: &str) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    target.with_file_name(name)
}

/// Puts `new_file` in place of `target`, which may be the running program.
///
/// The new file is first copied next to the target (same file system, so the
/// last step is a rename), made executable like the old one, then:
/// - `keep_old == false` (Linux, AppImage): renamed over the target in one step;
///   the running program keeps its open copy.
/// - `keep_old == true` (Windows, where a running exe cannot be deleted or
///   overwritten but can be renamed): the target goes to `<name>.old`, the new
///   file takes its name; on failure the old one comes back. `<name>.old` is
///   removed at the next start ([`cleanup_old`]).
pub fn install_file(new_file: &Path, target: &Path, keep_old: bool) -> io::Result<()> {
    let staged = sibling(target, ".new");
    let _ = std::fs::remove_file(&staged);
    let result = (|| {
        std::fs::copy(new_file, &staged)?;
        std::fs::File::open(&staged)?.sync_all()?;
        copy_permissions(target, &staged)?;
        if keep_old {
            let old = old_path(target);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(target, &old)?;
            if let Err(e) = std::fs::rename(&staged, target) {
                let _ = std::fs::rename(&old, target);
                return Err(e);
            }
            Ok(())
        } else {
            std::fs::rename(&staged, target)
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

#[cfg(unix)]
fn copy_permissions(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(from).map(|m| m.permissions().mode()).unwrap_or(0o755) | 0o755;
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn copy_permissions(_: &Path, _: &Path) -> io::Result<()> {
    Ok(())
}

/// Removes what an update left behind: `<name>.old` and a half-staged `<name>.new`.
pub fn cleanup_old(target: &Path) {
    let _ = std::fs::remove_file(old_path(target));
    let _ = std::fs::remove_file(sibling(target, ".new"));
}

// --- Changelog --------------------------------------------------------------------

/// A `## ` section of the changelog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangelogSection {
    /// The whole heading, without `## ` (also the key of "seen" for the what's-new popup).
    pub title: String,
    /// First word of the heading, without brackets or `v`: `0.1.0`.
    pub version: String,
    /// The rest of the heading: a date or a label.
    pub label: String,
    pub body: String,
}

/// The `## ` sections of a Markdown changelog, newest (first) first.
pub fn changelog_sections(md: &str) -> Vec<ChangelogSection> {
    let mut out: Vec<ChangelogSection> = Vec::new();
    for line in md.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            let title = head.trim().to_string();
            let (first, rest) = title.split_once(char::is_whitespace).unwrap_or((&title, ""));
            let version = first.trim_matches(['[', ']']).trim_start_matches('v').to_string();
            let label = rest.trim().trim_start_matches(['-', '—', '·', '–']).trim().to_string();
            out.push(ChangelogSection { title: title.clone(), version, label, body: String::new() });
        } else if let Some(s) = out.last_mut() {
            s.body.push_str(line);
            s.body.push('\n');
        }
    }
    for s in &mut out {
        s.body = s.body.trim().to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> String {
        hex(&sha256(s.as_bytes()))
    }

    #[test]
    fn sha256_test_vectors() {
        assert_eq!(h(""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(h("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            h("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            h("abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
        let million = vec![b'a'; 1_000_000];
        assert_eq!(hex(&sha256(&million)), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // Every padding boundary, fed whole or byte by byte.
        for len in 50..130 {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let mut piecewise = Sha256::new();
            for b in &data {
                piecewise.update(std::slice::from_ref(b));
            }
            assert_eq!(piecewise.finish(), sha256(&data), "length {len}");
        }
    }

    #[test]
    fn sha256_of_a_file() {
        let dir = temp_dir("sha");
        let path = dir.join("f.bin");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(hex(&sha256_file(&path).unwrap()), h("abc"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn versions_parse_and_print() {
        let v = BuildVersion::parse("0.1.0+1234.abc1234").unwrap();
        assert_eq!((v.major, v.minor, v.patch, v.build, v.hash.as_str()), (0, 1, 0, 1234, "abc1234"));
        assert_eq!(v.id(), "0.1.0+1234.abc1234");
        assert_eq!(BuildVersion::parse("v1.2.3").unwrap().id(), "1.2.3");
        assert_eq!(BuildVersion::parse("1.2.3+7").unwrap().build, 7);
        assert_eq!(BuildVersion::parse(" 0.2.0-nightly+9.ff ").unwrap().id(), "0.2.0+9.ff");
        for bad in ["", "nightly", "1.2", "1.2.3.4", "1.x.3", "1.2.3+x", "1.2.3+4.a b"] {
            assert_eq!(BuildVersion::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn version_order() {
        let v = |s: &str| BuildVersion::parse(s).unwrap();
        assert!(v("0.1.0+101.b").is_newer_than(&v("0.1.0+100.a")));
        assert!(!v("0.1.0+100.a").is_newer_than(&v("0.1.0+100.a")));
        assert!(!v("0.1.0+99.a").is_newer_than(&v("0.1.0+100.a")));
        assert!(v("0.2.0").is_newer_than(&v("0.1.9+500.a")));
        assert!(v("1.0.0").is_newer_than(&v("0.99.99")));
        assert!(!v("0.1.0+500").is_newer_than(&v("0.2.0")));
        // A stable tag without a build number does not beat the same version.
        assert!(!v("0.1.0").is_newer_than(&v("0.1.0+100.a")));
        // A build outside git (count 0) takes any counted build of its version.
        assert!(v("0.1.0+3.c").is_newer_than(&v("0.1.0")));
    }

    const RELEASE: &str = r#"{
      "url": "https://api.github.com/repos/nokogoat/ponzik/releases/1",
      "html_url": "https://github.com/nokogoat/ponzik/releases/tag/nightly",
      "id": 1, "tag_name": "nightly", "target_commitish": "main",
      "name": "Ponzik nightly", "draft": false, "prerelease": true,
      "published_at": "2026-11-23T12:00:00Z",
      "author": {"login": "ci", "id": 2, "site_admin": false},
      "assets": [
        {"url": "https://api.github.com/repos/nokogoat/ponzik/releases/assets/10", "id": 10,
         "name": "Ponzik-x86_64.AppImage", "label": null, "content_type": "application/octet-stream",
         "state": "uploaded", "size": 12345678, "download_count": 3,
         "browser_download_url": "https://github.com/nokogoat/ponzik/releases/download/nightly/Ponzik-x86_64.AppImage"},
        {"url": "https://api.github.com/repos/nokogoat/ponzik/releases/assets/11", "id": 11,
         "name": "SHA256SUMS", "size": 300,
         "browser_download_url": "https://github.com/nokogoat/ponzik/releases/download/nightly/SHA256SUMS"},
        {"name": "broken", "size": -1}
      ],
      "body": "Changes since the previous build:\r\n- Mixer: \"wider\" strips é\r\n\r\n<!-- ponzik-build: 0.1.0+1234.abc1234 -->"
    }"#;

    #[test]
    fn release_json() {
        let r = parse_release(RELEASE).unwrap();
        assert_eq!(r.tag, "nightly");
        assert!(r.prerelease);
        assert_eq!(r.assets.len(), 2);
        let a = r.asset(ASSET_APPIMAGE).unwrap();
        assert_eq!(a.size, 12_345_678);
        assert!(a.download_url.ends_with("/nightly/Ponzik-x86_64.AppImage"));
        assert!(a.api_url.ends_with("/assets/10"));
        assert_eq!(r.version().unwrap().id(), "0.1.0+1234.abc1234");
        assert!(r.notes().contains("\"wider\" strips é"));
        assert!(!r.notes().contains(BUILD_MARKER));
        // A tagged release without a marker takes its version from the tag.
        let tagged = parse_release(r#"{"tag_name": "v0.3.1", "assets": []}"#).unwrap();
        assert_eq!(tagged.version().unwrap().id(), "0.3.1");
        // The API's answer for a missing (or private) release.
        let err = parse_release(r#"{"message": "Not Found", "documentation_url": "x"}"#).unwrap_err();
        assert_eq!(err, "Not Found");
        assert!(parse_release("<html>").is_err());
    }

    #[test]
    fn checksum_lists() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let text = format!("{a}  Ponzik-x86_64.AppImage\n{b} *Ponzik-windows-x86_64.exe\nnot a line\n\n123  short\n");
        let sums = parse_sums(&text);
        assert_eq!(sums, vec![(a, ASSET_APPIMAGE.to_string()), ("b".repeat(64), ASSET_WINDOWS_EXE.to_string())]);
    }

    #[test]
    fn verify_checks_size_and_hash() {
        let dir = temp_dir("verify");
        let f = dir.join("dl");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(verify_file(&f, 3, &h("abc").to_uppercase()), Ok(()));
        assert!(matches!(verify_file(&f, 4, &h("abc")), Err(FetchError::Local(_))));
        assert!(matches!(verify_file(&f, 3, &h("abd")), Err(FetchError::Local(_))));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ponzik-update-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn swap_replaces_in_one_step() {
        let dir = temp_dir("swap");
        let (target, new) = (dir.join("Ponzik.AppImage"), dir.join("download"));
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&new, b"new build").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        // An open handle on the old file (the running program) keeps reading it.
        let mut running = std::fs::File::open(&target).unwrap();
        install_file(&new, &target, false).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new build");
        let mut kept = String::new();
        running.read_to_string(&mut kept).unwrap();
        assert_eq!(kept, "old");
        assert!(!old_path(&target).exists());
        assert!(!dir.join("Ponzik.AppImage.new").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn swap_keeps_the_old_file_aside_then_cleans_up() {
        let dir = temp_dir("swap-old");
        let (exe, new) = (dir.join("Ponzik.exe"), dir.join("download"));
        std::fs::write(&exe, b"v1").unwrap();
        std::fs::write(old_path(&exe), b"stale").unwrap();
        std::fs::write(&new, b"v2").unwrap();
        install_file(&new, &exe, true).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"v2");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"v1");
        assert_eq!(old_path(&exe).file_name().unwrap(), "Ponzik.exe.old");
        cleanup_old(&exe);
        assert!(!old_path(&exe).exists());
        assert_eq!(std::fs::read(&exe).unwrap(), b"v2");
        // A missing download leaves everything as it was.
        assert!(install_file(&dir.join("nothing"), &exe, true).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"v2");
        assert!(!dir.join("Ponzik.exe.new").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn changelog_sections_newest_first() {
        let md = "# Changelog\n\nIntro.\n\n## [0.1.1] - 2026-12-01\n\n### Mixer\n- Faster\n\n## 0.1.0 — 2026-11-23\n- First\n";
        let s = changelog_sections(md);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].version.as_str(), s[0].label.as_str()), ("0.1.1", "2026-12-01"));
        assert_eq!(s[0].body, "### Mixer\n- Faster");
        assert_eq!(s[0].title, "[0.1.1] - 2026-12-01");
        assert_eq!((s[1].version.as_str(), s[1].label.as_str(), s[1].body.as_str()), ("0.1.0", "2026-11-23", "- First"));
    }

    #[test]
    fn channels() {
        assert_eq!(Channel::from_id(Channel::Stable.id()), Some(Channel::Stable));
        assert!(Channel::Nightly.api_url().ends_with("/releases/tags/nightly"));
        assert!(Channel::Stable.api_url().ends_with("/releases/latest"));
    }
}
