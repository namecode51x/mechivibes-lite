//! The one place `config.json` is written.
//!
//! # Why this module exists
//!
//! `AppConfig::save()` serialises the *whole* struct. Any writer that read a
//! config, did something slow, and then saved that copy silently reverted every
//! field another subsystem changed in between. That defect shipped three times
//! from three unrelated call sites (the startup update check, the long-lived
//! `UpdateService` snapshot, and a debounced volume write), and each was patched
//! on its own. Patching writers one at a time cannot work: the mistake is
//! available to every new writer by default.
//!
//! So the *shape* is what changed. `save()` is no longer reachable from outside
//! `crate::state`; the only way to write is [`apply`], which takes a mutation
//! and runs it against state this module owns. Holding an `AppConfig` across
//! time and writing it back is not a discouraged pattern here - there is no API
//! that accepts one, so it cannot be expressed.
//!
//! # What a mutation may assume
//!
//! The closure gets `&mut AppConfig` holding the newest known state, and should
//! touch only the fields it is responsible for. Two subsystems mutating
//! different fields compose: each runs against the result of the other, so
//! neither can revert the other's work. That is the merge semantics the old
//! read-modify-save pattern lacked.
//!
//! # Threading
//!
//! Writers live on the Dioxus UI thread, the audio engine thread, tokio tasks
//! and the tray callback. A `Mutex` authority rather than a reply-based channel
//! is deliberate: every existing writer is synchronous and several read the
//! result back immediately (the Ctrl+Alt+M handler needs the toggled value to
//! move the engine's own cached flag in the same call). Contention is nil in
//! practice - writes are user-driven, not per-keystroke.
//!
//! # Coalesced disk writes
//!
//! `apply` mutates the in-memory authority and returns; the *disk* write is
//! done by a background flusher that waits [`FLUSH_DEBOUNCE`] for the burst to
//! end. The authority is authoritative either way - a read after `apply`
//! always sees the new value - only the file lags by at most one debounce.
//!
//! This exists because `save()` serialises the whole struct and does a
//! temp-file write plus a rename: doing that per slider tick put ~60
//! synchronous writes on the UI thread during a single drag (about 15% of one
//! core) and, since the authority `Mutex` was held across the I/O, blocked
//! every concurrent `current()` while it ran. Neither happens now: `apply`
//! touches no I/O and releases the authority before scheduling.
//!
//! Two rules make the deferral safe:
//!
//! * Nothing holds the authority across I/O. The flusher clones a snapshot
//!   under the lock and writes outside it.
//! * Every write path takes [`WRITE_LOCK`] first, so two writers can never
//!   race and land out of order - an older snapshot overwriting a newer one
//!   would be silent data loss.
//!
//! The cost is a window of at most one debounce in which a *hard* kill (not a
//! normal exit) drops the last change. Every exit path this app controls -
//! the titlebar X, the tray's Exit, and the update-install button, which all
//! end in `window.close()` - calls [`flush_pending`] first, so a normal close
//! never loses anything.

use crate::state::config::AppConfig;
use std::sync::atomic::{ AtomicU64, Ordering };
use std::sync::{ Condvar, Mutex, OnceLock };
use std::time::{ Duration, Instant };

/// How long the flusher waits for a change burst to end before touching disk.
///
/// Long enough that a slider drag collapses to one write instead of one per
/// tick, short enough that "change something, close the app" cannot outrun it
/// even without the explicit [`flush_pending`] on the exit paths.
const FLUSH_DEBOUNCE: Duration = Duration::from_millis(150);

/// Serialises actual file writes.
///
/// Separate from the authority `Mutex` on purpose: the flusher and
/// [`flush_pending`] can both be asked to write the same change, and without
/// this ordering them, whichever snapshotted first would land last and undo
/// the other. Taking this means "I hold the right to write, and I will
/// re-check that the file is still behind before doing so".
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Writes that have actually been issued, for tests that prove coalescing.
static WRITES: AtomicU64 = AtomicU64::new(0);

