// Global tray service for coordinating tray menu updates across the application
use crossbeam_channel::{ unbounded, Receiver, Sender };
use once_cell::sync::Lazy;

#[derive(Debug, Clone)]
pub enum TrayUpdateMessage {
    RefreshMenu,
}

/// Broadcasts "the mute state changed, redraw the menu" to whoever owns the
/// tray.
///
/// Crossbeam rather than `std::sync::mpsc` so the receiver can join the
/// `select!` that waits on the window's other queues: the window thread parks
/// on all of them at once instead of waking twenty times a second to ask each
/// one whether anything happened.
pub struct TrayUpdateService {
    sender: Sender<TrayUpdateMessage>,
    receiver: Receiver<TrayUpdateMessage>,
}

impl TrayUpdateService {
    fn new() -> Self {
        let (sender, receiver) = unbounded();
        Self { sender, receiver }
    }

    /// Send a request to update the tray menu
    pub fn request_update(&self) {
        if let Err(e) = self.sender.send(TrayUpdateMessage::RefreshMenu) {
            crate::always_eprint!("❌ Failed to send tray update request: {}", e);
        }
    }

    /// A handle on the request queue, for a thread that waits for one instead
    /// of polling for it.
    pub fn receiver(&self) -> Receiver<TrayUpdateMessage> {
        self.receiver.clone()
    }
}

// Global tray update service instance
pub static TRAY_UPDATE_SERVICE: Lazy<TrayUpdateService> = Lazy::new(TrayUpdateService::new);

/// Request a tray menu update from anywhere in the application
pub fn request_tray_update() {
    TRAY_UPDATE_SERVICE.request_update();
    crate::always_print!("🔄 Tray menu update requested");
}
