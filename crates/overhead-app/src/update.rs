//! Self-update of the Windows build from the repository's GitHub releases.
//!
//! CI (`.github/workflows/windows.yml`) builds every push to main with
//! `OVERHEAD_BUILD` (the run number) and `OVERHEAD_COMMIT` baked in and
//! publishes the exe as release `build-<N>` of the public repository. The
//! app reads the releases from the GitHub API without any token or account.
//!
//! On start (CI builds on Windows) a background check lists the latest
//! releases; a newer `build-*` exe is downloaded next to the running one as
//! `overhead.new.exe` and the status bar offers a restart. Restart renames
//! the running exe to `overhead.old.exe` (Windows allows renaming a running
//! exe), moves the new one into place and starts it with the same arguments;
//! the next start deletes the old one. Any failure keeps (or puts back) the
//! current exe. A failed check (no network, GitHub's rate limit for
//! unauthenticated requests) only shows a short note and "Retry".
//!
//! Dev builds (no `OVERHEAD_BUILD`) never check. On other systems the check
//! runs only with `OVERHEAD_UPDATE_CHECK=1` and only reports a newer build.

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use gpui::{AnyElement, ClickEvent, Context, Task, Window, div, prelude::*, px};
use serde::Deserialize;

use crate::theme::{self, MONO};

pub const REPO: &str = "mlodygbelmondo/overhead";
const API_HOST: &str = "api.github.com";
const EXE_ASSET: &str = "overhead.exe";
const NEW_EXE: &str = "overhead.new.exe";
const OLD_EXE: &str = "overhead.old.exe";
const NEW_BUILD: &str = "overhead.new.build";
const MAX_EXE: u64 = 1 << 30;

/// CI build number baked in at compile time; `None` in dev builds.
pub fn build() -> Option<u32> {
    option_env!("OVERHEAD_BUILD").and_then(|s| s.trim().parse().ok())
}

/// Short commit of the CI build.
pub fn commit() -> Option<&'static str> {
    option_env!("OVERHEAD_COMMIT").map(str::trim).filter(|s| !s.is_empty())
}

pub fn build_label() -> String {
    match (build(), commit()) {
        (Some(b), Some(c)) => format!("build {b} ({c})"),
        (Some(b), None) => format!("build {b}"),
        (None, _) => "dev build".into(),
    }
}

/// Whether this build can replace its own exe.
const fn can_install() -> bool {
    cfg!(windows)
}

fn checks_enabled() -> bool {
    build().is_some() && (can_install() || std::env::var_os("OVERHEAD_UPDATE_CHECK").is_some_and(|v| v == "1"))
}

// ---------------------------------------------------------------- releases

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    id: u64,
    name: String,
    size: u64,
    #[serde(default)]
    state: Option<String>,
}

/// The newest published `build-*` release with an exe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Latest {
    pub build: u32,
    pub asset_id: u64,
    pub size: u64,
}

pub fn tag_build(tag: &str) -> Option<u32> {
    let n = tag.strip_prefix("build-")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// The highest build in a `GET /repos/{repo}/releases` response; other
/// releases (e.g. `preview-1`), drafts and releases without a fully uploaded
/// `overhead.exe` are ignored.
pub fn parse_latest(json: &[u8]) -> anyhow::Result<Option<Latest>> {
    let rels: Vec<ApiRelease> = serde_json::from_slice(json)?;
    Ok(rels
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let build = tag_build(&r.tag_name)?;
            let a = r
                .assets
                .iter()
                .find(|a| a.name == EXE_ASSET && a.state.as_deref().is_none_or(|s| s == "uploaded") && a.size > 0)?;
            Some(Latest { build, asset_id: a.id, size: a.size })
        })
        .max_by_key(|l| l.build))
}

pub fn is_newer(current: u32, latest: &Latest) -> bool {
    latest.build > current
}

// ---------------------------------------------------------------- http

