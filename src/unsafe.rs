//! Every `unsafe` block in slintcon lives here; the rest of the crate is
//! `#![deny(unsafe_code)]`. The file is `unsafe.rs`, declared as `mod sys`
//! because `unsafe` is a keyword.

#![allow(unsafe_code)]

use std::os::fd::{BorrowedFd, RawFd};

use libc::{c_int, c_void};

// ---------------------------------------------------------------------------
// Process environment
// ---------------------------------------------------------------------------

/// `std::env::set_var`, which is unsafe in edition 2024 because other threads
/// may read the environment. Only call it at the top of `main`, before any
/// thread exists.
pub fn set_env(key: &str, value: &str) {
    // SAFETY: documented above; `main` calls this before starting threads.
    unsafe { std::env::set_var(key, value) }
}

// ---------------------------------------------------------------------------
// ioctl
// ---------------------------------------------------------------------------

/// An ioctl request number. The console ioctls predate the structured
/// `_IOC` encoding, so they are plain numbers.
pub type Request = rustix::ioctl::Opcode;

/// An ioctl with a raw argument, which is all the console ones need.
struct Raw {
    request: Request,
    arg: *mut c_void,
}

// SAFETY: `ioctl_int` and `ioctl_ptr` pair each request with the argument it
// takes; the kernel may write through `arg`, hence `IS_MUTATING`.
unsafe impl rustix::ioctl::Ioctl for Raw {
    type Output = ();
    const IS_MUTATING: bool = true;

    fn opcode(&self) -> rustix::ioctl::Opcode {
        self.request
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.arg
    }

    unsafe fn output_from_ptr(
        _: rustix::ioctl::IoctlOutput,
        _: *mut c_void,
    ) -> rustix::io::Result<()> {
        Ok(())
    }
}

/// Borrow a raw fd for one call. Every caller passes an fd it keeps open for
/// the duration, or a console fd that is never closed; an fd that is closed
/// anyway makes the call fail with `EBADF`.
pub fn borrow(fd: RawFd) -> BorrowedFd<'static> {
    // SAFETY: see above.
    unsafe { BorrowedFd::borrow_raw(fd) }
}

/// An ioctl that takes an integer.
pub fn ioctl_int(fd: RawFd, request: Request, arg: c_int) -> std::io::Result<()> {
    let arg = arg as isize as *mut c_void;
    // SAFETY: the kernel does not dereference an integer argument.
    Ok(unsafe { rustix::ioctl::ioctl(borrow(fd), Raw { request, arg }) }?)
}

/// An ioctl that reads or fills a `T`, which must be the struct `request`
/// works on (all of them are `#[repr(C)]` plain data).
pub fn ioctl_ptr<T>(fd: RawFd, request: Request, arg: &mut T) -> std::io::Result<()> {
    let arg = std::ptr::from_mut(arg).cast();
    // SAFETY: `arg` is valid for the call and `T` matches `request`.
    Ok(unsafe { rustix::ioctl::ioctl(borrow(fd), Raw { request, arg }) }?)
}
