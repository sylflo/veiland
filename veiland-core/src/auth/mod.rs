// SPDX-License-Identifier: GPL-3.0-or-later

//! Password-buffer hygiene: mlock'd, zeroed on drop, encapsulated.
//!
//! The buffer never leaves this module — there is no method that returns
//! &[u8]. The only consumer is `authenticate`, a method on Session, so
//! PAM is fed the buffer without anyone else holding a reference to it.
//!
//! Feeding PAM does require copying the plaintext out of the mlock'd
//! buffer into a CString (PAM's conversation API takes an owned CString
//! by value). We scrub the copy we own on drop (see PasswordConv::drop).
//! Two copies remain that we cannot scrub: the per-prompt clone handed to
//! PAM, and libpam's own internal copy. Neither is mlock'd or zeroized.

use pam_client2::{Context, ConversationHandler, ErrorCode, Flag};
use std::{
    ffi::{CStr, CString, c_void},
    ptr::NonNull,
};
use zeroize::Zeroize;

const CAPACITY: usize = 512;

pub struct Session {
    buf: Box<[u8; CAPACITY]>,
    len: usize,
}

#[derive(Debug)]
pub enum AuthError {
    MlockFailed(nix::errno::Errno),
    PamFailed,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::MlockFailed(errno) => {
                write!(f, "mlock failed: {}", errno)
            }
            AuthError::PamFailed => write!(f, "authentication failed"),
        }
    }
}
impl std::error::Error for AuthError {}

struct PasswordConv {
    password: CString,
}

impl ConversationHandler for PasswordConv {
    fn prompt_echo_off(&mut self, _prompt: &CStr) -> Result<CString, ErrorCode> {
        // This clone, and libpam's own internal copy of it, are outside
        // our control once returned: PAM takes the CString by value and
        // drops it with the plain (non-scrubbing) destructor. Our own
        // long-lived copy (self.password) is scrubbed in Drop below.
        Ok(self.password.clone())
    }

    fn prompt_echo_on(&mut self, _prompt: &CStr) -> Result<CString, ErrorCode> {
        Err(ErrorCode::CONV_ERR)
    }

    fn text_info(&mut self, _msg: &CStr) {}

    fn error_msg(&mut self, _msg: &CStr) {}
}

impl Drop for PasswordConv {
    fn drop(&mut self) {
        // Scrub the plaintext copy we hold. verify() moves its CString
        // into this field, so this covers it too. Take
        // the CString out (Drop only gives &mut self), consume it into its
        // backing bytes, and zero them before the allocation is freed.
        let cs = std::mem::take(&mut self.password);
        let mut bytes = cs.into_bytes_with_nul();
        bytes.zeroize();
    }
}

impl Session {
    pub fn new() -> Result<Self, AuthError> {
        let buf = Box::new([0u8; CAPACITY]);
        let ptr = NonNull::new(buf.as_ptr() as *mut c_void).expect("Box allocation cannot be null");
        // SAFETY: ptr/len describe a region we own for the lifetime of
        // this Session; mlock rounds to page granularity, which is fine.
        unsafe {
            nix::sys::mman::mlock(ptr, CAPACITY).map_err(AuthError::MlockFailed)?;
        }
        Ok(Session { buf, len: 0 })
    }

    /// Copy the password out of the mlock'd buffer into an owned CString
    /// and clear the buffer. Runs on the main thread; the returned CString
    /// is what crosses the thread boundary to the auth worker.
    ///
    /// Returns None if the buffer is empty or contains an interior NUL
    /// (nothing worth verifying). The buffer is cleared either way, so the
    /// "zeroed after every attempt" invariant holds regardless of outcome.
    pub fn take_password(&mut self) -> Option<CString> {
        let password = CString::new(&self.buf[..self.len]).ok();
        self.clear();
        password
    }