/// The flusher's schedule. `deadline` doubles as the dirty flag: `Some` means
/// the file does not yet hold everything the authority does.
///
/// Invariant, and the whole reason [`WRITE_LOCK`] exists: **while
/// `WRITE_LOCK` is held, `deadline.is_some()` is exactly equivalent to "the
/// file is behind"**. A writer therefore clears the deadline and writes in the
/// same critical section, so a caller arriving later either waits for a write
/// that is genuinely in flight or finds the deadline already cleared by one
/// that finished. It can never conclude "nothing to do" while a write is
/// still owed.
struct FlushState {
    deadline: Option<Instant>,
}

static FLUSHER: OnceLock<(Mutex<FlushState>, Condvar)> = OnceLock::new();
/// The flusher thread is spawned once, after `FLUSHER` is populated, so the
/// thread can take its reference from the static instead of racing init.
static FLUSHER_THREAD: OnceLock<()> = OnceLock::new();

fn flusher() -> &'static (Mutex<FlushState>, Condvar) {
    FLUSHER.get_or_init(|| (Mutex::new(FlushState { deadline: None }), Condvar::new()))
}

fn ensure_flusher_thread() {
    flusher();
    FLUSHER_THREAD.get_or_init(|| {
        std::thread::Builder::new()
            .name("config-flusher".into())
            .spawn(flusher_main)
            .expect("config flusher thread must start");
    });
}

fn lock_state<'a>(
    pair: &'a (Mutex<FlushState>, Condvar),
) -> std::sync::MutexGuard<'a, FlushState> {
    // Same recovery rule as the authority: a panicking writer leaves a valid
    // struct behind, and refusing to schedule would wedge the app on its first
    // panic.
    match pair.0.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The authoritative config. Loaded from disk once, on first use, and from then
/// on this - not the file - is what a mutation is applied to.
///
/// Re-reading the file before every mutation would also be correct, since this
/// process is its only writer, but it would put a synchronous parse on the UI
/// thread for every slider tick. Owning the state instead keeps the disk read
/// to one.
/// Nothing reachable from `AppConfig::load()` may call back into this module.
/// `load()` runs during the `get_or_init` below, so a re-entrant `current()`
/// would deadlock on the initialising `OnceLock`. The one thing `load()` calls
/// out to is `auto_startup::get_auto_startup_state`, which reads the Windows
/// registry (or, on macOS, the login-item service) and never touches the config - keep it that way.
static AUTHORITY: OnceLock<Mutex<AppConfig>> = OnceLock::new();

/// Bumped on every write that actually changed something.
///
/// This is how a mutation made off the UI thread reaches the window. A callback
/// cannot do it: the shared `Signal<AppConfig>` uses Dioxus's default
/// `UnsyncStorage` and is therefore not `Send`, so no closure registered here
/// could legally touch it. A counter can be read from anywhere, and the UI's
/// event loop (`libs/ui.rs`, the one already draining `UiEvent`s) compares it
/// once per wake and republishes when it moved - with `libs::wake` announcing
/// the write, so no polling is involved.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn authority() -> &'static Mutex<AppConfig> {
    AUTHORITY.get_or_init(|| Mutex::new(AppConfig::load()))
}

