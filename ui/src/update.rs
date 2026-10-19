//! Check GitHub tagged releases and install the matching package.
//!
//! `GET /repos/Lyamc/etherquic/releases` lists tags. Stable builds skip
//! `prerelease`. Beta also considers those. The asset name encodes the OS:
//! `*-windows-x64.msi`, `*-macos-arm64.pkg` / `.dmg`, `*-x86_64-linux.tar.gz`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

pub const REPO: &str = "Lyamc/etherquic";
const HOST_API: &str = "api.github.com";
const API_PATH: &str = "/repos/Lyamc/etherquic/releases?per_page=40";

/// What the Settings row shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    Current {
        latest: String,
    },
    Available(Offer),
    NoPackage {
        tag: String,
        page: String,
    },
    Downloading {
        tag: String,
    },
    Installed {
        tag: String,
        detail: String,
    },
    Failed(String),
}

/// A newer GitHub release with an installer for this computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub tag: String,
    pub version: Version,
    pub prerelease: bool,
    pub asset_name: String,
    pub asset_url: String,
    pub page: String,
}

/// `major.minor.patch` plus an optional fourth component (`0.1.5.3`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version([u16; 4]);

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('v');
        if text.is_empty() {
            return None;
        }
        let mut parts = [0u16; 4];
        let mut n = 0;
        for (i, piece) in text.split('.').enumerate() {
            if i >= 4 {
                return None;
            }
            parts[i] = piece.parse().ok()?;
            n += 1;
        }
        if n == 0 {
            return None;
        }
        Some(Self(parts))
    }

    pub fn display(self) -> String {
        if self.0[3] == 0 {
            format!("{}.{}.{}", self.0[0], self.0[1], self.0[2])
        } else {
            format!("{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Macos,
    Linux,
    Android,
    Other,
}

pub fn this_platform() -> Platform {
    if cfg!(target_os = "android") {
        Platform::Android
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::Macos
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else {
        Platform::Other
    }
}

pub fn current_version() -> Version {
    Version::parse(etherquic::RELEASE).unwrap_or_default()
}

/// Look up GitHub tags and pick a newer installer, if one exists.
pub fn check(include_prerelease: bool) -> Result<Status, String> {
    check_with(&fetch_releases()?, include_prerelease, current_version(), this_platform())
}

pub fn check_with(
    releases: &[Release],
    include_prerelease: bool,
    current: Version,
    platform: Platform,
) -> Result<Status, String> {
    let Some(release) = latest(releases, include_prerelease) else {
        return Ok(Status::Current {
            latest: current.display(),
        });
    };
    let found = Version::parse(&release.tag).ok_or_else(|| {
        format!("GitHub tag {} is not a version", release.tag)
    })?;
    if found <= current {
        return Ok(Status::Current {
            latest: release.tag.clone(),
        });
    }
    let page = format!("https://github.com/{REPO}/releases/tag/{}", release.tag);
    let Some(asset) = pick_asset(release, platform) else {
        return Ok(Status::NoPackage {
            tag: release.tag.clone(),
            page,
        });
    };
    Ok(Status::Available(Offer {
        tag: release.tag.clone(),
        version: found,
        prerelease: release.prerelease,
        asset_name: asset.name.clone(),
        asset_url: asset.url.clone(),
        page,
    }))
}

pub fn latest(releases: &[Release], include_prerelease: bool) -> Option<&Release> {
    releases.iter().filter(|rel| include_prerelease || !rel.prerelease).max_by(|a, b| {
        match (Version::parse(&a.tag), Version::parse(&b.tag)) {
            (Some(va), Some(vb)) => va.cmp(&vb),
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (None, None) => a.tag.cmp(&b.tag),
        }
    })
}

pub fn pick_asset(release: &Release, platform: Platform) -> Option<&Asset> {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let mac_arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    match platform {
        Platform::Windows => release.assets.iter().find(|a| {
            a.name.ends_with(".msi") && a.name.contains("windows")
        }),
        Platform::Macos => release
            .assets
            .iter()
            .find(|a| a.name.ends_with(".pkg") && a.name.contains("macos") && a.name.contains(mac_arch))
            .or_else(|| {
                release.assets.iter().find(|a| {
                    a.name.ends_with(".dmg") && a.name.contains("macos") && a.name.contains(mac_arch)
                })
            }),
        Platform::Linux => release.assets.iter().find(|a| {
            a.name.ends_with(".tar.gz") && a.name.contains("linux") && a.name.contains(arch)
        }),
        Platform::Android => release.assets.iter().find(|a| a.name.ends_with(".apk")),
        Platform::Other => None,
    }
}

/// Download `offer` and run the installer for this OS.
pub fn install(offer: &Offer) -> Result<String, String> {
    let dir = std::env::temp_dir().join("etherquic-update");
    std::fs::create_dir_all(&dir).map_err(|err| format!("create {}: {err}", dir.display()))?;
    let dest = dir.join(&offer.asset_name);
    download(&offer.asset_url, &dest)?;
    apply(&dest, offer)
}

fn apply(path: &Path, offer: &Offer) -> Result<String, String> {
    match this_platform() {
        Platform::Windows => {
            spawn_waitless("msiexec", &["/i", &path.display().to_string()])?;
            Ok(format!(
                "The {} installer is open. Finish it, then start etherquic again.",
                offer.tag
            ))
        }
        Platform::Macos => {
            spawn_waitless("open", &[path.to_str().ok_or("installer path is not UTF-8")?])?;
            Ok(format!(
                "macOS is opening {}. Finish the installer, then start etherquic again.",
                offer.asset_name
            ))
        }
        Platform::Linux => {
            let dest = linux_install_dir(&offer.tag)?;
            std::fs::create_dir_all(&dest)
                .map_err(|err| format!("create {}: {err}", dest.display()))?;
            let status = std::process::Command::new("tar")
                .arg("-xzf")
                .arg(path)
                .arg("-C")
                .arg(&dest)
                .status()
                .map_err(|err| format!("tar: {err}"))?;
            if !status.success() {
                return Err(format!("tar exited {status}"));
            }
            Ok(format!(
                "Unpacked {} into {}. Restart etherquic to use it.",
                offer.tag,
                dest.display()
            ))
        }
        Platform::Android | Platform::Other => Err(format!(
            "This computer has no in-app installer. Open {}",
            offer.page
        )),
    }
}

fn linux_install_dir(tag: &str) -> Result<PathBuf, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            if dir_writable(parent) {
                return Ok(parent.to_path_buf());
            }
        }
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let dest = PathBuf::from(home)
        .join("Downloads")
        .join(format!("etherquic-{tag}"));
    Ok(dest)
}

fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".etherquic-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn spawn_waitless(program: &str, args: &[&str]) -> Result<(), String> {
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("{program}: {err}"))
}

