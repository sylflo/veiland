// SPDX-License-Identifier: GPL-3.0-or-later

//! Fingerprint unlock: a second PAM conversation on the
//! `veiland-fingerprint` service, run on its own thread alongside the
//! password path (the gdm-fingerprint / kde-fingerprint model).
//!
//! This module only reports what PAM said; the unlock decision stays on
//! the main thread (`AppData::commit_unlock`). The fingerprint stack is
//! never handed a secret: any password prompt is refused and ends
//! fingerprint unlock for this lock.

use std::{
    ffi::{CStr, CString},
    path::Path,
    time::Duration,
};

use pam_client2::{Context, ConversationHandler, ErrorCode, Flag};

/// Hardcoded like the password service: the config must not be able to
/// point the core at an arbitrary PAM stack.
pub const SERVICE: &str = "veiland-fingerprint";

/// After this many `MAXTRIES` rounds (3 mismatches each with pam_fprintd's
/// default), stop and leave the password as the only way in. Restarting
/// forever would defeat the module's own brute-force limit.
const MAX_MAXTRIES_ROUNDS: u32 = 2;

const RETRY_DELAY: Duration = Duration::from_secs(1);
const UNAVAILABLE_BACKOFF_START: Duration = Duration::from_secs(2);
const UNAVAILABLE_BACKOFF_CAP: Duration = Duration::from_secs(10);

/// Sent from the fingerprint thread to the main thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpEvent {
    /// The module is waiting for a finger (it sent a TEXT_INFO).
    Prompt,
    /// A scan was rejected; the module may still be waiting.
    NoMatch,
    /// PAM authenticated the fingerprint.
    Matched,
    /// The reader isn't answering (no device yet, fprintd restarting).
    /// Retrying in the background.
    Waiting,
}

/// What one PAM conversation on the fingerprint service reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    pub result: Result<(), ErrorCode>,
    /// The module sent at least one TEXT_INFO.
    pub prompted: bool,
    /// The stack asked for a password or other typed input.
    pub asked_secret: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    Unlock,
    Retry(Duration),
    Stop(&'static str),
}

/// Decides what to do after each attempt. Pure, so it is unit-tested
/// without PAM.
#[derive(Debug, Default)]
pub struct Policy {
    maxtries_rounds: u32,
    unavailable_streak: u32,
}

impl Policy {
    pub fn next(&mut self, a: &Attempt) -> Next {
        if a.asked_secret {
            return Next::Stop(
                "the veiland-fingerprint PAM stack asked for a password; \
                 it must only contain a fingerprint module",
            );
        }
        match a.result {
            // pam_fprintd always prompts before it waits for a finger, so a
            // silent success is a misconfigured stack (e.g. a missing file
            // falling back to `other`, or pam_permit). Never unlock on it.
            Ok(()) if a.prompted => Next::Unlock,
            Ok(()) => Next::Stop(
                "the veiland-fingerprint PAM stack succeeded without asking \
                 for a finger; refusing it",
            ),
            Err(ErrorCode::MAXTRIES) => {
                self.unavailable_streak = 0;
                self.maxtries_rounds += 1;
                if self.maxtries_rounds >= MAX_MAXTRIES_ROUNDS {
                    Next::Stop("too many fingerprint mismatches; use the password")
                } else {
                    Next::Retry(RETRY_DELAY)
                }
            }
            // Timed out (or failed) while waiting for a finger: listen again.
            Err(_) if a.prompted => {
                self.unavailable_streak = 0;
                Next::Retry(RETRY_DELAY)
            }
            // Never got as far as waiting for a finger. Keep trying with
            // backoff: right after resume the reader can take a while to
            // come back.
            Err(_) => {
                self.unavailable_streak += 1;
                let factor = 1u32 << (self.unavailable_streak - 1).min(3);
                Next::Retry((UNAVAILABLE_BACKOFF_START * factor).min(UNAVAILABLE_BACKOFF_CAP))
            }
        }
    }
}

struct FingerprintConv<'a> {
    emit: &'a mut dyn FnMut(FpEvent),
    prompted: bool,
    asked_secret: bool,
}