/// Why a check or download failed, as shown in the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fail(pub String);

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<io::Error> for Fail {
    fn from(e: io::Error) -> Self {
        Fail(e.to_string())
    }
}

/// The status-bar text for an HTTP error status.
fn status_fail(code: u16, host: &str, api: bool) -> Fail {
    Fail(match code {
        // Unauthenticated API requests are limited per address (60 an hour).
        403 | 429 if api => "GitHub's rate limit, try again later".into(),
        404 if api => "no releases found on GitHub".into(),
        c => format!("HTTP {c} from {host}"),
    })
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(60))
            // Redirects are followed by hand so the API headers only go to the API host.
            .redirects(0)
            .user_agent("overhead-updater (https://github.com/mlodygbelmondo/overhead)")
            .build()
    })
}

/// Host of an absolute http(s) URL (no user info, no port).
pub fn url_host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let auth = rest.split(['/', '?', '#']).next()?;
    let host = auth.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then_some(host)
}

/// Whether a URL is the GitHub API over https (a redirect target, e.g. the
/// signed asset download, gets no API headers).
pub fn is_api(url: &str) -> bool {
    url.starts_with("https://") && url_host(url).is_some_and(|h| h.eq_ignore_ascii_case(API_HOST))
}

/// Absolute URL of a `Location` header.
pub fn resolve_location(base: &str, loc: &str) -> Option<String> {
    if loc.starts_with("https://") || loc.starts_with("http://") {
        return Some(loc.to_string());
    }
    if loc.starts_with('/') && !loc.starts_with("//") {
        let scheme = if base.starts_with("https://") { "https" } else { "http" };
        let rest = base.split_once("://")?.1;
        let auth = rest.split(['/', '?', '#']).next()?;
        return Some(format!("{scheme}://{auth}{loc}"));
    }
    None
}

fn get(url: &str, accept: &str) -> Result<ureq::Response, Fail> {
    let mut url = url.to_string();
    for _ in 0..6 {
        let mut req = agent().get(&url).set("Accept", accept);
        if is_api(&url) {
            req = req.set("X-GitHub-Api-Version", "2022-11-28");
        }
        let host = url_host(&url).unwrap_or("?").to_string();
        match req.call() {
            Ok(r) if (300..400).contains(&r.status()) => {
                let loc = r.header("Location").unwrap_or_default();
                url = resolve_location(&url, loc)
                    .ok_or_else(|| Fail(format!("bad redirect from {host} (HTTP {})", r.status())))?;
            }
            Ok(r) => return Ok(r),
            // Never echo the URL: a redirect target carries a signed query.
            Err(ureq::Error::Status(c, _)) => return Err(status_fail(c, &host, is_api(&url))),
            Err(ureq::Error::Transport(t)) => return Err(Fail(format!("network error ({}) reaching {host}", t.kind()))),
        }
    }
    Err(Fail("too many redirects".into()))
}

/// The newest `build-*` release.
pub fn fetch_latest() -> Result<Option<Latest>, Fail> {
    let url = format!("https://{API_HOST}/repos/{REPO}/releases?per_page=10");
    let r = get(&url, "application/vnd.github+json")?;
    let mut buf = Vec::new();
    r.into_reader().take(8 << 20).read_to_end(&mut buf)?;
    parse_latest(&buf).map_err(|e| Fail(format!("unexpected answer from GitHub: {e}")))
}

/// Download the release exe to `p.new` (via a `.part` file), checking its
/// size and that it is a Windows executable.
pub fn download(latest: &Latest, p: &Paths) -> Result<(), Fail> {
    let url = format!("https://{API_HOST}/repos/{REPO}/releases/assets/{}", latest.asset_id);
    let r = get(&url, "application/octet-stream")?;
    let part = p.new.with_extension("exe.part");
    let res = (|| -> Result<(), Fail> {
        let mut f = fs::File::create(&part)?;
        let n = io::copy(&mut r.into_reader().take(MAX_EXE), &mut f)?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        if n != latest.size {
            return Err(Fail(format!("download incomplete ({n} of {} bytes)", latest.size)));
        }
        install_download(&part, p, latest.build)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&part);
    }
    res
}