pub fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        return spawn_waitless("cmd", &["/C", "start", "", url]);
    }
    #[cfg(target_os = "macos")]
    {
        return spawn_waitless("open", &[url]);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        return spawn_waitless("xdg-open", &[url]);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux", target_os = "android")))]
    {
        let _ = url;
        Err("no URL opener on this OS".into())
    }
}

fn fetch_releases() -> Result<Vec<Release>, String> {
    let body = https_get(HOST_API, API_PATH)?;
    let text = String::from_utf8(body).map_err(|err| format!("GitHub body is not UTF-8: {err}"))?;
    parse_releases(&text)
}

pub fn parse_releases(json: &str) -> Result<Vec<Release>, String> {
    let mut p = Json::new(json);
    p.skip_ws();
    p.expect(b'[')?;
    let mut out = Vec::new();
    loop {
        p.skip_ws();
        if p.peek() == Some(b']') {
            p.i += 1;
            break;
        }
        if !out.is_empty() {
            p.expect(b',')?;
            p.skip_ws();
        }
        out.push(parse_release(&mut p)?);
    }
    drop(p);
    Ok(out)
}

fn parse_release(p: &mut Json<'_>) -> Result<Release, String> {
    let mut tag = String::new();
    let mut prerelease = false;
    let mut assets = Vec::new();
    p.expect(b'{')?;
    loop {
        p.skip_ws();
        if p.peek() == Some(b'}') {
            p.i += 1;
            break;
        }
        if p.peek() == Some(b',') {
            p.i += 1;
            p.skip_ws();
            if p.peek() == Some(b'}') {
                p.i += 1;
                break;
            }
        }
        let key = p.string()?;
        p.skip_ws();
        p.expect(b':')?;
        p.skip_ws();
        match key.as_str() {
            "tag_name" => tag = p.string()?,
            "prerelease" => prerelease = p.bool()?,
            "assets" => assets = parse_assets(p)?,
            _ => p.skip_value()?,
        }
    }
    if tag.is_empty() {
        return Err("release is missing tag_name".into());
    }
    Ok(Release {
        tag,
        prerelease,
        assets,
    })
}

