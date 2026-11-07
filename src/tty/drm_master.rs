//! Best-effort DRM master handover for the KMS path.
//!
//! Slint owns the DRM fd and never drops master, which blocks fbcon and other
//! compositors. Before acknowledging a VT release we drop master on every DRM
//! primary node open in this process (`/proc/self/fd`); when the VT is ours
//! again we set it on exactly those fds, re-checking device and inode first so
//! a reused fd number is skipped.

use std::os::fd::RawFd;

use rustix::fs::fstat;

use super::clients::is_drm_primary;
use super::{borrow, warn};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct DrmFd {
    fd: RawFd,
    rdev: u64,
    ino: u64,
}

#[derive(Default)]
pub(super) struct DrmMaster {
    /// Fds we dropped master on and have to restore.
    dropped: Vec<DrmFd>,
    warned: bool,
}

impl DrmMaster {
    /// Drop master on every DRM primary node fd that holds it (none on
    /// fbdev, where no DRM fd is open).
    pub(super) fn drop_master(&mut self) {
        for node in primary_nodes() {
            if self.dropped.contains(&node) {
                continue;
            }
            // EINVAL just means this fd is not the master.
            if drm_ffi::auth::release_master(borrow(node.fd)).is_ok() {
                self.dropped.push(node);
            }
        }
    }

    /// Set master again on the fds released by [`Self::drop_master`].
    /// Returns false while some fd could not get master back (e.g. another
    /// process still holds it); the caller retries later.
    pub(super) fn set_master(&mut self) -> bool {
        let mut failure = None;
        self.dropped.retain(|node| {
            if primary_node(node.fd).as_ref() != Some(node) {
                // Closed (and maybe reused) since: nothing to restore.
                return false;
            }
            match drm_ffi::auth::acquire_master(borrow(node.fd)) {
                Ok(()) => false,
                Err(error) => {
                    failure = Some(error);
                    true
                }
            }
        });
        match failure {
            None => {
                self.warned = false;
                true
            }
            Some(error) => {
                if !std::mem::replace(&mut self.warned, true) {
                    warn(format_args!(
                        "cannot become DRM master again ({error}); not drawing until it succeeds"
                    ));
                }
                false
            }
        }
    }
}

fn primary_nodes() -> Vec<DrmFd> {
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<RawFd>().ok())
        .filter_map(primary_node)
        .collect()
}

fn primary_node(fd: RawFd) -> Option<DrmFd> {
    let stat = fstat(borrow(fd)).ok()?;
    is_drm_primary(&stat).then_some(DrmFd {
        fd,
        rdev: stat.st_rdev,
        ino: stat.st_ino,
    })
}