// Called from libpam (C): nothing here may panic.
impl ConversationHandler for FingerprintConv<'_> {
    fn prompt_echo_on(&mut self, _prompt: &CStr) -> Result<CString, ErrorCode> {
        self.asked_secret = true;
        Err(ErrorCode::CONV_ERR)
    }

    fn prompt_echo_off(&mut self, _prompt: &CStr) -> Result<CString, ErrorCode> {
        self.asked_secret = true;
        Err(ErrorCode::CONV_ERR)
    }

    // The message text is translated by the module; never parse it.
    fn text_info(&mut self, _msg: &CStr) {
        self.prompted = true;
        (self.emit)(FpEvent::Prompt);
    }

    fn error_msg(&mut self, _msg: &CStr) {
        (self.emit)(FpEvent::NoMatch);
    }
}

/// Run one PAM conversation on the fingerprint service. Blocks until the
/// module returns (a match, a timeout, or max tries).
fn attempt(user: &str, emit: &mut dyn FnMut(FpEvent)) -> Attempt {
    let conv = FingerprintConv {
        emit,
        prompted: false,
        asked_secret: false,
    };
    let mut ctx = match Context::new(SERVICE, Some(user), conv) {
        Ok(ctx) => ctx,
        Err(e) => {
            return Attempt {
                result: Err(e.code()),
                prompted: false,
                asked_secret: false,
            };
        }
    };
    let result = ctx.authenticate(Flag::NONE).map_err(|e| e.code());
    let conv = ctx.conversation();
    Attempt {
        result,
        prompted: conv.prompted,
        asked_secret: conv.asked_secret,
    }
}

/// Whether the PAM service file is installed. Without it libpam falls
/// back to `/etc/pam.d/other`, which we never want to run.
pub fn service_installed() -> bool {
    ["/etc/pam.d", "/usr/lib/pam.d"]
        .iter()
        .any(|dir| Path::new(dir).join(SERVICE).is_file())
}

/// Thread body. `emit` returns false once the main thread is gone.
pub fn run(user: &str, emit: impl FnMut(FpEvent) -> bool) {
    run_loop(|emit| attempt(user, emit), emit, std::thread::sleep);
}