/// Move a finished download into place as `p.new` and record its build.
pub fn install_download(part: &Path, p: &Paths, build: u32) -> Result<(), Fail> {
    let mut head = [0u8; 2];
    fs::File::open(part)?.read_exact(&mut head)?;
    if &head != b"MZ" {
        return Err(Fail("downloaded file is not a Windows program".into()));
    }
    let _ = fs::remove_file(&p.marker);
    fs::rename(part, &p.new)?;
    fs::write(&p.marker, build.to_string())?;
    Ok(())
}

/// The build already downloaded as `p.new`, if its size matches `latest`.
pub fn downloaded(p: &Paths, latest: &Latest) -> bool {
    let build = fs::read_to_string(&p.marker).ok().and_then(|s| s.trim().parse::<u32>().ok());
    build == Some(latest.build) && fs::metadata(&p.new).is_ok_and(|m| m.len() == latest.size)
}

// ---------------------------------------------------------------- files

/// The running exe and its siblings used by the update.
#[derive(Clone, Debug)]
pub struct Paths {
    pub exe: PathBuf,
    pub new: PathBuf,
    pub old: PathBuf,
    pub marker: PathBuf,
}

impl Paths {
    pub fn beside(exe: &Path) -> Self {
        let dir = exe.parent().unwrap_or(Path::new("."));
        Self { exe: exe.to_path_buf(), new: dir.join(NEW_EXE), old: dir.join(OLD_EXE), marker: dir.join(NEW_BUILD) }
    }

    pub fn current() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        Some(Self::beside(&exe))
    }
}

/// Put `p.new` in place of `p.exe`, keeping the running one as `p.old`.
/// On failure the original exe is back at `p.exe`.
pub fn swap(p: &Paths) -> io::Result<()> {
    swap_with(p, |a, b| fs::rename(a, b))
}

fn swap_with(p: &Paths, mv: impl Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    if !p.new.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "the downloaded update is missing"));
    }
    if p.old.exists() {
        // Left over from the previous update; if it cannot go, that
        // version is still running and we must not touch anything.
        fs::remove_file(&p.old)?;
    }
    mv(&p.exe, &p.old)?;
    if let Err(e) = mv(&p.new, &p.exe) {
        if let Err(back) = mv(&p.old, &p.exe) {
            log::error!("update: could not restore {}: {back}", p.exe.display());
        }
        return Err(e);
    }
    let _ = fs::remove_file(&p.marker);
    Ok(())
}

/// Undo `swap` (the new exe did not start): the running exe goes back.
pub fn unswap(p: &Paths) -> io::Result<()> {
    unswap_with(p, |a, b| fs::rename(a, b))
}

fn unswap_with(p: &Paths, mv: impl Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    mv(&p.exe, &p.new)?;
    if let Err(e) = mv(&p.old, &p.exe) {
        // Keep at least the new build at the exe path.
        let _ = mv(&p.new, &p.exe);
        return Err(e);
    }
    Ok(())
}

/// Swap in the downloaded exe and start it with this process's arguments.
/// The caller quits on success.
pub fn restart(p: &Paths) -> Result<(), String> {
    swap(p).map_err(|e| format!("could not replace the program: {e}"))?;
    match std::process::Command::new(&p.exe).args(std::env::args_os().skip(1)).spawn() {
        Ok(_) => Ok(()),
        Err(e) => {
            let undo = unswap(p).err().map(|u| format!("; restoring failed: {u}")).unwrap_or_default();
            Err(format!("could not start the new version: {e}{undo}"))
        }
    }
}

/// Delete the exe left by the previous update (it may still be exiting, so
/// a few tries).
pub fn cleanup_old(p: &Paths, tries: u32, wait: Duration) -> bool {
    for i in 0..tries {
        if !p.old.exists() || fs::remove_file(&p.old).is_ok() {
            return true;
        }
        if i + 1 < tries {
            std::thread::sleep(wait);
        }
    }
    false
}

