//! Everything the window reacts to that does not come from the web view.
//!
//! # What this replaced
//!
//! `WindowController` used to walk three queues on a 50 ms timer - twenty
//! wakeups a second for the whole session, the overwhelming majority of them
//! finding nothing - because two of the queues belong to the tray library,
//! which offers neither a callback nor any other way to be told that a click
//! happened.
//!
//! This module does that wait the other way round. One thread sits in a
//! `select!` across all of them at once, so an event is delivered the instant
//! it arrives and *nothing at all* happens when none does: the thread has no
//! interval, no timeout and no backoff, it simply parks. The window side is
//! then a queue plus a [`Notify`] rather than a receiver - the loop that
//! drains it lives in a Dioxus `use_effect` closure, which must be `FnMut`
//! and therefore cannot take ownership of a channel, and a parked `Notify` is
//! exactly as free as a parked thread.

use crate::libs::tray::{ menu_event_receiver, menu_message, tray_icon_message, TrayMessage };
use crate::libs::tray_service::TRAY_UPDATE_SERVICE;
use crate::libs::window_manager::{ WindowAction, WINDOW_MANAGER };
use once_cell::sync::Lazy;
use std::collections::VecDeque;
use std::sync::atomic::{ AtomicBool, Ordering };
use std::sync::Mutex;
use tokio::sync::Notify;
use tray_icon::TrayIconEvent;

/// One thing the window has to act on.
pub enum Pending {
    /// Show or hide the window (`WINDOW_MANAGER` / the titlebar's hide button).
    WindowAction(WindowAction),
    /// The mute state changed somewhere; redraw the tray menu and icon.
    TrayUpdate,
    /// A tray icon or tray menu click.
    Tray(TrayMessage),
}

/// Messages waiting for the window, oldest first.
static QUEUE: Lazy<Mutex<VecDeque<Pending>>> = Lazy::new(|| Mutex::new(VecDeque::new()));

/// Parks the window's consumer between messages.
static READY: Lazy<Notify> = Lazy::new(Notify::new);

/// Keeps the blocking thread to one per process: the queue and its receivers
/// are process-wide, so a second `WindowController` mount must attach to the
/// running pump rather than start a competing one.
static STARTED: AtomicBool = AtomicBool::new(false);

/// What one pass of the `select!` came back with.
enum Hit {
    /// Something arrived on the window-action queue. `Err` means the queue was
    /// closed for good rather than that a message failed.
    WindowAction(Result<WindowAction, crossbeam_channel::RecvError>),
    /// An event from one of the tray queues, already translated - or one that
    /// translates to nothing, which is not worth waking the window for.
    Message(Option<Pending>),
}

/// The queue, recovered rather than propagated if a previous holder panicked -
/// a wedged tray would be a worse outcome than a poisoned lock.
fn queue() -> std::sync::MutexGuard<'static, VecDeque<Pending>> {
    match QUEUE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Hand `pending` to the window and wake it up.
fn push(pending: Pending) {
    queue().push_back(pending);
    // The permit is what closes the race with a consumer that has just drained
    // an empty queue: it comes back and finds this message (see `libs/wake.rs`,
    // which is the same primitive for the same reason).
    READY.notify_one();
}

