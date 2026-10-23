//! Headless mode: the audio engine and the real input capture, with no
//! window, webview or tray.
//!
//! # Why it exists
//!
//! Two audiences. People who want the sounds without a GUI process (a login
//! item, a tiling-WM setup, a machine where the window is just overhead), and
//! people whose WebView2 / webkit2gtk install is broken - for them the window
//! cannot open at all, and this is the difference between a working app and no
//! app.
//!
//! # What it deliberately reuses
//!
//! The input capture is *the same* one the GUI wires, not a simpler
//! substitute. On Windows that means the Raw Input worker process
//! (`input_worker_host`), never the rdev fallback: rdev's `WH_KEYBOARD_LL`
//! hook eats dead keys and delays clicks (see the phase 06 findings), so
//! reaching for it here because it is fewer lines would ship a mode that is
//! quietly worse than the GUI. `bootstrap::start_input_capture` is shared with
//! `main()` for exactly that reason - there is one wiring, and both modes get
//! it.
//!
//! # Config is read, never written
//!
//! Volume, device selection and the enabled-device filter all come from
//! `config.json`, and nothing here writes back to it. That includes the mute
//! hotkey: the engine's own Ctrl+Alt+M handler persists `enable_sound`, so
//! this loop consumes the hotkey *before* the engine sees it and flips a
//! session-local flag instead. A headless run leaves the file byte-identical,
//! which is what makes `--soundpack` safe to use as a one-off without
//! disturbing the GUI's settings.

use crossbeam_channel::{ Receiver, Sender };

use crate::libs::audio::AudioCommand;
use crate::libs::bootstrap;
use crate::libs::cli_args::CliArgs;
use crate::utils::constants::APP_NAME;

/// The decision the hotkey loop makes for one received message.
///
/// Split out from the loop so the mute/unmute sequencing is testable without
/// an audio device or an input worker.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HotkeyOutcome {
    /// Sound was toggled; the payload is the new enabled state.
    Toggled(bool),
    /// A message this loop does not act on.
    Ignored,
}

/// Applies one hotkey message to the session-local mute flag.
///
/// `enabled` is owned by the headless loop rather than by the config file.
/// The engine's `handle_toggle_sound` would persist the flip and notify the
/// tray; neither is wanted here, so this loop never forwards `TOGGLE_SOUND`
/// to the engine's hotkey channel and drives `SetSoundEnabled` directly.
pub(crate) fn apply_hotkey(message: &str, enabled: &mut bool) -> HotkeyOutcome {
    if message != "TOGGLE_SOUND" {
        return HotkeyOutcome::Ignored;
    }
    *enabled = !*enabled;
    HotkeyOutcome::Toggled(*enabled)
}

/// Resolves the soundpacks this run should load.
///
/// Returns `(keyboard, mouse)`, each `Some` only when it differs from what the
/// engine already loaded from config at startup. The engine loads the
/// configured packs itself in `run_engine`, so re-sending an identical id
/// would decode and resample the same audio a second time for nothing.
pub(crate) fn soundpack_overrides(
    args: &CliArgs,
    configured_keyboard: &str,
    configured_mouse: &str
) -> (Option<String>, Option<String>) {
    use crate::libs::cli_args::qualify_soundpack_id;

    let keyboard = args.soundpack
        .as_deref()
        .map(|id| qualify_soundpack_id(id, "keyboard/"))
        .filter(|id| id != configured_keyboard);

    let mouse = args.mouse_soundpack
        .as_deref()
        .map(|id| qualify_soundpack_id(id, "mouse/"))
        .filter(|id| id != configured_mouse);

    (keyboard, mouse)
}

