use crate::libs::tray::TrayManager;
use crate::libs::window_manager::{ WindowAction, WINDOW_MANAGER };
use crate::libs::window_pump::{ self, Pending };
use crate::libs::AudioContext;
use crate::{ debug_print, always_eprint };
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use std::sync::Arc;

/// Brings the main window back and in front, from hidden or minimized.
///
/// `set_visible(true)` + `set_focus()` is enough on Windows and Linux. On
/// macOS it is not for a window the user minimized to the Dock: tao's
/// `set_visible(true)` only orders the window front, which leaves a
/// miniaturized window where it is, and `set_focus()` returns early for a
/// minimized window, so nothing appears and the app is never activated. The
/// macOS branch therefore restores it from the Dock first and activates the
/// app explicitly. Windows and Linux behavior is unchanged.
fn show_window(window: &dioxus::desktop::DesktopContext) {
    #[cfg(target_os = "macos")]
    crate::always_print!(
        "🔼 Show requested (macOS): minimized={}, visible={}",
        window.is_minimized(),
        window.is_visible()
    );

    #[cfg(target_os = "macos")]
    window.set_minimized(false);

    window.set_visible(true);
    window.set_focus();

    #[cfg(target_os = "macos")]
    crate::libs::native_window::activate_app();
}

#[component]
pub fn WindowController() -> Element {
    let window = use_window();

    // The tray's mute item toggles the same engine-cached flag the UI does,
    // so it has to notify the engine rather than only rewriting the config.
    let audio_ctx = use_context::<Arc<AudioContext>>();

    // `set_sound_enabled` persists the flag but does not publish it to the
    // shared config signal, so the UI would keep rendering the pre-toggle mute
    // state until some unrelated write happened to republish it.
    let (_config, update_config) = crate::utils::config::use_config();

    // Bring the window inside the screen before anything else runs.
    //
    // This cannot happen at build time in `main.rs`: which monitor the window
    // opens on, and that monitor's work area and scale factor, are only known
    // once the window exists. `use_hook` runs it exactly once per mount, on the
    // first render, so the correction lands before the user sees the window
    // rather than as a visible jump afterwards.
    {
        let window = window.clone();
        use_hook(move || {
            crate::libs::window_bounds::fit_window_to_monitor(
                &window,
                crate::libs::window_bounds::DEFAULT_WINDOW_SIZE,
                crate::libs::window_bounds::MIN_WINDOW_SIZE
            );
        });
    }

    // One thread, blocked on every queue this component used to poll on a
    // 50 ms timer (window actions, tray refresh requests, tray clicks). Started
    // during render rather than in an effect so the window-action sender is
    // installed as early as possible - a second app launch retries against it.
    use_hook(|| window_pump::ensure_started());

    // Create a signal to hold the tray manager
    let mut tray_manager = use_signal(|| None::<TrayManager>);

    // Initialize tray using use_resource to avoid reactive scope warnings
    let _tray_init = use_resource(move || async move {
        match TrayManager::new() {
            Ok(tray) => {
                debug_print!("✅ System tray initialized successfully");
                tray_manager.set(Some(tray));
            }
            Err(e) => {
                always_eprint!("❌ Failed to initialize system tray: {}", e);
            }
        }
    });

    // Act on whatever the pump queued. The task parks between messages, so an
    // idle app - no clicks, no tray menu, no second launch - costs exactly
    // nothing here rather than waking twenty times a second to find out.
    use_effect(move || {
        let window_clone = window.clone();
        let mut tray_manager_clone = tray_manager.clone();
        let audio_ctx = audio_ctx.clone();
        let update_config = update_config.clone();

        spawn(async move {
            loop {
                window_pump::wait().await;

                // Drain everything queued since the last wake: the wake says
                // "there is something", not "there is one thing".
                while let Some(pending) = window_pump::next() {
                    match pending {
                        Pending::WindowAction(action) => match action {
                            WindowAction::Show => {
                                show_window(&window_clone);
                                WINDOW_MANAGER.set_visible(true);
                                crate::always_print!("🔼 Window shown from internal action");
                            }
                            WindowAction::Hide => {
                                window_clone.set_visible(false);
                                WINDOW_MANAGER.set_visible(false);
                                crate::always_print!("🔽 Window hidden from internal action");
                            }
                        },
                        Pending::TrayUpdate => {
                            tray_manager_clone.with_mut(|tray_opt| {
                                if let Some(tray) = tray_opt {
                                    if let Err(e) = tray.update_menu() {
                                        crate::always_eprint!("❌ Failed to update tray menu from global request: {}", e);
                                    } else {
                                        crate::always_print!("✅ Tray menu updated from global request");
                                    }
                                }
                            });
                        }
                        Pending::Tray(tray_message) => match tray_message {
                            crate::libs::tray::TrayMessage::Show => {
                                show_window(&window_clone);
                                WINDOW_MANAGER.set_visible(true);
                                debug_print!("🔼 Window shown from tray");
                            }
                            crate::libs::tray::TrayMessage::ToggleMute => {
                                // Toggle the global sound enable flag. This goes
                                // through the audio context (not a bare config
                                // write) so the engine thread, which caches this
                                // flag in its own state, actually stops playing.
                                let enabled = !audio_ctx.is_sound_enabled();
                                audio_ctx.set_sound_enabled(enabled);
                                // Publish the new flag so the mute button and the
                                // disabled sliders re-render; the audio context
                                // only writes the file.
                                update_config(
                                    Box::new(move |config| {
                                        config.enable_sound = enabled;
                                    })
                                );
                                debug_print!(
                                    "🔇 Sounds {} via tray menu",
                                    if enabled { "enabled" } else { "disabled" }
                                );
                                // Update tray menu to reflect new state
                                tray_manager_clone.with_mut(|tray_opt| {
                                    if let Some(tray) = tray_opt {
                                        if let Err(e) = tray.update_menu() {
                                            always_eprint!("❌ Failed to update tray menu: {}", e);
                                        }
                                    }
                                });
                            }
                            crate::libs::tray::TrayMessage::OpenGitHub => {
                                // Same constant the updater checks, so the
                                // tray cannot link to a different repo than
                                // the one updates actually come from.
                                let url = crate::utils::auto_updater::REPO_URL;
                                if let Err(e) = open::that(url) {
                                    always_eprint!("❌ Failed to open GitHub URL: {}", e);
                                } else {
                                    debug_print!("🐙 Opened GitHub repository in browser");
                                }
                            }
                            crate::libs::tray::TrayMessage::OpenDiscord => {
                                let url = "https://discord.com/invite/MMVrhWxa4w";
                                if let Err(e) = open::that(url) {
                                    crate::always_eprint!("❌ Failed to open Discord URL: {}", e);
                                } else {
                                    crate::always_print!("💬 Opened Discord community in browser");
                                }
                            }
                            crate::libs::tray::TrayMessage::OpenWebsite => {
                                let url = "https://mechvibes.com";
                                if let Err(e) = open::that(url) {
                                    crate::always_eprint!("❌ Failed to open website URL: {}", e);
                                } else {
                                    crate::always_print!("🌐 Opened official website in browser");
                                }
                            }
                            crate::libs::tray::TrayMessage::Exit => {
                                crate::always_print!("📢 Tray: Exit requested - closing application");
                                // Same reason as the titlebar X: the flusher
                                // thread does not survive the process, so a
                                // normal exit has to empty it first.
                                crate::state::config_writer::flush_pending();
                                // Close the window which will trigger app exit
                                window_clone.close();
                            }
                        },
                    }
                }
            }
        });
    });

    rsx! {
        // This component doesn't render anything visible
        span { style: "display: none;" }
    }
}
