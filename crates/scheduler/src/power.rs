//! Post-queue power actions (system-design §4.3): after a scheduled queue
//! drains, optionally sleep/hibernate/shutdown the machine — always behind a
//! cancellable countdown and never run elevated.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Post-queue action, mirroring the `queues.post_action` column values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PostAction {
    /// Do nothing.
    #[default]
    None,
    /// Suspend the system.
    Sleep,
    /// Hibernate the system.
    Hibernate,
    /// Shut the system down.
    Shutdown,
}

impl PostAction {
    /// Parses the `queues.post_action` column value.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "sleep" => Self::Sleep,
            "hibernate" => Self::Hibernate,
            "shutdown" => Self::Shutdown,
            _ => Self::None,
        }
    }

    /// Whether an actual OS action follows the countdown.
    #[must_use]
    pub fn is_real(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// OS power actions, injected so tests never touch the real machine.
pub trait PowerActions: Send + Sync {
    /// Suspend the system.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message when the OS call fails.
    fn sleep_system(&self) -> Result<(), String>;

    /// Hibernate the system (may fall back to sleep where unsupported).
    ///
    /// # Errors
    ///
    /// Returns a human-readable message when the OS call fails.
    fn hibernate(&self) -> Result<(), String>;

    /// Shut the system down.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message when the OS call fails.
    fn shutdown(&self) -> Result<(), String>;
}

/// No-op implementation for tests and non-interactive contexts.
#[derive(Debug, Default)]
pub struct NoopPower;

impl PowerActions for NoopPower {
    fn sleep_system(&self) -> Result<(), String> {
        Ok(())
    }

    fn hibernate(&self) -> Result<(), String> {
        Ok(())
    }

    fn shutdown(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Outcome of [`countdown_then_action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountdownOutcome {
    /// The countdown finished and the action was invoked.
    Fired,
    /// The countdown was cancelled — nothing happened.
    Cancelled,
}

/// Runs a cancellable countdown, then invokes the OS action.
///
/// The 60-second default countdown exists so the user (or a tray command)
/// can abort a pending sleep/hibernate/shutdown; `cancel` aborts it.
///
/// # Errors
///
/// The future itself never errors — OS failures are reported through
/// `on_execute` (`Err(message)`).
pub async fn countdown_then_action(
    action: PostAction,
    countdown: Duration,
    cancel: CancellationToken,
    power: std::sync::Arc<dyn PowerActions>,
    on_execute: impl FnOnce(&Result<(), String>) + Send + 'static,
) -> CountdownOutcome {
    if !action.is_real() {
        return CountdownOutcome::Cancelled;
    }
    let mut remaining = countdown;
    loop {
        if cancel.is_cancelled() {
            return CountdownOutcome::Cancelled;
        }
        tokio::select! {
            () = cancel.cancelled() => return CountdownOutcome::Cancelled,
            () = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
        if let Some(rest) = remaining.checked_sub(Duration::from_millis(200)) {
            remaining = rest;
        } else {
            break;
        }
    }
    let result = tokio::task::spawn_blocking(move || match action {
        PostAction::Sleep => power.sleep_system(),
        PostAction::Hibernate => power.hibernate(),
        PostAction::Shutdown => power.shutdown(),
        PostAction::None => Ok(()),
    })
    .await
    .unwrap_or_else(|err| Err(format!("power task panicked: {err}")));
    on_execute(&result);
    CountdownOutcome::Fired
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // tests may panic on failure

    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Counter(AtomicUsize);
    impl PowerActions for Counter {
        fn sleep_system(&self) -> Result<(), String> {
            Err("sleep".into())
        }
        fn hibernate(&self) -> Result<(), String> {
            Err("hibernate".into())
        }
        fn shutdown(&self) -> Result<(), String> {
            let _ = self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn countdown_fires_when_not_cancelled() {
        let power = Arc::new(Counter::default());
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = Arc::clone(&fired);
        let outcome = countdown_then_action(
            PostAction::Shutdown,
            Duration::from_secs(1),
            CancellationToken::new(),
            Arc::clone(&power) as Arc<dyn PowerActions>,
            move |result| {
                assert!(result.is_ok());
                fired2.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await;
        assert_eq!(outcome, CountdownOutcome::Fired);
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn countdown_can_be_cancelled_midway() {
        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        let fired = Arc::new(AtomicUsize::new(0));
        let fired2 = Arc::clone(&fired);
        let task = tokio::spawn(async move {
            countdown_then_action(
                PostAction::Hibernate,
                Duration::from_secs(60),
                cancel2,
                Arc::new(Counter::default()) as Arc<dyn PowerActions>,
                move |_| {
                    fired2.fetch_add(1, Ordering::SeqCst);
                },
            )
            .await
        });
        tokio::time::advance(Duration::from_secs(30)).await;
        cancel.cancel();
        assert_eq!(task.await.expect("join"), CountdownOutcome::Cancelled);
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn post_action_parses_column_values() {
        assert_eq!(PostAction::parse("none"), PostAction::None);
        assert_eq!(PostAction::parse("sleep"), PostAction::Sleep);
        assert_eq!(PostAction::parse("hibernate"), PostAction::Hibernate);
        assert_eq!(PostAction::parse("shutdown"), PostAction::Shutdown);
        assert_eq!(PostAction::parse("garbage"), PostAction::None);
    }
}