fn parse_assets(p: &mut Json<'_>) -> Result<Vec<Asset>, String> {
    p.expect(b'[')?;
    let mut out = Vec::new();
    loop {
        p.skip_ws();
        if p.peek() == Some(b']') {
            p.i += 1;
            break;
        }
        if !out.is_empty() {
            p.expect(b',')?;
            p.skip_ws();
        }
        out.push(parse_asset(p)?);
    }
    Ok(out)
}

fn parse_asset(p: &mut Json<'_>) -> Result<Asset, String> {
    p.expect(b'{')?;
    let mut name = String::new();
    let mut url = String::new();
    loop {
        p.skip_ws();
        if p.peek() == Some(b'}') {
            p.i += 1;
            break;
        }
        if p.peek() == Some(b',') {
            p.i += 1;
            p.skip_ws();
            if p.peek() == Some(b'}') {
                p.i += 1;
                break;
            }
        }
        let key = p.string()?;
        p.skip_ws();
        p.expect(b':')?;
        p.skip_ws();
        match key.as_str() {
            "name" => name = p.string()?,
            "browser_download_url" => url = p.string()?,
            _ => p.skip_value()?,
        }
    }
    Ok(Asset { name, url })
}

struct Json<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Json<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            s: text.as_bytes(),
            i: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.i += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        self.skip_ws();
        if self.peek() != Some(byte) {
            return Err(format!(
                "JSON expected {} at {}",
                byte as char, self.i
            ));
        }
        self.i += 1;
        Ok(())
    }

    fn bool(&mut self) -> Result<bool, String> {
        if self.s[self.i..].starts_with(b"true") {
            self.i += 4;
            return Ok(true);
        }
        if self.s[self.i..].starts_with(b"false") {
            self.i += 5;
            return Ok(false);
        }
        Err(format!("JSON bool at {}", self.i))
    }

    fn string(&mut self) -> Result<String, String> {
        self.skip_ws();
        self.expect(b'"')?;
        let mut out = String::new();
        while let Some(b) = self.peek() {
            self.i += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = self.peek().ok_or("unterminated JSON string")?;
                    self.i += 1;
                    match esc {
                        b'"' | b'\\' | b'/' => out.push(esc as char),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = self.take(4)?;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| "bad JSON unicode escape".to_string())?;
                            out.push(char::from_u32(code).ok_or("bad JSON unicode scalar")?);
                        }
                        _ => return Err("bad JSON escape".into()),
                    }
                }
                _ => out.push(b as char),
            }
        }
        Err("unterminated JSON string".into())
    }

    fn take(&mut self, n: usize) -> Result<&'a str, String> {
        let end = self.i.checked_add(n).ok_or("JSON overflow")?;
        let slice = self.s.get(self.i..end).ok_or("JSON overflow")?;
        self.i = end;
        std::str::from_utf8(slice).map_err(|_| "JSON unicode escape is not UTF-8".into())
    }

    fn skip_value(&mut self) -> Result<(), String> {
        self.skip_ws();
        match self.peek() {
            Some(b'"') => {
                self.string()?;
            }
            Some(b't') | Some(b'f') => {
                self.bool()?;
            }
            Some(b'n') => {
                if !self.s[self.i..].starts_with(b"null") {
                    return Err("JSON null".into());
                }
                self.i += 4;
            }
            Some(b'[') => {
                self.i += 1;
                loop {
                    self.skip_ws();
                    if self.peek() == Some(b']') {
                        self.i += 1;
                        break;
                    }
                    self.skip_value()?;
                    self.skip_ws();
                    if self.peek() == Some(b',') {
                        self.i += 1;
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                loop {
                    self.skip_ws();
                    if self.peek() == Some(b'}') {
                        self.i += 1;
                        break;
                    }
                    if self.peek() == Some(b',') {
                        self.i += 1;
                        continue;
                    }
                    self.string()?;
                    self.skip_ws();
                    self.expect(b':')?;
                    self.skip_value()?;
                }
            }
            Some(b'-') | Some(b'0'..=b'9') => {
                self.i += 1;
                while matches!(self.peek(), Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')) {
                    self.i += 1;
                }
            }
            _ => return Err(format!("JSON value at {}", self.i)),
        }
        Ok(())
    }
}

fn https_get(host: &str, path: &str) -> Result<Vec<u8>, String> {
    https_request("GET", host, path, None)
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    let (host, path) = split_url(url)?;
    let body = https_request("GET", &host, &path, None)?;
    std::fs::write(dest, body).map_err(|err| format!("write {}: {err}", dest.display()))
}

fn https_request(
    method: &str,
    host: &str,
    path: &str,
    redirects: Option<u8>,
) -> Result<Vec<u8>, String> {
    let left = redirects.unwrap_or(8);
    let mut stream = tls_connect(host)?;
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: etherquic/{}\r\nAccept: application/vnd.github+json\r\nConnection: close\r\n\r\n",
        etherquic::RELEASE
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|err| format!("write {host}: {err}"))?;
    stream.flush().map_err(|err| format!("flush {host}: {err}"))?;
    let (status, headers, body) = read_http(&mut stream)?;
    if (300..400).contains(&status) {
        if left == 0 {
            return Err("too many HTTP redirects".into());
        }
        let loc = header(&headers, "location").ok_or("redirect with no Location")?;
        let (next_host, next_path) = if loc.starts_with("https://") {
            split_url(loc)?
        } else if loc.starts_with('/') {
            (host.to_string(), loc.to_string())
        } else {
            return Err(format!("unsupported redirect {loc}"));
        };
        return https_request(method, &next_host, &next_path, Some(left - 1));
    }
    if !(200..300).contains(&status) {
        let preview = String::from_utf8_lossy(&body);
        return Err(format!(
            "HTTP {status} from {host}: {}",
            preview.chars().take(180).collect::<String>()
        ));
    }
    Ok(body)
}