/// A snapshot of the current config.
///
/// Read-only by construction: writing it back is impossible, because no
/// function in this crate accepts an `AppConfig` to persist. Use it to read,
/// then use [`apply`] to change something.
pub fn current() -> AppConfig {
    // A poisoned lock means a mutation closure panicked mid-write. The config
    // is still a valid struct (the panic happened between field assignments at
    // worst), and refusing to serve settings would take the app down with it,
    // so the value is recovered rather than propagated.
    match authority().lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Apply `mutate` to the authoritative config and schedule a disk write.
///
/// The mutation sees the newest state, so a concurrent change from another
/// subsystem is already present and survives. Returns whether anything
/// changed.
///
/// A mutation that leaves the data unchanged is not persisted. Mount effects
/// re-assert the value they already hold on every navigation, and writing those
/// unconditionally turned navigation into a continuous rewrite of the file.
///
/// The authority `Mutex` is released before the write is scheduled, so no
/// caller ever holds it across I/O - which is what keeps `current()` cheap on
/// the UI thread during a drag.
pub fn apply(mutate: impl FnOnce(&mut AppConfig)) -> bool {
    let changed = {
        let mut guard = match authority().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let before = guard.clone();
        mutate(&mut guard);

        let changed = !guard.data_equals(&before);
        if changed {
            guard.last_updated = chrono::Utc::now();
        } else {
            // Undo a metadata-only edit so the in-memory struct cannot drift
            // from the file - `data_equals` ignores those fields, so they would
            // otherwise never be written.
            *guard = before;
        }

        changed
    };

    if changed {
        // Release ordering pairs with the `Acquire` load in `generation`: a
        // reader that sees the new number is guaranteed to see the write that
        // produced it, so it cannot republish a config older than the bump it
        // reacted to.
        GENERATION.fetch_add(1, Ordering::Release);
        // ...and this is what tells the window to go look. The counter alone
        // would only be noticed by a poll loop, and the app no longer has one:
        // a write made on the engine thread or in a tokio task parks the UI
        // until it is announced (see `libs/wake.rs`).
        crate::libs::wake::wake_ui();
        // UI first, disk second: the window must show the new value even if
        // the flusher has not run yet, and it will not - that is the point.
        schedule_flush();
    }

    changed
}

/// Tells the flusher the file is behind and when to catch up.
///
/// Pushes the deadline forward rather than replacing it in one place only:
/// while changes keep arriving the deadline keeps moving, so the write lands
/// once the burst ends.
fn schedule_flush() {
    ensure_flusher_thread();
    let pair = flusher();
    {
        let mut state = lock_state(pair);
        let now = Instant::now();
        state.deadline = Some(match state.deadline {
            // Never move the deadline backwards: that would let a slow write
            // be delayed again by an unrelated change and stretch the window
            // in which the file is stale.
            Some(existing) if existing > now + FLUSH_DEBOUNCE => existing,
            _ => now + FLUSH_DEBOUNCE,
        });
    }
    pair.1.notify_one();
}

/// How many times the config has changed since the process started.
///
/// The UI reads this after each wake and republishes [`current`] into the
/// shared signal when it moves, which is what lets a write made on the audio
/// engine thread (Ctrl+Alt+M) or in a tokio task re-render the window. Before
/// this existed, the hotkey muted the app while the window kept rendering the
/// unmuted icon until some unrelated UI write happened to republish.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Forces any pending change onto disk, then the bytes onto the platter.
///
/// Call this on exit paths and before anything that must survive a hard
/// restart of the machine.
///
/// `apply` writes through `save_json_to_file_atomically`, which writes a temp
/// file and renames it over the target but never syncs, so the new bytes can sit
/// in the OS cache for a while. That is fine for a slider, but not for the
/// start-at-login preferences: they are read on the very next launch, which is
/// often right after a logout or reboot.
///
/// - The file is synced on every OS. It is opened for write (without
///   truncating) because Windows' `FlushFileBuffers` needs a writable handle.
///   On macOS `File::sync_all` issues `F_FULLFSYNC`, which is what actually
///   reaches the disk.
/// - The parent directory is synced too, on Unix only, because that is where
///   the rename lives. A directory cannot be opened this way on Windows.
///
/// Best effort: a failure is logged, never fatal.
pub fn flush_to_disk() {
    // The sync below would otherwise be pointless: with coalescing, the file
    // may not hold the value being asked to survive a reboot yet.
    flush_pending();

    let path = crate::state::paths::data::config_json();
    if let Err(e) = flush_path(&path) {
        crate::always_eprint!("❌ [config] Failed to flush {} to disk: {}", path.display(), e);
    }
}

/// Writes a scheduled change to disk right now, without syncing.
///
/// This is what every exit path calls: without it a normal close inside
/// [`FLUSH_DEBOUNCE`] of a change would drop it, because the flusher thread is
/// killed with the process.
///
/// Returns whether anything was actually written - `false` means the file
/// already held everything the authority did.
pub fn flush_pending() -> bool {
    // Lock order: WRITE_LOCK before the flusher lock, everywhere.
    let _write = match WRITE_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };

    let pair = flusher();
    // Re-check under the write lock: the flusher may have already caught up
    // while this caller was queued behind it, and writing a snapshot nobody
    // asked for would be churn rather than a fix.
    {
        let mut state = lock_state(pair);
        if state.deadline.is_none() {
            return false;
        }
        state.deadline = None;
    }

    write_snapshot();
    true
}