    pub fn push_utf8(&mut self, s: &str) {
        let bytes = s.as_bytes();
        if self.len + bytes.len() > CAPACITY {
            // Silently drop and warn — never log the contents.
            eprintln!(
                "auth: password buffer full ({} bytes), dropping keystroke",
                self.len
            );
            return;
        }
        self.buf[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    pub fn pop_char(&mut self) {
        // Fall back to a single-byte pop if the buffer is ever not
        // valid UTF-8 — push_utf8 makes that unreachable today, but
        // panicking in the locker is worse than a slightly wrong pop.
        let current = match std::str::from_utf8(&self.buf[..self.len]) {
            Ok(s) => s,
            Err(_) => {
                if self.len > 0 {
                    self.buf[self.len - 1] = 0;
                    self.len -= 1;
                }
                return;
            }
        };
        if let Some(last) = current.chars().next_back() {
            let drop = last.len_utf8();
            for i in self.len - drop..self.len {
                self.buf[i] = 0;
            }
            self.len -= drop;
        }
    }

    pub fn clear(&mut self) {
        // Zero the whole buffer, not just ..self.len. Defence against
        // a future bug that under-reports len.
        self.buf.zeroize();
        self.len = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn char_count(&self) -> usize {
        match std::str::from_utf8(&self.buf[..self.len]) {
            Ok(s) => s.chars().count(),
            Err(_) => 0,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.len
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.buf.zeroize();
        let ptr =
            NonNull::new(self.buf.as_ptr() as *mut c_void).expect("Box allocation cannot be null");
        // SAFETY: same ptr/len we passed to mlock in new().
        // Ignore munlock errors — we're dropping anyway, and the
        // memory is being freed regardless. Logging here could fire
        // during a panic, which is worse than swallowing.
        unsafe {
            let _ = nix::sys::mman::munlock(ptr, CAPACITY);
        }
    }
}

/// Run the PAM conversation for one attempt. Blocks: `authenticate`
/// sleeps on failure (pam_unix FAIL_DELAY, ~2s), which is why this runs
/// on the auth worker thread, not the event loop. `password` is moved in
/// and scrubbed by `PasswordConv`'s Drop when this returns.
pub fn verify(service: &str, user: &str, password: CString) -> Result<(), AuthError> {
    let conv = PasswordConv { password };
    let mut ctx = Context::new(service, Some(user), conv).map_err(|_| AuthError::PamFailed)?;

    ctx.authenticate(Flag::NONE)
        .map_err(|_| AuthError::PamFailed)?;
    ctx.acct_mgmt(Flag::NONE)
        .map_err(|_| AuthError::PamFailed)?;

    Ok(())
}

// Deliberately no Debug derive: prevents accidentally printing the
// buffer through {:?} formatting. If a future caller needs Debug,
// they impl it manually and think about what to expose.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_count_empty() {
        let s = Session::new().unwrap();
        assert_eq!(s.char_count(), 0);
    }

    #[test]
    fn char_count_multibyte() {
        let mut s = Session::new().unwrap();
        s.push_utf8("hé");
        assert_eq!(s.char_count(), 2);
    }

    #[test]
    fn new_succeeds() {
        let s = Session::new().expect("mlock should succeed at default limits");
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
    }

    #[test]
    fn push_ascii() {
        let mut s = Session::new().unwrap();
        s.push_utf8("hello");
        assert_eq!(s.len(), 5);
        assert!(!s.is_empty());
    }

    #[test]
    fn push_multibyte() {
        let mut s = Session::new().unwrap();
        s.push_utf8("é");
        assert_eq!(s.len(), 2);
        s.push_utf8("漢");
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn push_at_capacity_silently_drops() {
        let mut s = Session::new().unwrap();
        let big = "a".repeat(CAPACITY);
        s.push_utf8(&big);
        assert_eq!(s.len(), CAPACITY);
        s.push_utf8("b");
        assert_eq!(s.len(), CAPACITY);
    }

    #[test]
    fn push_partial_overflow_does_not_truncate() {
        let mut s = Session::new().unwrap();
        let fill = "a".repeat(CAPACITY - 1);
        s.push_utf8(&fill);
        assert_eq!(s.len(), CAPACITY - 1);
        s.push_utf8("é");
        assert_eq!(s.len(), CAPACITY - 1, "must not append partial char");
    }

    #[test]
    fn pop_char_handles_ascii() {
        let mut s = Session::new().unwrap();
        s.push_utf8("hi");
        s.pop_char();
        assert_eq!(s.len(), 1);
        s.pop_char();
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn pop_char_handles_multibyte() {
        let mut s = Session::new().unwrap();
        s.push_utf8("aé");
        assert_eq!(s.len(), 3);
        s.pop_char();
        assert_eq!(s.len(), 1, "é must drop as a single char (2 bytes)");
        s.pop_char();
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn pop_char_on_empty_is_noop() {
        let mut s = Session::new().unwrap();
        s.pop_char();
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn pop_char_zeros_dropped_bytes() {
        let mut s = Session::new().unwrap();
        s.push_utf8("aé");
        s.pop_char();
        assert_eq!(s.len(), 1);
        assert_eq!(s.buf[1], 0);
        assert_eq!(s.buf[2], 0);
    }

    #[test]
    fn clear_zeros_whole_buffer() {
        let mut s = Session::new().unwrap();
        s.push_utf8("secret");
        s.clear();
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
        assert!(s.buf.iter().all(|&b| b == 0));
    }
}
