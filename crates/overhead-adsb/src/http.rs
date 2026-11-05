//! Minimal blocking HTTP helpers (ranges, retries) on top of ureq.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(60))
            .user_agent("overhead/0.1 (https://github.com/mlodygbelmondo/overhead)")
            .build()
    })
}

/// Total size of a resource from a one-byte range request, or `None` on 404.
pub fn probe_size(url: &str) -> Result<Option<u64>> {
    for attempt in 0..4 {
        match agent().get(url).set("Range", "bytes=0-0").call() {
            Ok(r) => {
                let size = r
                    .header("Content-Range")
                    .and_then(|v| v.rsplit('/').next())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .or_else(|| r.header("Content-Length").and_then(|v| v.parse().ok()));
                return Ok(size);
            }
            Err(ureq::Error::Status(404, _)) => return Ok(None),
            Err(e) if attempt < 3 => {
                eprintln!("  probe {url}: {e}; retrying");
                std::thread::sleep(Duration::from_secs(2 << attempt));
            }
            Err(e) => return Err(e).with_context(|| format!("probing {url}")),
        }
    }
    unreachable!()
}

/// Open a byte range `[start, end)` (`end = None`: to the end) as a stream.
/// `file://` URLs are read from disk (tests, local mirrors).
pub fn open_range(url: &str, start: u64, end: Option<u64>) -> Result<Box<dyn Read + Send + Sync>> {
    if let Some(path) = url.strip_prefix("file://") {
        use std::io::{Seek, SeekFrom};
        let mut f = std::fs::File::open(path).with_context(|| format!("open {path}"))?;
        f.seek(SeekFrom::Start(start))?;
        return Ok(match end {
            Some(e) => Box::new(f.take(e.saturating_sub(start))),
            None => Box::new(f),
        });
    }
    let range = match end {
        Some(e) => format!("bytes={start}-{}", e.saturating_sub(1)),
        None => format!("bytes={start}-"),
    };
    for attempt in 0..5 {
        match agent().get(url).set("Range", &range).call() {
            Ok(r) => {
                if r.status() != 206 && start > 0 {
                    bail!("server ignored range request for {url} (status {})", r.status());
                }
                return Ok(Box::new(r.into_reader()));
            }
            Err(e) if attempt < 4 => {
                eprintln!("  GET {url} [{range}]: {e}; retrying");
                std::thread::sleep(Duration::from_secs(2 << attempt));
            }
            Err(e) => return Err(e).with_context(|| format!("GET {url} [{range}]")),
        }
    }
    unreachable!()
}

/// Read a whole byte range into memory.
pub fn get_range(url: &str, start: u64, end: u64) -> Result<Vec<u8>> {
    let mut last = None;
    for attempt in 0..5 {
        let res = open_range(url, start, Some(end)).and_then(|mut r| {
            let mut buf = Vec::with_capacity((end - start) as usize);
            r.read_to_end(&mut buf)?;
            Ok(buf)
        });
        match res {
            Ok(b) => return Ok(b),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(Duration::from_secs(1 << attempt));
            }
        }
    }
    Err(last.unwrap())
}

/// GET a small resource fully; `None` on 404.
pub fn get_bytes(url: &str) -> Result<Option<Vec<u8>>> {
    for attempt in 0..4 {
        match agent().get(url).call() {
            Ok(r) => {
                let mut buf = Vec::new();
                r.into_reader().take(2 << 30).read_to_end(&mut buf)?;
                return Ok(Some(buf));
            }
            Err(ureq::Error::Status(404, _)) | Err(ureq::Error::Status(403, _)) => return Ok(None),
            Err(e) if attempt < 3 => {
                eprintln!("  GET {url}: {e}; retrying");
                std::thread::sleep(Duration::from_secs(2 << attempt));
            }
            Err(e) => return Err(e).with_context(|| format!("GET {url}")),
        }
    }
    unreachable!()
}
