//! One-way wakeups from producers to the UI's event loops.
//!
//! # Why this exists
//!
//! The window mirrors state that changes on other threads: keystrokes decoded
//! by the audio engine, and config writes made by the Ctrl+Alt+M hotkey, the
//! tray callback and the update checker. None of those can touch a `Signal`
//! (the shared one uses Dioxus's non-`Send` storage), so the window used to
//! discover their work by *polling* - `libs/ui.rs` re-read a generation
//! counter and drained the event channel every millisecond. That is a thousand
//! scheduled no-op wakeups per second, for the whole session, whether or not
//! anything was happening.
//!
//! A [`Notify`] inverts it: producers keep producing exactly as before and
//! only announce themselves when they do; the loop parks in between and costs
//! nothing while idle.
//!
//! # Why this cannot lose a wake
//!
//! [`Notify::notify_one`] stores a permit when no waiter is registered, and
//! the next [`Notify::notified`] consumes it immediately. So a change that
//! lands between the loop draining its queues and parking is not dropped - the
//! loop turns straight around and drains again.
//!
//! Several wakes arriving back to back collapse into that single permit. That
//! is safe precisely because a wake is a *hint*, not a queue: the loop drains
//! everything that is pending rather than reading one item per wake, so one
//! permit is enough to catch up with any backlog.

use once_cell::sync::Lazy;
use tokio::sync::Notify;

/// The single wake channel for the UI event loop.
static UI: Lazy<Notify> = Lazy::new(Notify::new);

/// Announce on `ui`, which is the whole implementation: [`UI`] in production.
///
/// Kept as a parameter so the tests can drive a private channel (see below)
/// without touching the process-wide one, while exercising exactly the code
/// the UI runs.
fn announce(ui: &Notify) {
    ui.notify_one();
}

/// Completes on the next announcement on `ui`.
async fn await_announcement(ui: &Notify) {
    ui.notified().await;
}

/// Announce that something is waiting for the UI event loop.
///
/// Cheap enough to call per keystroke: an uncontended atomic, plus unparking a
/// thread only when the loop is actually parked (which is the normal state).
pub fn wake_ui() {
    announce(&UI);
}

/// Completes on the next [`wake_ui`].
///
/// Call this *after* draining the queues it feeds; a wake that races with the
/// drain is kept as a permit and returns here immediately.
pub async fn wait_ui() {
    await_announcement(&UI).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Every behavioural test below drives its **own** `Notify`, not [`UI`].
    ///
    /// The channel is process-wide in production, and [`crate::state::config_writer::apply`]
    /// announces on it whenever settings change - which the config tests do
    /// constantly, in parallel with these. One of those permits landing while
    /// the idle test was asserting that an empty wait blocks is what made it
    /// fail: the wake came from another test's `apply`, not from the channel
    /// under test. A private channel keeps the assertion about the mechanism
    /// and drops the dependence on what every other test in the binary is doing.

    /// The race the whole design hinges on: a producer announcing work before
    /// the consumer has reached its await. If the permit were dropped instead
    /// of stored, the loop would park on a change it never learns about and
    /// the window would stop updating until the next unrelated wake.
    #[tokio::test]
    async fn a_wake_raised_before_the_wait_is_not_lost() {
        let ui = Notify::new();
        announce(&ui);

        tokio::time::timeout(Duration::from_millis(2_000), await_announcement(&ui))
            .await
            .expect("a wake raised with no waiter must complete the next wait");
    }

    /// Two wakes back to back collapse into one permit by design, so the loop
    /// must still make progress: one wait is enough to catch up with a backlog.
    #[tokio::test]
    async fn stacked_wakes_still_complete_a_wait() {
        let ui = Notify::new();
        announce(&ui);
        announce(&ui);

        tokio::time::timeout(Duration::from_millis(2_000), await_announcement(&ui))
            .await
            .expect("collapsed wakes must still wake the loop");
    }

    /// The idle shape: with no producer active the wait must actually block,
    /// because a wait that returns immediately is a spin loop in disguise -
    /// the exact defect replacing the old poll loop must not come back.
    #[tokio::test]
    async fn an_idle_wait_does_not_return_on_its_own() {
        let ui = Notify::new();
        let woke = tokio::time::timeout(Duration::from_millis(100), await_announcement(&ui)).await;
        assert!(woke.is_err(), "waiting with nothing announced must park");
    }

    /// The one test that uses the real channel: the public pair must still be
    /// wired to it, since everything else above proves only the mechanism.
    ///
    /// Safe against parallel tests: a wake only ever *adds* a permit, so the
    /// extra permits an `apply` elsewhere can leave behind make this complete
    /// sooner, never later.
    #[tokio::test]
    async fn the_public_wake_and_wait_pair_is_wired_to_the_ui_channel() {
        wake_ui();

        tokio::time::timeout(Duration::from_millis(2_000), wait_ui())
            .await
            .expect("wake_ui must satisfy wait_ui");
    }
}