fn tls_connect(host: &str) -> Result<StreamOwned<ClientConnection, TcpStream>, String> {
    etherquic::install_pure_crypto();
    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    if native.certs.is_empty() {
        return Err(format!(
            "no TLS roots from the OS ({:?})",
            native.errors
        ));
    }
    for cert in native.certs {
        roots
            .add(cert)
            .map_err(|err| format!("trust store: {err}"))?;
    }
    let config = ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
        .with_safe_default_protocol_versions()
        .map_err(|err| err.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string()).map_err(|err| err.to_string())?;
    let conn = ClientConnection::new(Arc::new(config), name).map_err(|err| err.to_string())?;
    let tcp = TcpStream::connect((host, 443)).map_err(|err| format!("connect {host}: {err}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(45)))
        .map_err(|err| err.to_string())?;
    tcp.set_write_timeout(Some(Duration::from_secs(45)))
        .map_err(|err| err.to_string())?;
    Ok(StreamOwned::new(conn, tcp))
}

fn split_url(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("not https: {url}"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    Ok((host.to_string(), path))
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn read_http(stream: &mut impl Read) -> Result<(u16, Vec<(String, String)>, Vec<u8>), String> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).map_err(|err| format!("read: {err}"))?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(at) = find_headers_end(&raw) {
            let (head, rest) = raw.split_at(at);
            let (status, headers) = parse_head(head)?;
            let mut body = rest.to_vec();
            if header(&headers, "transfer-encoding")
                .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
            {
                let more = read_to_end(stream)?;
                body.extend_from_slice(&more);
                return Ok((status, headers, decode_chunked(&body)?));
            }
            if let Some(len) = header(&headers, "content-length").and_then(|v| v.parse::<usize>().ok())
            {
                while body.len() < len {
                    let n = stream.read(&mut buf).map_err(|err| format!("read: {err}"))?;
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&buf[..n]);
                }
                body.truncate(len);
                return Ok((status, headers, body));
            }
            body.extend_from_slice(&read_to_end(stream)?);
            return Ok((status, headers, body));
        }
        if raw.len() > 1024 * 1024 {
            return Err("HTTP headers too large".into());
        }
    }
    Err("HTTP response ended before headers".into())
}

fn read_to_end(stream: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    stream
        .read_to_end(&mut out)
        .map_err(|err| format!("read: {err}"))?;
    Ok(out)
}