/// At start-up, once the window is open (Windows): drop a partial download
/// at once, and `overhead.old.exe` only after this version has been
/// running for a while, so a new build that dies at start leaves the
/// previous exe behind to go back to.
fn startup_cleanup() {
    if !can_install() {
        return;
    }
    if let Some(p) = Paths::current() {
        let _ = fs::remove_file(p.new.with_extension("exe.part"));
        if p.old.exists() {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(20));
                if !cleanup_old(&p, 40, Duration::from_millis(500)) {
                    log::warn!("update: could not delete {}", p.old.display());
                }
            });
        }
    }
}

// ---------------------------------------------------------------- settings

/// The settings folder (`%APPDATA%\Overhead`, `~/.config/overhead`). The
/// first time, the folder of the old name (`%APPDATA%\Doppler`,
/// `~/.config/doppler`) is renamed to it if only that one exists (or, if the
/// rename fails, used as it is).
pub fn config_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let (base, new, old) = if cfg!(windows) {
            (std::env::var_os("APPDATA").map(PathBuf::from)?, "Overhead", "Doppler")
        } else {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
            (base, "overhead", "doppler")
        };
        Some(pick_config_dir(base.join(new), base.join(old), |a, b| fs::rename(a, b)))
    })
    .clone()
}

/// `new` unless only `old` exists; then `old` renamed to `new`, or `old`
/// itself if that fails.
fn pick_config_dir(new: PathBuf, old: PathBuf, mv: impl Fn(&Path, &Path) -> io::Result<()>) -> PathBuf {
    if new.exists() || !old.is_dir() {
        return new;
    }
    match mv(&old, &new) {
        Ok(()) => new,
        Err(e) => {
            log::warn!("settings: could not rename {} to {}: {e}", old.display(), new.display());
            old
        }
    }
}

/// Files of the token-based updater of earlier builds: no longer used.
fn remove_old_token_files() {
    if let Some(d) = config_dir() {
        for f in ["github-token", "updates-skipped"] {
            let _ = fs::remove_file(d.join(f));
        }
    }
}

// ---------------------------------------------------------------- UI

#[derive(Clone, Debug, PartialEq)]
enum State {
    /// Dev build or no updates on this system.
    Off,
    Idle,
    Checking,
    UpToDate,
    /// A newer build exists but this system cannot install it.
    Available(u32),
    Downloading(u32),
    Ready(u32),
    /// Check or download failed (the current exe is untouched).
    Failed(String),
    /// Restart failed (the current exe was kept or put back).
    RestartFailed(String),
}

/// Status-bar segment: build number, update state, "Check for updates".
pub struct UpdateUi {
    state: State,
    task: Option<Task<()>>,
}

impl UpdateUi {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self { state: State::Off, task: None };
        if checks_enabled() {
            startup_cleanup();
            remove_old_token_files();
            this.state = State::Idle;
            this.check(cx);
        }
        this
    }

    fn check(&mut self, cx: &mut Context<Self>) {
        if self.state == State::Off || matches!(self.state, State::Checking | State::Downloading(_)) {
            return;
        }
        let current = build().unwrap_or(0);
        self.state = State::Checking;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { fetch_latest() }).await;
            let latest = match res {
                Ok(Some(l)) if is_newer(current, &l) => l,
                Ok(_) => {
                    this.update(cx, |this, cx| {
                        this.state = State::UpToDate;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Err(e) => {
                    this.update(cx, |this, cx| this.fail(e, cx)).ok();
                    return;
                }
            };
            let paths = Paths::current();
            let go = this
                .update(cx, |this, cx| {
                    this.state = if !can_install() { State::Available(latest.build) } else { State::Downloading(latest.build) };
                    cx.notify();
                    can_install() && paths.is_some()
                })
                .unwrap_or(false);
            let Some(paths) = paths.filter(|_| go) else { return };
            let b = latest.build;
            let res = cx
                .background_executor()
                .spawn(async move { if downloaded(&paths, &latest) { Ok(()) } else { download(&latest, &paths) } })
                .await;
            this.update(cx, |this, cx| match res {
                Ok(()) => {
                    log::info!("update: build {b} downloaded");
                    this.state = State::Ready(b);
                    cx.notify();
                }
                Err(e) => this.fail(e, cx),
            })
            .ok();
        }));
    }

    fn fail(&mut self, e: Fail, cx: &mut Context<Self>) {
        log::info!("update: {e}");
        self.state = State::Failed(e.to_string());
        cx.notify();
    }

    fn do_restart(&mut self, cx: &mut Context<Self>) {
        let Some(p) = Paths::current() else { return };
        match restart(&p) {
            Ok(()) => cx.quit(),
            Err(e) => {
                log::error!("update: {e}");
                self.state = State::RestartFailed(e);
                cx.notify();
            }
        }
    }
}