/// Runs headless until the process is interrupted.
///
/// Returns only when the input channels close, which in practice means the
/// engine thread died - Ctrl+C terminates the process rather than unwinding
/// to here.
pub fn run(args: &CliArgs) {
    // Read once. `config_writer::current()` is the same authority the GUI
    // reads, so volume, audio device and the per-device input filter are
    // whatever the user last set in the window.
    let config = crate::state::config_writer::current();

    let (keyboard_tx, keyboard_rx) = crossbeam_channel::unbounded::<String>();
    let (mouse_tx, mouse_rx) = crossbeam_channel::unbounded::<String>();
    // The engine gets a hotkey receiver it will never hear from: this loop
    // owns the hotkey channel so it can mute without writing config. Handing
    // the engine the real receiver would let its persisting handler run.
    let (engine_hotkey_tx, engine_hotkey_rx) = crossbeam_channel::unbounded::<String>();
    let (hotkey_tx, hotkey_rx) = crossbeam_channel::unbounded::<String>();

    crate::always_print!("🔊 {} headless mode", APP_NAME);

    // Spawned before the listeners so no keystroke can arrive with nothing
    // to receive it. The engine loads the configured packs itself.
    let engine = crate::libs::audio::spawn_engine(keyboard_rx, mouse_rx, engine_hotkey_rx);

    let (keyboard_override, mouse_override) = soundpack_overrides(
        args,
        &config.keyboard_soundpack,
        &config.mouse_soundpack
    );

    // `update_cache_on_error: false` - a bad id passed on the command line is
    // this run's problem and must not write a failure entry into the shared
    // soundpack cache the GUI reads.
    if let Some(soundpack_id) = &keyboard_override {
        crate::always_print!("🎹 Keyboard soundpack (this run only): {}", soundpack_id);
        engine.send(AudioCommand::LoadKeyboardPack {
            soundpack_id: soundpack_id.clone(),
            update_cache_on_error: false,
        });
    } else {
        crate::always_print!("🎹 Keyboard soundpack: {}", config.keyboard_soundpack);
    }

    if let Some(soundpack_id) = &mouse_override {
        crate::always_print!("🖱️ Mouse soundpack (this run only): {}", soundpack_id);
        engine.send(AudioCommand::LoadMousePack {
            soundpack_id: soundpack_id.clone(),
            update_cache_on_error: false,
        });
    } else {
        crate::always_print!("🖱️ Mouse soundpack: {}", config.mouse_soundpack);
    }

    crate::always_print!(
        "🔈 Volume: {:.0}% keyboard, {:.0}% mouse",
        config.volume * 100.0,
        config.mouse_volume * 100.0
    );

    // The same capture the GUI starts. On Windows this spawns the Raw Input
    // worker process; there is no window here, so nothing competes for the
    // registration, but the worker is still the right path - it is the one
    // without the dead-key and click-delay defects of the rdev hook.
    bootstrap::start_input_capture(keyboard_tx, mouse_tx, hotkey_tx);

    if !config.enable_sound {
        crate::always_print!("🔇 Sound is muted in settings - press Ctrl+Alt+M to unmute");
    }
    crate::always_print!("✅ Listening. Ctrl+Alt+M mutes, Ctrl+C exits.");

    run_hotkey_loop(&hotkey_rx, engine_hotkey_tx, config.enable_sound, &engine);
}