fn find_headers_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn parse_head(head: &[u8]) -> Result<(u16, Vec<(String, String)>), String> {
    let text = std::str::from_utf8(head).map_err(|_| "HTTP headers are not UTF-8")?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().ok_or("empty HTTP response")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line {status_line}"))?;
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    Ok((status, headers))
}

fn decode_chunked(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut i = 0;
    let mut out = Vec::new();
    while i < body.len() {
        let rest = &body[i..];
        let line_end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("truncated chunk size")?;
        let size_line = std::str::from_utf8(&rest[..line_end]).map_err(|_| "chunk size")?;
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| "chunk size")?;
        i += line_end + 2;
        if size == 0 {
            break;
        }
        if i + size > body.len() {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&body[i..i + size]);
        i += size;
        if body.get(i..i + 2) == Some(&b"\r\n"[..]) {
            i += 2;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_four_components() {
        assert!(Version::parse("0.1.10").unwrap() > Version::parse("v0.1.9").unwrap());
        assert!(Version::parse("0.1.5.3").unwrap() > Version::parse("0.1.5").unwrap());
        assert_eq!(Version::parse("v0.1.9").unwrap().display(), "0.1.9");
        assert_eq!(Version::parse("0.1.5.3").unwrap().display(), "0.1.5.3");
    }

    #[test]
    fn parse_github_payload_and_skip_prerelease() {
        let json = r#"[{"tag_name":"v0.1.10","prerelease":true,"assets":[{"name":"etherquic-0.1.10-x86_64-linux.tar.gz","browser_download_url":"https://example/linux"}]},{"tag_name":"v0.1.9","prerelease":false,"html_url":"x","assets":[{"name":"notes.txt","browser_download_url":"https://example/notes"},{"name":"etherquic-0.1.9-x86_64-linux.tar.gz","browser_download_url":"https://example/linux9"},{"name":"etherquic-0.1.9-windows-x64.msi","browser_download_url":"https://example/msi"}]}]"#;
        let releases = parse_releases(json).unwrap();
        assert_eq!(releases.len(), 2);
        assert!(releases[0].prerelease);
        let stable = latest(&releases, false).unwrap();
        assert_eq!(stable.tag, "v0.1.9");
        let beta = latest(&releases, true).unwrap();
        assert_eq!(beta.tag, "v0.1.10");
        let linux = pick_asset(stable, Platform::Linux).unwrap();
        assert!(linux.name.contains("linux"));
        let win = pick_asset(stable, Platform::Windows).unwrap();
        assert!(win.name.ends_with(".msi"));
        let current = Version::parse("0.1.8").unwrap();
        match check_with(&releases, false, current, Platform::Linux).unwrap() {
            Status::Available(offer) => {
                assert_eq!(offer.tag, "v0.1.9");
                assert!(!offer.prerelease);
            }
            other => panic!("{other:?}"),
        }
        match check_with(&releases, false, Version::parse("0.1.9").unwrap(), Platform::Linux)
            .unwrap()
        {
            Status::Current { latest } => assert_eq!(latest, "v0.1.9"),
            other => panic!("{other:?}"),
        }
        match check_with(&releases, true, Version::parse("0.1.9").unwrap(), Platform::Linux)
            .unwrap()
        {
            Status::Available(offer) => assert_eq!(offer.tag, "v0.1.10"),
            other => panic!("{other:?}"),
        }
        match check_with(&releases, false, current, Platform::Android).unwrap() {
            Status::NoPackage { tag, .. } => assert_eq!(tag, "v0.1.9"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn chunked_body_round_trips() {
        let raw = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(raw).unwrap(), b"hello world");
    }

    #[test]
    #[ignore]
    fn github_lists_tagged_releases() {
        let releases = fetch_releases().expect("GitHub releases");
        assert!(!releases.is_empty(), "GitHub returned no releases");
        assert!(Version::parse(&releases[0].tag).is_some());
    }

    #[test]
    fn split_https_url() {
        let (host, path) = split_url("https://github.com/Lyamc/etherquic/releases/download/v0.1.9/a.tar.gz")
            .unwrap();
        assert_eq!(host, "github.com");
        assert_eq!(
            path,
            "/Lyamc/etherquic/releases/download/v0.1.9/a.tar.gz"
        );
    }
}
