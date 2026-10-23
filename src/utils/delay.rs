use std::time::Duration;

/// Utility functions for common delays used throughout the application
pub struct Delay;

impl Delay {
    /// Custom delay with specified duration in milliseconds
    pub async fn ms(ms: u64) {
        futures_timer::Delay::new(Duration::from_millis(ms)).await;
    }
}