/// Blocks on the hotkey channel for the rest of the run.
///
/// A blocking `recv()`, not a poll: this thread has nothing else to do, and a
/// sleep loop would burn a wakeup per interval forever on what is meant to be
/// a background process.
///
/// `engine_hotkey_tx` is taken by value and never sent on. It exists only to
/// keep the engine's hotkey receiver open for the life of the run: were it
/// dropped, that arm of the engine's `select!` would go permanently ready with
/// a disconnect error. Owning it here ties its lifetime to the loop's, which
/// is the invariant, rather than leaving it to a comment.
fn run_hotkey_loop(
    hotkey_rx: &Receiver<String>,
    engine_hotkey_tx: Sender<String>,
    initial_enabled: bool,
    engine: &crate::libs::audio::engine::AudioEngineHandle
) {
    let mut enabled = initial_enabled;

    while let Ok(message) = hotkey_rx.recv() {
        if let HotkeyOutcome::Toggled(now_enabled) = apply_hotkey(&message, &mut enabled) {
            // Session-local: the engine's cached flag moves, the file does not.
            engine.send(AudioCommand::SetSoundEnabled(now_enabled));
            crate::always_print!(
                "{} Sound {} (this session only)",
                if now_enabled { "🔊" } else { "🔇" },
                if now_enabled { "on" } else { "off" }
            );
        }
    }

    // Only reachable once every input listener has hung up.
    drop(engine_hotkey_tx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hotkey_toggles_and_toggles_back() {
        let mut enabled = true;
        assert_eq!(apply_hotkey("TOGGLE_SOUND", &mut enabled), HotkeyOutcome::Toggled(false));
        assert!(!enabled);
        assert_eq!(apply_hotkey("TOGGLE_SOUND", &mut enabled), HotkeyOutcome::Toggled(true));
        assert!(enabled);
    }

    #[test]
    fn a_run_started_muted_unmutes_on_the_first_press() {
        // Starting from the config's `enable_sound: false` rather than from a
        // hardcoded `true` is what makes the first Ctrl+Alt+M do the audible
        // thing instead of muting an already-silent session.
        let mut enabled = false;
        assert_eq!(apply_hotkey("TOGGLE_SOUND", &mut enabled), HotkeyOutcome::Toggled(true));
    }

    #[test]
    fn unrelated_messages_leave_the_flag_alone() {
        let mut enabled = true;
        assert_eq!(apply_hotkey("SOMETHING_ELSE", &mut enabled), HotkeyOutcome::Ignored);
        assert!(enabled, "an unknown message must not mute the session");
    }

    fn args_with(soundpack: Option<&str>, mouse: Option<&str>) -> CliArgs {
        CliArgs {
            headless: true,
            minimized: false,
            soundpack: soundpack.map(String::from),
            mouse_soundpack: mouse.map(String::from),
        }
    }

    #[test]
    fn no_flag_means_no_override() {
        let (keyboard, mouse) = soundpack_overrides(
            &args_with(None, None),
            "keyboard/eg-oreo",
            "mouse/ping"
        );
        assert_eq!(keyboard, None, "the engine already loaded the configured pack");
        assert_eq!(mouse, None);
    }

    #[test]
    fn a_bare_name_is_qualified_before_it_reaches_the_loader() {
        let (keyboard, _) = soundpack_overrides(
            &args_with(Some("cherrymx-black-abs"), None),
            "keyboard/eg-oreo",
            "mouse/ping"
        );
        assert_eq!(keyboard.as_deref(), Some("keyboard/cherrymx-black-abs"));
    }

    #[test]
    fn an_override_equal_to_the_configured_pack_is_dropped() {
        // Re-sending it would make the engine decode and resample the very
        // audio it loaded a moment earlier at startup.
        let (keyboard, mouse) = soundpack_overrides(
            &args_with(Some("eg-oreo"), Some("ping")),
            "keyboard/eg-oreo",
            "mouse/ping"
        );
        assert_eq!(keyboard, None);
        assert_eq!(mouse, None);
    }

    #[test]
    fn the_mouse_override_defaults_to_the_mouse_prefix() {
        let (_, mouse) = soundpack_overrides(
            &args_with(None, Some("chat")),
            "keyboard/eg-oreo",
            "mouse/ping"
        );
        assert_eq!(mouse.as_deref(), Some("mouse/chat"));
    }

    #[test]
    fn a_fully_qualified_override_crosses_slots_untouched() {
        // Passing `mouse/...` to --soundpack is a user error the loader
        // reports ("This is a mouse soundpack"). Rewriting it into
        // `keyboard/mouse/...` would turn that clear message into a
        // file-not-found.
        let (keyboard, _) = soundpack_overrides(
            &args_with(Some("mouse/ping"), None),
            "keyboard/eg-oreo",
            "mouse/ping"
        );
        assert_eq!(keyboard.as_deref(), Some("mouse/ping"));
    }

    /// The guarantee the whole mode rests on: `headless.rs` must contain no
    /// path that writes the config file.
    ///
    /// Checked against the source rather than by running a headless session,
    /// because the run loop needs an audio device and an input worker. A
    /// reviewer adding a `config_writer::apply` here - the natural way to make
    /// the mute hotkey "stick" - would silently break the promise that a
    /// `--soundpack` run leaves the GUI's settings alone.
    #[test]
    fn headless_never_writes_config() {
        let runtime = runtime_code();

        for writer in ["config_writer::apply", ".save()", "handle_toggle_sound"] {
            assert!(
                !runtime.contains(writer),
                "{writer} writes settings - headless mode must leave config.json untouched"
            );
        }
        assert!(
            runtime.contains("config_writer::current"),
            "config is still read, just never written"
        );
    }

    /// This module's executable code, with comments stripped.
    ///
    /// The doc comments above deliberately name the very APIs the guards below
    /// forbid, in order to explain why they are forbidden. Scanning the raw
    /// source would match that prose and fail on a file that is entirely
    /// correct, so the checks run against code only.
    fn runtime_code() -> String {
        const SOURCE: &str = include_str!("headless.rs");
        SOURCE.split("#[cfg(test)]")
            .next()
            .expect("runtime code precedes tests")
            .lines()
            .map(|line| {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    return "";
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The hotkey must not reach the engine's own handler, which persists.
    #[test]
    fn the_engines_persisting_hotkey_path_is_bypassed() {
        let runtime = runtime_code();

        // The engine is given a separate receiver, and TOGGLE_SOUND is never
        // forwarded onto it.
        assert!(runtime.contains("engine_hotkey_rx"), "the engine gets its own idle receiver");
        assert!(
            runtime.contains("AudioCommand::SetSoundEnabled"),
            "mute goes through the non-persisting command instead"
        );
    }

    /// No polling: the mode is meant to idle at zero CPU.
    #[test]
    fn the_run_loop_blocks_rather_than_polling() {
        let runtime = runtime_code();

        assert!(runtime.contains("hotkey_rx.recv()"), "the loop must block on recv");
        for busy in ["try_recv", "thread::sleep", "sleep(Duration"] {
            assert!(!runtime.contains(busy), "{busy} would turn an idle process into a poller");
        }
    }
}
