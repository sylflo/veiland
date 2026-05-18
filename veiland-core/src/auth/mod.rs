// SPDX-License-Identifier: GPL-3.0-or-later

//! Password-buffer hygiene: mlock'd, zeroed on drop, encapsulated.
//!
//! The bytes never leave this module — there is no method that returns
//! &[u8]. The only consumer will be `authenticate` (M4 step 3), added
//! as a method on Session so PAM is fed the buffer without anyone else
//! ever holding a reference to it.

use std::ffi::c_void;
use std::ptr::NonNull;
use zeroize::Zeroize;

const CAPACITY: usize = 512;

pub struct Session {
    buf: Box<[u8; CAPACITY]>,
    len: usize,
}

#[derive(Debug)]
pub enum AuthError {
    MlockFailed(nix::errno::Errno),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::MlockFailed(errno) => {
                write!(f, "mlock failed: {}", errno)
            }
        }
    }
}
impl std::error::Error for AuthError {}

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

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
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

// Deliberately no Debug derive: prevents accidentally printing the
// buffer through {:?} formatting. If a future caller needs Debug,
// they impl it manually and think about what to expose.

#[cfg(test)]
mod tests {
    use super::*;

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