/// Starts the pump if it is not running already.
///
/// Called from `use_hook`, i.e. during the first render of `WindowController`,
/// which is also where the window-action sender is installed - early enough
/// that a second app launch asking for the window finds a live receiver.
pub fn ensure_started() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    let (window_action_tx, window_action_rx) = crossbeam_channel::unbounded::<WindowAction>();
    WINDOW_MANAGER.set_action_sender(window_action_tx);
    let tray_update_rx: crossbeam_channel::Receiver<_> = TRAY_UPDATE_SERVICE.receiver();

    let built = std::thread::Builder::new()
        .name("mechvibes-pump".into())
        .spawn(move || {
            // Resolved on every pass, not captured once: on macOS the tray
            // swaps its menu queue for a private one when the handler is
            // installed, so the receiver in use has to be re-read.
            #[cfg(target_os = "macos")]
            crate::libs::tray::install_menu_event_handler();

            // `Some` until the window-action queue reports closed. A closed
            // channel is always ready, so selecting on it again would spin at
            // full speed; `never()` takes its place and keeps the other three
            // arms alive, which is what keeps the tray alive too.
            let mut window_actions: Option<crossbeam_channel::Receiver<WindowAction>> =
                Some(window_action_rx);

            loop {
                let window_action_queue = window_actions
                    .clone()
                    .unwrap_or_else(crossbeam_channel::never);
                let tray_icon_queue = TrayIconEvent::receiver().clone();
                let tray_update_queue = tray_update_rx.clone();
                let menu_queue = menu_event_receiver();

                let hit = crossbeam_channel::select! {
                    recv(window_action_queue) -> msg => Hit::WindowAction(msg),
                    recv(tray_update_queue) -> _ => Hit::Message(Some(Pending::TrayUpdate)),
                    recv(tray_icon_queue) ->
                        msg => Hit::Message(msg.ok().and_then(tray_icon_message).map(Pending::Tray)),
                    recv(menu_queue) ->
                        msg => Hit::Message(msg.ok().and_then(menu_message).map(Pending::Tray)),
                };

                match hit {
                    // An event the tray library sends but the window does not
                    // act on (a single click, a hover) simply lands here: it
                    // woke the thread, it just has nothing to deliver.
                    Hit::Message(Some(pending)) => push(pending),
                    Hit::Message(None) => {}
                    Hit::WindowAction(Ok(action)) => push(Pending::WindowAction(action)),
                    Hit::WindowAction(Err(_)) => {
                        crate::debug_print!("🔌 Window action queue closed; tray keeps pumping");
                        window_actions = None;
                    }
                }
            }
        });

    match built {
        Ok(_) => crate::debug_print!("🧵 Window pump started"),
        Err(e) => {
            STARTED.store(false, Ordering::SeqCst);
            crate::always_eprint!("❌ Failed to start the window pump: {}", e);
        }
    }
}

/// Completes when the pump has queued something for the window.
///
/// The window must drain with [`next`] until it returns `None` afterwards: a
/// wake is a hint that *something* arrived, not a count of it.
pub async fn wait() {
    READY.notified().await;
}

/// The oldest queued message, or `None` when the queue is empty.
pub fn next() -> Option<Pending> {
    queue().pop_front()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The queue and its pending wake are process-global, so the tests that
    /// assert on them must not overlap.
    static SERIALISE: Mutex<()> = Mutex::new(());

    /// Leaves an empty queue and a parked consumer, which is the state every
    /// test wants to start from - a wake left behind by the previous test
    /// would otherwise make the next one's assertions meaningless.
    async fn start_clean() {
        while next().is_some() {}
        // At most one wake can be pending, so this parks after spending it.
        while tokio::time::timeout(Duration::from_millis(50), wait()).await.is_ok() {
            while next().is_some() {}
        }
    }

    #[tokio::test]
    async fn messages_are_handed_out_oldest_first() {
        let _serialised = SERIALISE.lock().unwrap_or_else(|e| e.into_inner());
        start_clean().await;

        assert!(next().is_none(), "a clean queue has nothing to give");

        push(Pending::TrayUpdate);
        push(Pending::WindowAction(WindowAction::Hide));

        assert!(matches!(next(), Some(Pending::TrayUpdate)), "FIFO order");
        assert!(
            matches!(next(), Some(Pending::WindowAction(WindowAction::Hide))),
            "and the second message follows the first"
        );
        assert!(next().is_none(), "drained means empty, not more");
    }

    #[tokio::test]
    async fn a_message_wakes_the_window() {
        let _serialised = SERIALISE.lock().unwrap_or_else(|e| e.into_inner());
        start_clean().await;

        push(Pending::TrayUpdate);

        tokio::time::timeout(Duration::from_millis(2_000), wait())
            .await
            .expect("a queued message must wake the consumer");
        assert!(matches!(next(), Some(Pending::TrayUpdate)));
    }

    /// An idle window must stay parked: a consumer that keeps waking itself is
    /// the poll loop this module replaced, wearing a different hat.
    #[tokio::test]
    async fn an_empty_queue_does_not_wake_anyone() {
        let _serialised = SERIALISE.lock().unwrap_or_else(|e| e.into_inner());
        start_clean().await;

        let woken = tokio::time::timeout(Duration::from_millis(100), wait()).await;
        assert!(woken.is_err(), "nothing was queued, so nothing may wake");
    }
}
