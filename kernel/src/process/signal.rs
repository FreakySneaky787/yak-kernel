//! Signals: asynchronous notifications for a process (numbers as on Linux).
//!
//! Every process has a disposition per signal: default (end the process),
//! ignore, or a handler function in user space. `send` sets the signal's
//! pending bit and wakes the process; blocking system calls notice it
//! (`pending_for_current`) and return `EINTR`. On the way back to user mode
//! - after a system call, or after a timer / wake interrupt - `deliver`
//! acts on the lowest pending signal (SIGKILL first).
//!
//! Running a handler: the kernel pushes four words onto the user stack
//! (`sig`, `handler`, `rflags`, `rip` - the interrupted instruction) and
//! continues the program at its *trampoline* (registered with `sigaction`,
//! it lives in `ulib`). The trampoline saves every register and the SSE
//! state, calls `handler(sig)`, calls `sigreturn` (which allows the next
//! handler to run - until then further signals stay pending), restores
//! everything and returns with `popfq; ret`, which also restores RSP. No
//! register state passes through the kernel, so interrupts and system calls
//! need no full register frame. This requires code without a red zone
//! below RSP (the user target sets `disable-redzone`).
//!
//! `spawn` starts a program with default dispositions, except that signals
//! the parent ignores stay ignored (like `exec`).

pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGKILL: u32 = 9;
pub const SIGUSR1: u32 = 10;
pub const SIGSEGV: u32 = 11;
pub const SIGUSR2: u32 = 12;
pub const SIGPIPE: u32 = 13;
pub const SIGALRM: u32 = 14;
pub const SIGTERM: u32 = 15;
/// Signals are 1..NSIG.
pub const NSIG: u32 = 32;

/// Dispositions for `sigaction` (any other value is a handler address).
pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

/// Exit reason of a process ended by `sig` (also shown by the shell).
pub fn reason(sig: u32) -> &'static str {
    match sig {
        SIGHUP => "hangup",
        SIGINT => "interrupted",
        SIGQUIT => "quit",
        SIGKILL => "killed",
        SIGUSR1 => "user signal 1",
        SIGSEGV => "segmentation fault",
        SIGUSR2 => "user signal 2",
        SIGPIPE => "broken pipe",
        SIGALRM => "alarm",
        SIGTERM => "terminated",
        _ => "signal",
    }
}

/// The signal number reported for an exit reason; CPU exceptions count as SIGSEGV.
pub fn number_for_reason(text: &str) -> u32 {
    (1..NSIG)
        .find(|&s| reason(s) != "signal" && reason(s) == text)
        .unwrap_or(SIGSEGV)
}

/// What `deliver` has to do for the current process.
pub enum Action {
    /// End the process with this signal.
    Terminate(u32),
    /// Run `handler(sig)` through the trampoline.
    Handle { sig: u32, handler: u64, trampoline: u64 },
}

/// Per-process signal state.
#[derive(Clone)]
pub struct Signals {
    /// Index = signal number.
    handlers: [u64; NSIG as usize],
    /// User address of the `ulib` trampoline (0 = none registered).
    trampoline: u64,
    pending: u32,
    /// A handler runs (until `sigreturn`); handled signals wait meanwhile.
    in_handler: bool,
}

impl Signals {
    pub const fn new() -> Signals {
        Signals {
            handlers: [SIG_DFL; NSIG as usize],
            trampoline: 0,
            pending: 0,
            in_handler: false,
        }
    }

    /// The state for a new program started by a process with this state.
    pub fn for_child(&self) -> Signals {
        let mut child = Signals::new();
        for (sig, h) in self.handlers.iter().enumerate() {
            if *h == SIG_IGN {
                child.handlers[sig] = SIG_IGN;
            }
        }
        child
    }

    pub fn disposition(&self, sig: u32) -> u64 {
        self.handlers[sig as usize]
    }

    /// Whether the program installed a handler for `sig`.
    pub fn is_handled(&self, sig: u32) -> bool {
        self.handlers[sig as usize] > SIG_IGN
    }

