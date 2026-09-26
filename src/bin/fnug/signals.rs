//! SIGINT, SIGTERM and SIGHUP as cancellation, so fnug can stop its commands before it exits.

use std::io;
use std::process::ExitCode;

use fnug::process::StopSignal;
use fnug::runner::CancelCause;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

/// A cancellation token that the first termination signal cancels.
pub struct Signals {
    pub cancel: CancellationToken,
    /// The signal that cancelled `cancel`.
    pub cause: CancelCause,
}

impl Signals {
    /// 128 plus the number of the first signal received, the shell's code for dying by it.
    pub fn exit_code(&self) -> Option<ExitCode> {
        let signal = self.cause.signal()?;
        Some(ExitCode::from(
            u8::try_from(128 + signal.raw()).unwrap_or(u8::MAX),
        ))
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
    let cause = CancelCause::default();
    for (kind, received) in [
        (SignalKind::interrupt(), StopSignal::Interrupt),
        (SignalKind::terminate(), StopSignal::Terminate),
        (SignalKind::hangup(), StopSignal::Hangup),
    ] {
        let mut stream = signal(kind)?;
        let cancel = cancel.clone();
        let cause = cause.clone();
        tokio::spawn(async move {
            while stream.recv().await.is_some() {
                cause.set_signal(received);
                cancel.cancel();
            }
        });
    }
    Ok(Signals { cancel, cause })
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
