use std::time::{Duration, Instant};

use portable_pty::{PtySize, native_pty_system};

/// Whether a PTY can be opened here; prints a skip notice when it can't (e.g. nix sandbox).
pub(crate) fn pty_available() -> bool {
    let ok = native_pty_system().openpty(PtySize::default()).is_ok();
    if !ok {
        eprintln!("skipping: no PTY available");
    }
    ok
}

/// Poll `cond` every 10ms until it holds or `timeout` passes, returning its last value.
pub(crate) fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}