fn run_loop(
    mut attempt: impl FnMut(&mut dyn FnMut(FpEvent)) -> Attempt,
    mut emit: impl FnMut(FpEvent) -> bool,
    mut sleep: impl FnMut(Duration),
) {
    let mut policy = Policy::default();
    let mut waiting = false;
    loop {
        let a = attempt(&mut |ev| {
            let _ = emit(ev);
        });
        match policy.next(&a) {
            Next::Unlock => {
                let _ = emit(FpEvent::Matched);
                return;
            }
            Next::Stop(why) => {
                eprintln!("veiland-core: fingerprint: {why}; fingerprint unlock off for this lock");
                return;
            }
            Next::Retry(delay) => {
                if a.prompted {
                    waiting = false;
                } else {
                    if !waiting {
                        eprintln!(
                            "veiland-core: fingerprint: reader not available ({:?}); retrying",
                            a.result
                        );
                        waiting = true;
                    }
                    if !emit(FpEvent::Waiting) {
                        return;
                    }
                }
                sleep(delay);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt_of(result: Result<(), ErrorCode>, prompted: bool) -> Attempt {
        Attempt {
            result,
            prompted,
            asked_secret: false,
        }
    }

    #[test]
    fn prompted_success_unlocks() {
        let mut p = Policy::default();
        assert_eq!(p.next(&attempt_of(Ok(()), true)), Next::Unlock);
    }

    #[test]
    fn silent_success_never_unlocks() {
        let mut p = Policy::default();
        assert!(matches!(p.next(&attempt_of(Ok(()), false)), Next::Stop(_)));
    }

    #[test]
    fn password_prompt_stops_even_on_success() {
        let mut p = Policy::default();
        let a = Attempt {
            result: Ok(()),
            prompted: true,
            asked_secret: true,
        };
        assert!(matches!(p.next(&a), Next::Stop(_)));
    }

    #[test]
    fn maxtries_is_capped() {
        let mut p = Policy::default();
        let a = attempt_of(Err(ErrorCode::MAXTRIES), true);
        assert_eq!(p.next(&a), Next::Retry(RETRY_DELAY));
        assert!(matches!(p.next(&a), Next::Stop(_)));
    }

    #[test]
    fn timeout_after_prompt_retries() {
        let mut p = Policy::default();
        let a = attempt_of(Err(ErrorCode::AUTHINFO_UNAVAIL), true);
        for _ in 0..10 {
            assert_eq!(p.next(&a), Next::Retry(RETRY_DELAY));
        }
    }

    #[test]
    fn unavailable_backs_off_and_keeps_trying() {
        let mut p = Policy::default();
        let a = attempt_of(Err(ErrorCode::AUTHINFO_UNAVAIL), false);
        let delays: Vec<u64> = (0..6)
            .map(|_| match p.next(&a) {
                Next::Retry(d) => d.as_secs(),
                other => panic!("expected Retry, got {other:?}"),
            })
            .collect();
        assert_eq!(delays, [2, 4, 8, 10, 10, 10]);
    }

    #[test]
    fn prompt_resets_backoff() {
        let mut p = Policy::default();
        p.next(&attempt_of(Err(ErrorCode::AUTHINFO_UNAVAIL), false));
        p.next(&attempt_of(Err(ErrorCode::AUTHINFO_UNAVAIL), false));
        p.next(&attempt_of(Err(ErrorCode::AUTHINFO_UNAVAIL), true));
        assert_eq!(
            p.next(&attempt_of(Err(ErrorCode::AUTHINFO_UNAVAIL), false)),
            Next::Retry(UNAVAILABLE_BACKOFF_START)
        );
    }

    #[test]
    fn conversation_refuses_secrets() {
        let mut events = Vec::new();
        let mut emit = |ev| events.push(ev);
        let mut conv = FingerprintConv {
            emit: &mut emit,
            prompted: false,
            asked_secret: false,
        };
        assert_eq!(
            conv.prompt_echo_off(c"Password: "),
            Err(ErrorCode::CONV_ERR)
        );
        assert_eq!(conv.prompt_echo_on(c"Login: "), Err(ErrorCode::CONV_ERR));
        assert!(conv.asked_secret);
        assert!(!conv.prompted);
    }

    #[test]
    fn conversation_forwards_messages() {
        let mut events = Vec::new();
        {
            let mut emit = |ev| events.push(ev);
            let mut conv = FingerprintConv {
                emit: &mut emit,
                prompted: false,
                asked_secret: false,
            };
            conv.text_info(c"Place your finger on the reader");
            conv.error_msg(c"Failed to match fingerprint");
            assert!(conv.prompted);
        }
        assert_eq!(events, [FpEvent::Prompt, FpEvent::NoMatch]);
    }

    #[test]
    fn loop_unlocks_after_a_match() {
        let mut script = vec![
            attempt_of(Ok(()), true),
            attempt_of(Err(ErrorCode::AUTHINFO_UNAVAIL), true),
            attempt_of(Err(ErrorCode::AUTHINFO_UNAVAIL), false),
        ];
        let mut events = Vec::new();
        let mut sleeps = Vec::new();
        run_loop(
            |_| script.pop().expect("loop ran past the script"),
            |ev| {
                events.push(ev);
                true
            },
            |d| sleeps.push(d),
        );
        assert_eq!(events, [FpEvent::Waiting, FpEvent::Matched]);
        assert_eq!(sleeps, [UNAVAILABLE_BACKOFF_START, RETRY_DELAY]);
    }

    #[test]
    fn loop_exits_when_main_thread_is_gone() {
        let mut calls = 0;
        run_loop(
            |_| {
                calls += 1;
                attempt_of(Err(ErrorCode::AUTHINFO_UNAVAIL), false)
            },
            |_| false,
            |_| {},
        );
        assert_eq!(calls, 1);
    }
}
