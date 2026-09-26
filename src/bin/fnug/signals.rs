//! SIGINT, SIGTERM and SIGHUP as cancellation, so fnug can stop its commands before it exits.

use std::io;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

/// A cancellation token that the first termination signal cancels.
pub struct Signals {
    pub cancel: CancellationToken,
    received: Arc<AtomicI32>,
}

impl Signals {
    /// 128 plus the number of the first signal received, the shell's code for dying by it.
    pub fn exit_code(&self) -> Option<ExitCode> {
        match self.received.load(Ordering::SeqCst) {
            0 => None,
            signal => Some(ExitCode::from(
                u8::try_from(128 + signal).unwrap_or(u8::MAX),
            )),
        }
    }
}

/// Listen for SIGINT, SIGTERM and SIGHUP. From now on they no longer end fnug by themselves:
/// the first one is recorded and cancels the token, later ones are ignored.
///
/// # Errors
///
/// Returns the error from registering a signal handler.
pub fn install() -> io::Result<Signals> {
    let cancel = CancellationToken::new();
    let received = Arc::new(AtomicI32::new(0));
    for kind in [
        SignalKind::interrupt(),
        SignalKind::terminate(),
        SignalKind::hangup(),
    ] {
        let mut stream = signal(kind)?;
        let cancel = cancel.clone();
        let received = received.clone();
        tokio::spawn(async move {
            while stream.recv().await.is_some() {
                let _ = received.compare_exchange(
                    0,
                    kind.as_raw_value(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                cancel.cancel();
            }
        });
    }
    Ok(Signals { cancel, received })
}

/// Let SIGTERM and SIGHUP end fnug again, for the TUI, which handles neither. SIGINT stays
/// with tokio, which the TUI listens to.
pub fn restore_default() {
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: setting the default disposition is always valid; it only replaces tokio's
        // handler, whose listeners then see no more signals.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
    }
}
