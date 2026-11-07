//! Finds display clients started from our shell.
//!
//! A compositor started from the terminal holds a DRM primary node open (the
//! seat manager passes the fd on), whoever owns the VT mode, so this also works
//! with seatd. Only descendants of this process count: a compositor on another
//! VT must not suspend us.

use std::collections::HashMap;
use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use procfs::process::{FDTarget, Process, all_processes};
use rustix::fs::{FileType, inotify, major, minor, stat};

/// Whether `stat` is a DRM primary node (`/dev/dri/cardN`: major 226, minors
/// 0..64; render nodes start at 128).
pub(super) fn is_drm_primary(stat: &rustix::fs::Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::CharacterDevice
        && major(stat.st_rdev) == 226
        && minor(stat.st_rdev) < 64
}

/// Without inotify: scan this often.
const FALLBACK_INTERVAL: Duration = Duration::from_millis(250);
/// After an open/close event: scan on every call for this long. The opener
/// (seatd/logind) opens the node a moment before the fd reaches the
/// compositor, so the first scan can be too early.
const BURST: Duration = Duration::from_millis(500);
/// While a client is known: rescan this often in case an event was missed.
const SAFETY_INTERVAL: Duration = Duration::from_secs(2);

/// Cached answer to "does a descendant hold a DRM primary node". An inotify
/// watch on the DRM nodes says when to look, so an idle display is not polled.
pub(super) struct DisplayClients {
    watch: Option<Watch>,
    last_scan: Option<Instant>,
    burst_until: Option<Instant>,
    found: bool,
}

impl Default for DisplayClients {
    fn default() -> Self {
        let nodes = drm_primary_nodes();
        let watch = Watch::new(&nodes);
        match &watch {
            Some(_) => log::info!("watching {} DRM node(s) for display clients", nodes.len()),
            None => log::warn!(
                "cannot watch the DRM nodes ({} found); scanning every {:?}",
                nodes.len(),
                FALLBACK_INTERVAL
            ),
        }
        Self {
            watch,
            last_scan: None,
            burst_until: None,
            found: false,
        }
    }
}

impl DisplayClients {
    pub(super) fn any(&mut self) -> bool {
        let now = Instant::now();
        let due = match &self.watch {
            Some(watch) => {
                if watch.drain() {
                    self.burst_until = Some(now + BURST);
                }
                self.last_scan.is_none()
                    || self.burst_until.is_some_and(|until| now < until)
                    || (self.found && since(self.last_scan, now) >= SAFETY_INTERVAL)
            }
            None => since(self.last_scan, now) >= FALLBACK_INTERVAL,
        };
        if due {
            self.last_scan = Some(now);
            let found = scan();
            if found != self.found {
                log::info!("display client {}", if found { "appeared" } else { "gone" });
            }
            self.found = found;
        }
        self.found
    }
}

fn since(last: Option<Instant>, now: Instant) -> Duration {
    last.map_or(Duration::MAX, |last| now.duration_since(last))
}

fn drm_primary_nodes() -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir("/dev/dri") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().as_bytes().starts_with(b"card"))
        .map(|entry| entry.path())
        .collect()
}

/// A non-blocking inotify instance watching the DRM nodes.
struct Watch {
    fd: OwnedFd,
}

impl Watch {
    /// None when inotify is unavailable or nothing could be watched.
    fn new(nodes: &[PathBuf]) -> Option<Self> {
        let fd =
            inotify::init(inotify::CreateFlags::NONBLOCK | inotify::CreateFlags::CLOEXEC).ok()?;
        let mask = inotify::WatchFlags::OPEN
            | inotify::WatchFlags::CLOSE_WRITE
            | inotify::WatchFlags::CLOSE_NOWRITE;
        let watched = nodes
            .iter()
            .filter(|node| inotify::add_watch(&fd, node.as_path(), mask).is_ok())
            .count();
        (watched > 0).then_some(Self { fd })
    }

    /// Read and discard pending events; true if there were any.
    fn drain(&self) -> bool {
        let mut buffer = [0u8; 4096];
        let mut any = false;
        while matches!(rustix::io::read(&self.fd, &mut buffer), Ok(read) if read > 0) {
            any = true;
        }
        any
    }
}

fn scan() -> bool {
    let Ok(processes) = all_processes() else {
        return false;
    };
    let parents: HashMap<u32, u32> = processes
        .flatten()
        .filter_map(|process| Some((process.pid as u32, process.stat().ok()?.ppid as u32)))
        .collect();
    let me = std::process::id();
    parents
        .keys()
        .copied()
        .filter(|&pid| is_descendant(pid, me, &parents))
        .any(holds_drm_primary)
}

fn is_descendant(mut pid: u32, ancestor: u32, parents: &HashMap<u32, u32>) -> bool {
    // Bounded: a corrupt table must not loop forever.
    for _ in 0..4096 {
        match parents.get(&pid) {
            Some(&parent) if parent == ancestor => return true,
            Some(&parent) if parent > 1 && parent != pid => pid = parent,
            _ => return false,
        }
    }
    false
}

fn holds_drm_primary(pid: u32) -> bool {
    let Ok(fds) = Process::new(pid as i32).and_then(|process| process.fd()) else {
        return false;
    };
    fds.flatten().any(|fd| {
        // `stat` follows the link; a vanished or non-device target just fails.
        let FDTarget::Path(path) = fd.target else {
            return false;
        };
        stat(&path).is_ok_and(|stat| is_drm_primary(&stat))
    })
}