impl Render for UpdateUi {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme::theme(cx);
        let hover = t.hover;
        let link = |id: &'static str, label: String, color| {
            div()
                .id(id)
                .px(px(4.))
                .rounded(px(theme::RADIUS_SM))
                .cursor_pointer()
                .text_color(color)
                .hover(move |s| s.bg(hover))
                .child(label)
        };
        let check = |label: &str| {
            link("upd-check", label.to_string(), t.text_muted)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.check(cx)))
        };
        let state: Option<AnyElement> = match &self.state {
            State::Off => None,
            State::Idle => Some(check("Check for updates").into_any_element()),
            State::UpToDate => Some(
                div()
                    .flex()
                    .items_center()
                    .child(div().text_color(t.text_faint).child("up to date"))
                    .child(check("Check for updates"))
                    .into_any_element(),
            ),
            State::Checking => Some(div().text_color(t.text_faint).child("checking for updates…").into_any_element()),
            State::Available(b) => Some(div().text_color(t.text_muted).child(format!("build {b} available")).into_any_element()),
            State::Downloading(b) => {
                Some(div().text_color(t.text_faint).child(format!("downloading build {b}…")).into_any_element())
            }
            State::Ready(b) => Some(
                div()
                    .id("upd-restart")
                    .px(px(6.))
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .rounded(px(theme::RADIUS_SM))
                    .bg(t.accent)
                    .text_color(t.accent_text)
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.88))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.do_restart(cx)))
                    .child(format!("Update ready (build {b}) — Restart"))
                    .into_any_element(),
            ),
            State::Failed(e) | State::RestartFailed(e) => Some(
                div()
                    .flex()
                    .items_center()
                    .min_w(px(0.))
                    .child(
                        div()
                            .max_w(px(320.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(if matches!(self.state, State::RestartFailed(_)) { t.danger } else { t.warn })
                            .child(format!("Update failed: {e}")),
                    )
                    .child(check("Retry"))
                    .into_any_element(),
            ),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .text_size(px(theme::TEXT_XS))
            .child(div().font_family(MONO).text_color(t.text_faint).child(build_label()))
            .children(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed `GET /repos/mlodygbelmondo/overhead/releases` answer
    /// in GitHub's REST format (fields the app ignores removed).
    const SAMPLE: &str = r#"[
  {
    "url": "https://api.github.com/repos/mlodygbelmondo/overhead/releases/190000003",
    "id": 190000003,
    "tag_name": "build-14",
    "target_commitish": "main",
    "name": "Build 14",
    "draft": true,
    "prerelease": true,
    "created_at": "2026-11-03T19:10:00Z",
    "published_at": null,
    "assets": [
      {"id": 7003, "name": "overhead.exe", "content_type": "application/x-msdownload", "state": "uploaded", "size": 31000000}
    ]
  },
  {
    "url": "https://api.github.com/repos/mlodygbelmondo/overhead/releases/190000002",
    "id": 190000002,
    "tag_name": "build-13",
    "target_commitish": "main",
    "name": "Build 13",
    "draft": false,
    "prerelease": true,
    "created_at": "2026-11-03T18:55:12Z",
    "published_at": "2026-11-03T18:55:40Z",
    "assets": [
      {
        "url": "https://api.github.com/repos/mlodygbelmondo/overhead/releases/assets/7002",
        "id": 7002,
        "node_id": "RA_kwDOxxxx",
        "name": "latest.json",
        "label": "",
        "content_type": "application/json",
        "state": "uploaded",
        "size": 64,
        "download_count": 0,
        "browser_download_url": "https://github.com/mlodygbelmondo/overhead/releases/download/build-13/latest.json"
      },
      {
        "url": "https://api.github.com/repos/mlodygbelmondo/overhead/releases/assets/7001",
        "id": 7001,
        "node_id": "RA_kwDOyyyy",
        "name": "overhead.exe",
        "label": "",
        "uploader": {"login": "github-actions[bot]", "id": 41898282, "type": "Bot"},
        "content_type": "application/x-msdownload",
        "state": "uploaded",
        "size": 30871552,
        "download_count": 3,
        "created_at": "2026-11-03T18:55:20Z",
        "updated_at": "2026-11-03T18:55:38Z",
        "browser_download_url": "https://github.com/mlodygbelmondo/overhead/releases/download/build-13/overhead.exe"
      }
    ],
    "body": "Windows build 13 of main"
  },
  {
    "tag_name": "build-12",
    "draft": false,
    "prerelease": true,
    "assets": [{"id": 6001, "name": "overhead.exe", "state": "uploaded", "size": 30870000}]
  },
  {
    "tag_name": "build-9",
    "draft": false,
    "assets": [{"id": 5001, "name": "overhead.exe", "state": "uploaded", "size": 30000000}]
  },
  {
    "tag_name": "preview-1",
    "draft": false,
    "prerelease": true,
    "assets": [{"id": 4001, "name": "Overhead-Windows.zip", "state": "uploaded", "size": 216883451}]
  }
]"#;

    #[test]
    fn tags() {
        assert_eq!(tag_build("build-13"), Some(13));
        assert_eq!(tag_build("build-0"), Some(0));
        assert_eq!(tag_build("build-"), None);
        assert_eq!(tag_build("build-+3"), None);
        assert_eq!(tag_build("build-1a"), None);
        assert_eq!(tag_build("preview-1"), None);
        assert_eq!(tag_build("win-1"), None);
    }

    #[test]
    fn parses_newest_published_build() {
        let l = parse_latest(SAMPLE.as_bytes()).unwrap().unwrap();
        assert_eq!(l, Latest { build: 13, asset_id: 7001, size: 30871552 });
    }

    #[test]
    fn numeric_not_lexical_order() {
        let json = r#"[{"tag_name":"build-9","assets":[{"id":1,"name":"overhead.exe","size":5}]},
                       {"tag_name":"build-10","assets":[{"id":2,"name":"overhead.exe","size":5}]}]"#;
        assert_eq!(parse_latest(json.as_bytes()).unwrap().unwrap().build, 10);
    }

    #[test]
    fn ignores_releases_without_a_ready_exe() {
        let json = r#"[{"tag_name":"build-5","assets":[{"id":1,"name":"overhead.exe","state":"starter","size":5}]},
                       {"tag_name":"build-4","assets":[{"id":2,"name":"latest.json","size":5}]},
                       {"tag_name":"preview-1","assets":[{"id":3,"name":"overhead.exe","size":5}]}]"#;
        assert_eq!(parse_latest(json.as_bytes()).unwrap(), None);
        assert_eq!(parse_latest(b"[]").unwrap(), None);
        assert!(parse_latest(br#"{"message":"Bad credentials"}"#).is_err());
    }

    #[test]
    fn version_comparison() {
        let l = Latest { build: 13, asset_id: 1, size: 1 };
        assert!(is_newer(12, &l));
        assert!(is_newer(9, &l));
        assert!(!is_newer(13, &l));
        assert!(!is_newer(14, &l));
    }

    #[test]
    fn api_headers_go_to_the_api_host_only() {
        assert!(is_api("https://api.github.com/repos/x/y/releases/assets/1"));
        assert!(is_api("https://API.github.com/repos"));
        assert!(!is_api("http://api.github.com/repos"));
        assert!(!is_api("https://objects.githubusercontent.com/github-production-release-asset/1?X-Amz-Signature=1"));
        assert!(!is_api("https://release-assets.githubusercontent.com/github-production-release-asset/1"));
        assert!(!is_api("https://api.github.com.evil.example/x"));
        assert!(!is_api("https://api.github.com@evil.example/x"));
        assert_eq!(url_host("https://user@host.example:443/p?q"), Some("host.example"));
    }

    #[test]
    fn redirect_locations() {
        let base = "https://api.github.com/repos/x/y/releases/assets/1";
        assert_eq!(resolve_location(base, "https://cdn.example/a?b=c").as_deref(), Some("https://cdn.example/a?b=c"));
        assert_eq!(resolve_location(base, "/repositories/5/releases/assets/1").as_deref(), Some("https://api.github.com/repositories/5/releases/assets/1"));
        assert_eq!(resolve_location(base, "//evil.example/x"), None);
        assert_eq!(resolve_location(base, ""), None);
    }

    #[test]
    fn http_errors() {
        assert_eq!(status_fail(403, API_HOST, true).0, "GitHub's rate limit, try again later");
        assert_eq!(status_fail(429, API_HOST, true).0, "GitHub's rate limit, try again later");
        assert_eq!(status_fail(404, API_HOST, true).0, "no releases found on GitHub");
        assert_eq!(status_fail(403, "cdn.example", false).0, "HTTP 403 from cdn.example");
        assert_eq!(status_fail(500, API_HOST, true).0, "HTTP 500 from api.github.com");
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("overhead-update-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn swap_and_unswap() {
        let d = tmp("swap");
        let p = Paths::beside(&d.join("overhead.exe"));
        fs::write(&p.exe, "old version").unwrap();
        fs::write(&p.new, "MZ new version").unwrap();
        fs::write(&p.marker, "13").unwrap();
        swap(&p).unwrap();
        assert_eq!(fs::read_to_string(&p.exe).unwrap(), "MZ new version");
        assert_eq!(fs::read_to_string(&p.old).unwrap(), "old version");
        assert!(!p.new.exists() && !p.marker.exists());

        // the new exe would not start: the old one comes back
        unswap(&p).unwrap();
        assert_eq!(fs::read_to_string(&p.exe).unwrap(), "old version");
        assert_eq!(fs::read_to_string(&p.new).unwrap(), "MZ new version");
        assert!(!p.old.exists());

        // a leftover .old from an earlier update is replaced
        fs::write(&p.old, "ancient").unwrap();
        swap(&p).unwrap();
        assert_eq!(fs::read_to_string(&p.old).unwrap(), "old version");
        assert!(cleanup_old(&p, 1, Duration::ZERO));
        assert!(!p.old.exists());
        assert_eq!(fs::read_to_string(&p.exe).unwrap(), "MZ new version");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn swap_without_download_keeps_the_exe() {
        let d = tmp("missing");
        let p = Paths::beside(&d.join("overhead.exe"));
        fs::write(&p.exe, "current").unwrap();
        assert!(swap(&p).is_err());
        assert_eq!(fs::read_to_string(&p.exe).unwrap(), "current");
        assert!(!p.old.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn swap_failing_midway_restores_the_exe() {
        let d = tmp("midway");
        let p = Paths::beside(&d.join("overhead.exe"));
        fs::write(&p.exe, "current").unwrap();
        fs::write(&p.new, "MZ new").unwrap();
        // moving the new exe into place fails (e.g. blocked by a virus scanner)
        let r = swap_with(&p, |a, b| {
            if a == p.new { Err(io::Error::other("blocked")) } else { fs::rename(a, b) }
        });
        assert!(r.is_err());
        assert_eq!(fs::read_to_string(&p.exe).unwrap(), "current");
        assert_eq!(fs::read_to_string(&p.new).unwrap(), "MZ new");
        assert!(!p.old.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unswap_failing_midway_keeps_an_exe() {
        let d = tmp("unswap");
        let p = Paths::beside(&d.join("overhead.exe"));
        fs::write(&p.exe, "MZ new").unwrap();
        fs::write(&p.old, "old").unwrap();
        let r = unswap_with(&p, |a, b| {
            if a == p.old { Err(io::Error::other("blocked")) } else { fs::rename(a, b) }
        });
        assert!(r.is_err());
        assert_eq!(fs::read_to_string(&p.exe).unwrap(), "MZ new");
        assert_eq!(fs::read_to_string(&p.old).unwrap(), "old");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn download_install_checks_the_file() {
        let d = tmp("install");
        let p = Paths::beside(&d.join("overhead.exe"));
        let part = d.join("x.part");
        fs::write(&part, "<html>not an exe</html>").unwrap();
        assert!(install_download(&part, &p, 13).is_err());
        assert!(!p.new.exists());
        fs::write(&part, b"MZ\x90\0rest").unwrap();
        install_download(&part, &p, 13).unwrap();
        assert!(p.new.exists() && !part.exists());
        let l = Latest { build: 13, asset_id: 1, size: 8 };
        assert!(downloaded(&p, &l));
        assert!(!downloaded(&p, &Latest { size: 9, ..l.clone() }));
        assert!(!downloaded(&p, &Latest { build: 14, ..l }));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn settings_folder_of_the_old_name() {
        let d = tmp("config");
        let (new, old) = (d.join("overhead"), d.join("doppler"));
        let rename = |a: &Path, b: &Path| fs::rename(a, b);
        // neither: the new one (created by whoever writes to it)
        assert_eq!(pick_config_dir(new.clone(), old.clone(), rename), new);
        assert!(!new.exists());
        // only the old one: renamed, with its files
        fs::create_dir(&old).unwrap();
        fs::write(old.join("layout.json"), "{}").unwrap();
        assert_eq!(pick_config_dir(new.clone(), old.clone(), rename), new);
        assert!(new.join("layout.json").is_file() && !old.exists());
        // both: the new one, the old one untouched
        fs::create_dir(&old).unwrap();
        assert_eq!(pick_config_dir(new.clone(), old.clone(), rename), new);
        assert!(old.is_dir());
        // only the old one and the rename fails: read the old one
        fs::remove_dir_all(&new).unwrap();
        assert_eq!(pick_config_dir(new.clone(), old.clone(), |_, _| Err(io::Error::other("in use"))), old);
        assert!(!new.exists());
        let _ = fs::remove_dir_all(&d);
    }

    /// Live check against GitHub (network, no token): lists the releases and,
    /// with `OVERHEAD_TEST_ASSET` (an asset id) and `OVERHEAD_TEST_ASSET_SIZE`,
    /// downloads that asset through the redirect, comparing its size.
    #[test]
    #[ignore]
    fn live_github() {
        let latest = fetch_latest().expect("list releases");
        eprintln!("latest build release: {latest:?}");
        if let (Ok(id), Ok(size)) = (std::env::var("OVERHEAD_TEST_ASSET"), std::env::var("OVERHEAD_TEST_ASSET_SIZE")) {
            let url = format!("https://{API_HOST}/repos/{REPO}/releases/assets/{id}");
            let r = get(&url, "application/octet-stream").expect("asset");
            let n = io::copy(&mut r.into_reader(), &mut io::sink()).unwrap();
            assert_eq!(n, size.parse::<u64>().unwrap());
        }
    }
}