    /// Sets the disposition of `sig`; returns the old one. SIGKILL cannot be
    /// changed, handlers need a trampoline.
    pub fn set(&mut self, sig: u32, handler: u64, trampoline: u64) -> Result<u64, ()> {
        if sig == 0 || sig >= NSIG || sig == SIGKILL || (handler > SIG_IGN && trampoline == 0) {
            return Err(());
        }
        if handler > SIG_IGN {
            self.trampoline = trampoline;
        }
        if handler == SIG_IGN {
            self.pending &= !(1 << sig);
        }
        Ok(core::mem::replace(&mut self.handlers[sig as usize], handler))
    }

    /// Marks `sig` as pending. False if the process ignores it.
    pub fn post(&mut self, sig: u32) -> bool {
        if sig != SIGKILL && self.handlers[sig as usize] == SIG_IGN {
            return false;
        }
        self.pending |= 1 << sig;
        true
    }

    /// The next signal to act on: SIGKILL first, then the lowest one that
    /// ends the process or whose handler may run now.
    fn next(&self) -> Option<u32> {
        if self.pending & (1 << SIGKILL) != 0 {
            return Some(SIGKILL);
        }
        (1..NSIG).find(|&s| self.pending & (1 << s) != 0 && (self.handlers[s as usize] == SIG_DFL || !self.in_handler))
    }

    /// Whether a signal needs attention (blocking system calls give up).
    pub fn is_pending(&self) -> bool {
        self.next().is_some()
    }

    /// Takes the next signal off the pending set and says what to do.
    pub fn take(&mut self) -> Option<Action> {
        let sig = self.next()?;
        self.pending &= !(1 << sig);
        let handler = self.handlers[sig as usize];
        if handler == SIG_DFL || sig == SIGKILL {
            return Some(Action::Terminate(sig));
        }
        self.in_handler = true;
        Some(Action::Handle {
            sig,
            handler,
            trampoline: self.trampoline,
        })
    }

    /// The handler returned (`sigreturn`).
    pub fn handler_done(&mut self) {
        self.in_handler = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn dispositions_and_pending_signals() {
        let mut s = Signals::new();
        assert!(s.take().is_none());
        assert!(s.set(SIGKILL, SIG_IGN, 0).is_err(), "SIGKILL must stay fatal");
        assert!(s.set(SIGUSR1, 0x40_1000, 0).is_err(), "a handler needs a trampoline");
        assert_eq!(s.set(SIGINT, SIG_IGN, 0), Ok(SIG_DFL));
        assert!(!s.post(SIGINT), "ignored signals are dropped");
        assert_eq!(s.set(SIGUSR1, 0x40_1000, 0x40_2000), Ok(SIG_DFL));
        assert!(s.post(SIGUSR1) && s.post(SIGTERM));
        // the handler first (lower number), then nothing handled while it runs
        assert!(matches!(
            s.take(),
            Some(Action::Handle {
                sig: SIGUSR1,
                handler: 0x40_1000,
                trampoline: 0x40_2000
            })
        ));
        assert!(s.post(SIGUSR1));
        // ...but default actions still happen
        assert!(matches!(s.take(), Some(Action::Terminate(SIGTERM))));
        assert!(!s.is_pending(), "the handled signal must wait for sigreturn");
        s.handler_done();
        assert!(matches!(s.take(), Some(Action::Handle { sig: SIGUSR1, .. })));
        // SIGKILL beats everything
        s.handler_done();
        assert!(s.post(SIGHUP) && s.post(SIGKILL));
        assert!(matches!(s.take(), Some(Action::Terminate(SIGKILL))));
        // a new program keeps only the ignored dispositions
        let child = s.for_child();
        assert_eq!(child.disposition(SIGINT), SIG_IGN);
        assert_eq!(child.disposition(SIGUSR1), SIG_DFL);
    }

    #[test_case]
    fn exit_reasons_map_back_to_numbers() {
        for sig in [SIGINT, SIGKILL, SIGTERM, SIGUSR1, SIGHUP] {
            assert_eq!(number_for_reason(reason(sig)), sig);
        }
        assert_eq!(number_for_reason("page fault at 0x0"), SIGSEGV);
    }
}