/// How many writes have actually reached `config.json` this process.
///
/// Only read by tests: it is what proves the debounce collapses a burst of
/// changes into a single write instead of trusting the timing.
///
/// Not `#[cfg(test)]` because it counts in every build - one relaxed atomic
/// per write, and writes are now rare by design. Marked `allow(dead_code)` for
/// the non-test builds where nothing reads it.
#[allow(dead_code)]
fn write_count() -> u64 {
    WRITES.load(Ordering::Acquire)
}

/// Serialises the current authority over `config.json`.
///
/// Must be called with [`WRITE_LOCK`] held and the authority *not* held: the
/// snapshot is what decides the content, and taking the authority only to read
/// it means a concurrent `current()` is never blocked by the file I/O.
fn write_snapshot() {
    let snapshot = match authority().lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };

    WRITES.fetch_add(1, Ordering::AcqRel);
    if let Err(e) = snapshot.save() {
        crate::always_eprint!("❌ [config] Failed to save config: {}", e);
    }
}

/// The background writer: sleeps until the deadline, then catches the file up.
///
/// Ordering is `flusher lock -> (released) -> WRITE_LOCK -> flusher lock`,
/// which is the same order every other entry point uses (`flush_pending` takes
/// `WRITE_LOCK` first too). Nothing ever waits for `WRITE_LOCK` while holding
/// the flusher lock, so the two cannot deadlock.
fn flusher_main() {
    let pair = flusher();

    loop {
        // Sleep until there is something to write and it is due.
        {
            let mut state = lock_state(pair);
            loop {
                match state.deadline {
                    // Nothing pending: sleep until something is scheduled.
                    None => {
                        state = match pair.1.wait(state) {
                            Ok(guard) => guard,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                    }
                    Some(deadline) => {
                        let now = Instant::now();
                        if now >= deadline {
                            break;
                        }
                        let (guard, _) = match pair.1.wait_timeout(state, deadline - now) {
                            Ok(v) => v,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        state = guard;
                    }
                }
            }
        }

        let _write = match WRITE_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        // Re-check under the write lock: `flush_pending` may have already
        // caught up while this thread was queued behind it.
        let owed = {
            let mut state = lock_state(pair);
            match state.deadline.take() {
                Some(_) => true,
                None => false,
            }
        };
        if owed {
            write_snapshot();
        }
        // `WRITE_LOCK` released here, at the end of the iteration.
    }
}

fn flush_path(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .and_then(|_| sync_parent_dir(path))
}

#[cfg(unix)]
fn sync_parent_dir(path: &std::path::Path) -> std::io::Result<()> {
    match path.parent() {
        Some(dir) => std::fs::File::open(dir)?.sync_all(),
        None => Ok(()),
    }
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_path_syncs_an_existing_file_without_changing_it() {
        let path = std::env::temp_dir().join(format!("mv-flush-test-{}.json", std::process::id()));
        std::fs::write(&path, b"{\"a\":1}").unwrap();
        assert!(flush_path(&path).is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"a\":1}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn flush_path_reports_a_missing_file_instead_of_creating_it() {
        let path = std::env::temp_dir().join(format!("mv-flush-missing-{}.json", std::process::id()));
        assert!(flush_path(&path).is_err());
        assert!(!path.exists());
    }
    use crate::libs::theme::{ BuiltInTheme, Theme };

    /// The authority is process-global and backed by the real config file, so
    /// these tests drive an equivalent local one. What is being proven is the
    /// merge rule - a mutation runs against the newest state - which is a
    /// property of `apply`'s body, not of where the state is stored.
    struct Authority {
        config: AppConfig,
        writes: usize,
    }

    impl Authority {
        fn new() -> Self {
            Self { config: AppConfig::default(), writes: 0 }
        }

        /// Mirrors `apply`: mutate the live state, persist only a real change.
        fn apply(&mut self, mutate: impl FnOnce(&mut AppConfig)) -> bool {
            let before = self.config.clone();
            mutate(&mut self.config);
            let changed = !self.config.data_equals(&before);
            if changed {
                self.writes += 1;
            } else {
                self.config = before;
            }
            changed
        }
    }

    /// The composition property the whole refactor exists for: two subsystems
    /// mutating different fields must both survive, in either order, with no
    /// re-read discipline required of either caller.
    #[test]
    fn mutations_from_different_subsystems_compose() {
        let mut authority = Authority::new();

        // The UI thread changes the theme.
        authority.apply(|config| {
            config.theme = Theme::BuiltIn(BuiltInTheme::Dark);
        });
        // The audio engine thread mutes, knowing nothing about the theme.
        authority.apply(|config| {
            config.enable_sound = false;
        });
        // A tokio task records an update check, knowing nothing about either.
        authority.apply(|config| {
            config.auto_update.last_check = Some(1_700_000_000);
        });

        assert_eq!(
            authority.config.theme,
            Theme::BuiltIn(BuiltInTheme::Dark),
            "the UI's field must survive the two later writers"
        );
        assert!(!authority.config.enable_sound, "the engine's field must survive the tokio write");
        assert_eq!(
            authority.config.auto_update.last_check,
            Some(1_700_000_000),
            "and the tokio task's own field must be recorded"
        );
    }

    /// The startup-update-check shape: a writer that begins its work, is slow
    /// (a network round trip), and finishes after the user has changed
    /// something. Under the old read-modify-save API the pre-request struct was
    /// written back. A mutation cannot carry state across that gap, so the late
    /// write touches only its own field.
    #[test]
    fn a_slow_writer_finishing_late_does_not_revert_a_concurrent_change() {
        let mut authority = Authority::new();

        // The check starts here. All it may carry across the await is the value
        // it intends to write - there is no config to hold.
        let recorded_check_time = 1_700_000_000;

        // While the request is in flight the user drags the volume slider and
        // picks a theme.
        authority.apply(|config| {
            config.volume = 0.35;
        });
        authority.apply(|config| {
            config.theme = Theme::BuiltIn(BuiltInTheme::Dark);
        });

        // The request returns and the check writes its result.
        authority.apply(|config| {
            config.auto_update.last_check = Some(recorded_check_time);
        });

        assert_eq!(authority.config.volume, 0.35, "the volume set during the check must survive");
        assert_eq!(
            authority.config.theme,
            Theme::BuiltIn(BuiltInTheme::Dark),
            "and so must the theme"
        );
        assert_eq!(
            authority.config.auto_update.last_check,
            Some(recorded_check_time),
            "while the check still records its own result"
        );
    }

    /// The `UpdateService` shape: a writer that lives for the whole session and
    /// writes repeatedly. Its first write must not restore the state the app
    /// launched with, and neither must its tenth.
    #[test]
    fn a_long_lived_writer_does_not_restore_launch_state() {
        let mut authority = Authority::new();
        let launch_volume = authority.config.volume;

        // The service starts at launch. It captures no config.
        // The user immediately changes the volume.
        authority.apply(|config| {
            config.volume = 0.88;
        });
        assert_ne!(launch_volume, 0.88, "the test must actually move the value");

        // Every periodic tick for the rest of the session.
        for tick in 0..10 {
            authority.apply(|config| {
                config.auto_update.last_check = Some(1_700_000_000 + tick);
            });
            assert_eq!(
                authority.config.volume,
                0.88,
                "tick {tick} must not restore the launch-time volume"
            );
        }
    }

    /// The debounce shape: a deferred write that lands after the user has moved
    /// on. It may only carry the value it means to write, so whatever else
    /// changed meanwhile is untouched.
    #[test]
    fn a_deferred_write_landing_late_touches_only_its_own_field() {
        let mut authority = Authority::new();

        // Slider drag: the value is published now, and a deferred write of the
        // same value is scheduled.
        let debounced_volume = 0.2;
        authority.apply(|config| {
            config.volume = debounced_volume;
        });

        // The user navigates away and mutes before the timer fires.
        authority.apply(|config| {
            config.enable_sound = false;
        });

        // The deferred write finally lands.
        let wrote = authority.apply(|config| {
            config.volume = debounced_volume;
        });

        assert!(!wrote, "re-writing an already-current value must not touch the disk");
        assert!(!authority.config.enable_sound, "and must not revert the mute");
        assert_eq!(authority.config.volume, debounced_volume, "while the volume still holds");
    }

    /// Disk churn is a real regression risk: mount effects re-assert their
    /// current value on every navigation. Only genuine changes may reach the
    /// file.
    #[test]
    fn re_asserting_the_current_value_writes_nothing() {
        let mut authority = Authority::new();
        let current_volume = authority.config.volume;

        for _ in 0..20 {
            let wrote = authority.apply(|config| {
                config.volume = current_volume;
            });
            assert!(!wrote, "a no-op mutation must not be persisted");
        }

        assert_eq!(authority.writes, 0, "twenty navigations must produce zero writes");

        let wrote = authority.apply(|config| {
            config.volume = current_volume + 0.25;
        });
        assert!(wrote, "a real change must still be written");
        assert_eq!(authority.writes, 1, "exactly one write for one real change");
    }

    /// Serialises the tests that drive the *real* authority.
    ///
    /// They each snapshot the config, mutate it, and restore it. The authority
    /// is process-global and the test harness runs threads in parallel, so two
    /// of them overlapping would have one restore its snapshot over the other's
    /// assertions - a contention between the tests, not a defect in `apply`.
    static REAL_AUTHORITY_TESTS: Mutex<()> = Mutex::new(());

    fn lock_real_authority() -> std::sync::MutexGuard<'static, ()> {
        match REAL_AUTHORITY_TESTS.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Drives the **real** `apply` from several OS threads at once - the real
    /// `Mutex` authority, the real atomic write, the real file - rather than
    /// the local model above.
    ///
    /// Every writer in the app lives on one of these: the Dioxus UI thread, the
    /// audio engine thread, a tokio task, or the tray callback. Each is a plain
    /// OS thread here mutating the one field that subsystem owns. Under the old
    /// read-modify-save pattern, whichever thread finished last would restore
    /// the fields the others had written; all of them must now survive.
    ///
    /// The config is captured before and restored after, so running the suite
    /// leaves the developer's settings exactly as they were.
    #[test]
    fn real_writers_on_different_threads_all_survive() {
        use std::sync::{ Arc, Barrier };

        let _serialised = lock_real_authority();
        let original = current();

        let threads = 8;
        let iterations = 40;
        let barrier = Arc::new(Barrier::new(threads));

        let handles: Vec<_> = (0..threads)
            .map(|id| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    // Start together so the writes genuinely overlap rather
                    // than queueing up behind one another.
                    barrier.wait();

                    for round in 0..iterations {
                        match id {
                            0 =>
                                apply(|config| {
                                    config.volume = 0.5;
                                }),
                            1 =>
                                apply(|config| {
                                    config.mouse_volume = 0.25;
                                }),
                            2 =>
                                apply(|config| {
                                    config.enable_sound = false;
                                }),
                            3 =>
                                apply(|config| {
                                    config.landscape_mode = true;
                                }),
                            4 =>
                                apply(|config| {
                                    config.auto_update.last_check = Some(1_700_000_000 + round);
                                }),
                            5 =>
                                apply(|config| {
                                    config.custom_css = "body{}".to_string();
                                }),
                            6 =>
                                apply(|config| {
                                    config.enable_volume_boost = true;
                                }),
                            _ =>
                                apply(|config| {
                                    config.start_minimized = true;
                                }),
                        };
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("no writer thread may panic");
        }

        let final_config = current();

        // Restore before asserting, so a failure still leaves the machine
        // clean.
        apply(|config| {
            *config = original;
        });

        assert_eq!(final_config.volume, 0.5, "the volume writer's field must survive");
        assert_eq!(final_config.mouse_volume, 0.25, "and the mouse volume writer's");
        assert!(!final_config.enable_sound, "and the mute writer's");
        assert!(final_config.landscape_mode, "and the layout writer's");
        assert_eq!(
            final_config.auto_update.last_check,
            Some(1_700_000_000 + (iterations - 1)),
            "and the update checker's"
        );
        assert_eq!(final_config.custom_css, "body{}", "and the customization writer's");
        assert!(final_config.enable_volume_boost, "and the volume-boost writer's");
        assert!(final_config.start_minimized, "and the startup writer's");
    }

    /// Every write goes through a temp file and a rename, so a reader that
    /// catches the file mid-save sees either the old document or the new one -
    /// never a truncated one. A truncated `config.json` is what routes the load
    /// path into the `.corrupt` rescue, costing the user every setting.
    ///
    /// `flush_pending` is driven explicitly here rather than waiting for the
    /// debounce: the point of the test is that a write and a read race, and a
    /// timing-dependent wait would turn that into a test that sometimes races
    /// and sometimes does nothing at all.
    #[test]
    fn a_concurrent_reader_never_observes_a_partial_document() {
        use std::sync::atomic::{ AtomicBool, Ordering as AtomicOrdering };
        use std::sync::Arc;

        let _serialised = lock_real_authority();
        let original = current();
        let path = crate::state::paths::data::config_json();
        let stop = Arc::new(AtomicBool::new(false));

        // Make sure the file exists and is current before the reader starts,
        // otherwise a first run would spend the whole test reading a file that
        // is not there yet and assert nothing.
        apply(|config| {
            config.auto_update.last_check = Some(1_700_000_000);
        });
        flush_pending();

        // The reader announces its first parseable document, and the write
        // burst waits for that signal. Without it the two phases overlap by
        // scheduling alone: on a busy machine the writer can finish all 40
        // flushes before the reader has done a single read, and the test then
        // reports "no document was ever observed" - a fact about when threads
        // happened to run, not about the writes it is supposed to judge.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        let reader = {
            let stop = Arc::clone(&stop);
            let path = path.clone();
            std::thread::spawn(move || {
                let mut partial_reads = 0usize;
                let mut successful_reads = 0usize;
                let mut unreadable = 0usize;
                while !stop.load(AtomicOrdering::Relaxed) {
                    // A missing file is not a half-written one, so it is
                    // counted apart: a torn write must be proven with reads
                    // that reached a document, never with reads that never
                    // found one.
                    match std::fs::read_to_string(&path) {
                        Ok(contents) => {
                            if crate::state::config::parse_lenient(&contents).is_ok() {
                                if successful_reads == 0 {
                                    let _ = ready_tx.send(());
                                }
                                successful_reads += 1;
                            } else {
                                partial_reads += 1;
                            }
                        }
                        Err(_) => unreadable += 1,
                    }
                }
                (successful_reads, partial_reads, unreadable)
            })
        };

        let reader_was_reading = ready_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();

        for round in 0..200 {
            apply(|config| {
                config.auto_update.last_check = Some(1_700_000_000 + round);
            });
            // Every fifth round, force the write the flusher would otherwise
            // do after the debounce, so the reader genuinely overlaps ~40
            // writes instead of racing a file nobody touches.
            if round % 5 == 0 {
                flush_pending();
            }
        }

        stop.store(true, AtomicOrdering::Relaxed);
        let (successful_reads, partial_reads, unreadable) =
            reader.join().expect("reader must not panic");

        flush_pending();
        apply(|config| {
            *config = original;
        });
        flush_pending();

        // Everything the other two cannot separate: a file that was never
        // there, one that never parsed, and one that parsed all along are
        // three different failures with three different causes, and reporting
        // them one message at a time hid which one happened.
        assert!(
            reader_was_reading && successful_reads > 0,
            "the reader must actually have observed the file: {successful_reads} reads parsed, \
             {partial_reads} failed to parse, {unreadable} could not be opened, ready signal \
             received: {reader_was_reading} - {} exists now ({})",
            path.exists(),
            path.display()
        );
        assert_eq!(
            partial_reads,
            0,
            "a half-written config was observed {partial_reads} times - \
             the write is not atomic ({unreadable} reads could not open the file, path {})",
            path.display()
        );
    }

    /// The regression this whole change exists to prevent: a burst of changes
    /// must collapse into a single disk write.
    ///
    /// Before coalescing, one write happened per `apply` - measured at 60
    /// writes for a 60-step slider drag, each a clone + serialise + temp-file
    /// + rename on the UI thread, with the authority `Mutex` held across it.
    #[test]
    fn a_burst_of_changes_collapses_into_one_write() {
        let _serialised = lock_real_authority();
        let original = current();

        // Drain anything a previous test left scheduled, so the count starts
        // from a file that is known to be current.
        flush_pending();
        let before = write_count();

        // A slider drag: many real changes in quick succession, all inside the
        // debounce window.
        for step in 0..50 {
            apply(|config| {
                config.volume = 0.1 + (step as f32) * 0.01;
            });
        }
        let after_burst = write_count() - before;

        // Nothing may have reached disk yet: the burst has not ended, and the
        // flusher's whole job is to wait it out.
        //
        // Counted as "at most one" rather than "exactly zero" because other
        // tests in this binary share the real authority without taking
        // `lock_real_authority`, and one of their scheduled writes can land
        // inside this window. Fifty changes still must not become fifty
        // writes - which is the defect being guarded against.
        assert!(
            after_burst <= 1,
            "the debounce let {after_burst} writes through in the middle of a \
             fifty-change burst"
        );

        // One explicit flush stands in for the debounce elapsing.
        flush_pending();
        let after_flush = write_count() - before;

        assert!(
            after_flush >= 1,
            "the burst owed the disk a write and never got one"
        );
        assert!(
            after_flush <= 2,
            "fifty changes must collapse to one or two writes, got {after_flush}"
        );

        // A second flush has nothing left to do. Asserted on the return value
        // only: another test sharing this authority could legitimately arm a
        // new deadline in between, so a write *count* here would be a flake
        // rather than a defect. The re-check itself lives under WRITE_LOCK in
        // `flush_pending`.
        flush_pending();

        apply(|config| {
            *config = original;
        });
        flush_pending();
    }

    /// The in-memory authority is what every reader sees, so it must move
    /// immediately even though the file waits. Losing this would mean the UI
    /// showed a stale value for up to a debounce after every change.
    #[test]
    fn the_authority_updates_immediately_while_the_file_lags() {
        let _serialised = lock_real_authority();
        let original = current();

        flush_pending();

        apply(|config| {
            config.volume = 0.42;
        });

        assert_eq!(
            current().volume,
            0.42,
            "a read right after apply must see the new value"
        );

        apply(|config| {
            *config = original;
        });
        flush_pending();
    }

    /// A normal exit must not lose the last change, which is exactly why the
    /// titlebar X, the tray Exit and the update-install button flush first.
    #[test]
    fn flush_pending_writes_a_change_the_debounce_has_not_reached_yet() {
        let _serialised = lock_real_authority();
        let original = current();
        let path = crate::state::paths::data::config_json();

        flush_pending();
        let before = write_count();

        apply(|config| {
            config.mouse_volume = 0.33;
        });
        // Deliberately NOT waiting for the debounce - this is the exit path.
        assert!(flush_pending(), "an unflushed change must be written by the exit path");

        let on_disk: AppConfig = crate::state::config::parse_lenient(
            &std::fs::read_to_string(&path).expect("the config file must exist after a write"),
        )
        .expect("the written config must parse");

        assert_eq!(on_disk.mouse_volume, 0.33, "the exit flush must have reached the disk");
        assert!(
            write_count() >= before + 1,
            "the exit flush must have issued at least one write"
        );

        apply(|config| {
            *config = original;
        });
        flush_pending();
    }

    /// A mutation that only bumps metadata is indistinguishable from a no-op to
    /// `data_equals`, so it must not leave the in-memory struct carrying a
    /// timestamp the file does not have.
    #[test]
    fn a_metadata_only_mutation_does_not_desync_memory_from_disk() {
        let mut authority = Authority::new();
        let before = authority.config.clone();

        let wrote = authority.apply(|config| {
            config.last_updated = chrono::Utc::now() + chrono::Duration::hours(1);
        });

        assert!(!wrote, "a metadata-only edit is not a change worth writing");
        assert_eq!(
            authority.config.last_updated,
            before.last_updated,
            "and must be rolled back rather than left only in memory"
        );
    }
}
