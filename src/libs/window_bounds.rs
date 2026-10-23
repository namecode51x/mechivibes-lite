//! Keeping the window on screen at startup.
//!
//! The window is created at a fixed logical size with no saved geometry: there
//! is no position persistence anywhere in the app, so every launch places the
//! window from scratch. Two things went wrong with that.
//!
//! A fixed 820pt height does not fit a 1366x768 laptop, whose work area is
//! roughly 728pt tall once the taskbar is subtracted, so the bottom of the app
//! (the dock, and the buttons above it) sat below the screen edge. And letting
//! the platform choose the position put the window partly outside the desktop
//! on some machines, with no way to drag it back because the title bar is
//! custom and can end up off-screen too.
//!
//! The fix is to measure the work area of the monitor the window actually
//! landed on, shrink the window to fit it, and centre what remains inside it.
//! The layout already scrolls (`routes.rs` puts the content in a `flex-1
//! overflow-auto` between a fixed title bar and dock), so a shorter window
//! shows less at once rather than clipping anything away.
//!
//! Everything here works in *physical* pixels. Work areas come from the OS in
//! physical pixels and monitors on one desktop can have different scale
//! factors, so converting to logical units would mean picking one monitor's
//! factor and being wrong about the others. Only the final call into tao
//! converts, using the scale factor of the window's own monitor.

/// The size the window opens at on a screen with room for it, in logical
/// points. Shared with the window builder in `main.rs` so the startup fit
/// measures the same window the builder asked for.
pub const DEFAULT_WINDOW_SIZE: (u32, u32) = (470, 820);

/// The smallest the window is allowed to get. Below this the dock and the
/// title bar controls start overlapping the content they sit around.
pub const MIN_WINDOW_SIZE: (u32, u32) = (470, 600);

/// A rectangle in physical pixels, in desktop coordinates.
///
/// The origin can be negative: a monitor placed to the left of the primary one
/// starts at a negative x, which is exactly the case that puts a window
/// off-screen when it is assumed to start at zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }
}

/// Where the window should end up: a size that fits and a position inside the
/// work area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
}

/// Fit `desired` (width, height) into `work_area` and centre it there.
///
/// Size is capped first, because a window wider or taller than the work area
/// cannot be positioned inside it at all - clamping the position of an
/// oversized window would just pin it to one corner and leave the far edge off
/// screen. Once it fits, centring is what puts it fully inside, and it also
/// gives a sane first-launch position on the monitor the user is looking at.
///
/// `min_size` keeps the window usable on a work area small enough that fitting
/// would otherwise squash it to nothing, but it only applies while it fits.
/// Staying on screen wins over honouring the minimum: a 600pt minimum is 750
/// physical pixels at 125% scaling, which is already taller than a 1366x768
/// laptop's 728px work area, so enforcing it there would push the dock back off
/// the bottom of the screen - the exact bug this function exists to prevent.
/// A cramped window the user can reach beats a comfortable one they cannot.
pub fn fit_to_work_area(work_area: Rect, desired: (u32, u32), min_size: (u32, u32)) -> Placement {
    let (desired_w, desired_h) = desired;
    let (min_w, min_h) = min_size;

    // `.min(work_area)` last, so the work area overrides the minimum whenever
    // the two disagree.
    let width = desired_w.min(work_area.width).max(min_w).min(work_area.width);
    let height = desired_h.min(work_area.height).max(min_h).min(work_area.height);

    // Centre, then guard the top-left. `saturating_sub` on the u32 difference
    // handles the case where the window is still larger than the work area
    // (min_size won out): the offset is zero rather than wrapping, leaving the
    // window at the work area's origin.
    let x = work_area.x + ((work_area.width.saturating_sub(width) / 2) as i32);
    let y = work_area.y + ((work_area.height.saturating_sub(height) / 2) as i32);

    Placement { width, height, x, y }
}

/// The work area of the monitor the window is on, in physical pixels.
///
/// "Work area" and not the full monitor bounds: the taskbar (or dock, or
/// panel) is what makes the difference between a window that looks placed and
/// one whose bottom edge is buried, and on a 1366x768 screen it is a large
/// enough slice to decide whether an 820pt window fits.
///
/// Returns `None` when the work area cannot be determined, in which case the
/// caller leaves the window alone rather than guessing - moving a window to a
/// wrong place is worse than not moving it.
#[cfg(target_os = "windows")]
pub fn monitor_work_area(monitor: &dioxus::desktop::tao::monitor::MonitorHandle) -> Option<Rect> {
    use dioxus::desktop::tao::platform::windows::MonitorHandleExtWindows;
    use winapi::um::winuser::{ GetMonitorInfoW, MONITORINFO };

    let hmonitor = monitor.hmonitor();
    if hmonitor == 0 {
        return None;
    }

    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;

    // SAFETY: `hmonitor` comes from tao's live monitor handle, so it is a valid
    // HMONITOR for as long as that monitor exists. `info` is a correctly sized,
    // zeroed MONITORINFO with `cbSize` set as the API requires, and it is only
    // read after the call reports success.
    let ok = unsafe { GetMonitorInfoW(hmonitor as _, &mut info) };
    if ok == 0 {
        return None;
    }

    let work = info.rcWork;
    let width = work.right.checked_sub(work.left)?;
    let height = work.bottom.checked_sub(work.top)?;
    if width <= 0 || height <= 0 {
        return None;
    }

    Some(Rect::new(work.left, work.top, width as u32, height as u32))
}

/// The work area of the monitor the window is on, in physical pixels.
///
/// tao exposes no work area outside Windows, so this falls back to the full
/// monitor bounds. That still fixes the case that matters most - a window
/// taller than the screen itself - and only leaves the panel-sized margin
/// unaccounted for.
#[cfg(not(target_os = "windows"))]
pub fn monitor_work_area(monitor: &dioxus::desktop::tao::monitor::MonitorHandle) -> Option<Rect> {
    let size = monitor.size();
    if size.width == 0 || size.height == 0 {
        return None;
    }
    let position = monitor.position();
    Some(Rect::new(position.x, position.y, size.width, size.height))
}

/// Shrink the window to its monitor's work area and centre it there.
///
/// Runs once, after the window exists: the monitor it opened on is not known
/// before that, and on a multi-monitor desktop guessing the primary one would
/// move the window away from where the user is working.
///
/// `desired` and `min_size` are logical points - the same units `main.rs`
/// builds the window with. They are converted to physical pixels using the
/// scale factor of the window's own monitor, which is what makes this correct
/// at 125% and 150% DPI: an 820pt window is 1025 physical pixels at 125%, and
/// comparing that against a physical work area is the only comparison that
/// means anything. Mixing the two is the classic form of this bug.
pub fn fit_window_to_monitor(
    window: &dioxus::desktop::tao::window::Window,
    desired: (u32, u32),
    min_size: (u32, u32)
) {
    use dioxus::desktop::tao::dpi::{ PhysicalPosition, PhysicalSize };

    let Some(monitor) = window.current_monitor() else {
        crate::debug_print!("📐 No monitor reported for the window; leaving its size and position alone");
        return;
    };

    let Some(work_area) = monitor_work_area(&monitor) else {
        crate::debug_print!("📐 Could not read the monitor work area; leaving the window alone");
        return;
    };

    let scale = monitor.scale_factor();
    let to_physical = |points: (u32, u32)| -> (u32, u32) {
        (
            ((points.0 as f64) * scale).round() as u32,
            ((points.1 as f64) * scale).round() as u32,
        )
    };

    let placement = fit_to_work_area(work_area, to_physical(desired), to_physical(min_size));

    // The builder's minimum is 600pt tall, which on a small or heavily scaled
    // screen is taller than the fitted window. Left in place the platform
    // would simply refuse the smaller size and hand back the overflowing
    // window, so the minimum has to come down to whatever actually fits. It is
    // lowered only as far as necessary, so on a normal screen the user still
    // cannot drag the window below a usable height.
    let (min_w, min_h) = to_physical(min_size);
    window.set_min_inner_size(
        Some(PhysicalSize::new(min_w.min(placement.width), min_h.min(placement.height)))
    );
    // Raising the height cap alongside the new size matters: the window was
    // built with a maximum equal to its default height, so without this the
    // platform would refuse to ever grow it back if the user moves to a larger
    // screen. Width stays pinned to the fitted width: the app is a fixed-width
    // column and only vertical resizing is intended.
    window.set_max_inner_size(Some(PhysicalSize::new(placement.width, work_area.height)));
    window.set_inner_size(PhysicalSize::new(placement.width, placement.height));
    window.set_outer_position(PhysicalPosition::new(placement.x, placement.y));

    crate::debug_print!(
        "📐 Window fitted to {}x{} at ({}, {}) in work area {}x{}+{}+{} (scale {:.2})",
        placement.width,
        placement.height,
        placement.x,
        placement.y,
        work_area.width,
        work_area.height,
        work_area.x,
        work_area.y,
        scale
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1920x1080 desktop with a standard Windows taskbar.
    const DESKTOP: Rect = Rect { x: 0, y: 0, width: 1920, height: 1040 };
    /// The 1366x768 laptop from the report, taskbar included.
    const LAPTOP: Rect = Rect { x: 0, y: 0, width: 1366, height: 728 };

    /// The app's own numbers, in logical points at 100% scaling.
    const DESIRED: (u32, u32) = (470, 820);
    const MIN: (u32, u32) = (470, 600);

    #[test]
    fn a_window_that_fits_keeps_its_size_and_is_centred() {
        let placement = fit_to_work_area(DESKTOP, DESIRED, MIN);
        assert_eq!((placement.width, placement.height), DESIRED, "no need to shrink here");
        assert_eq!(placement.x, (1920 - 470) / 2);
        assert_eq!(placement.y, (1040 - 820) / 2);
    }

    #[test]
    fn the_window_shrinks_to_fit_a_1366x768_laptop() {
        // The reported bug: 820 tall on a 728 tall work area put the dock and
        // the controls above it below the bottom edge of the screen.
        let placement = fit_to_work_area(LAPTOP, DESIRED, MIN);
        assert_eq!(placement.height, 728, "the window must not exceed the work area");
        assert_eq!(placement.width, 470, "width already fits, leave it alone");
        assert_eq!(placement.y, 0);
        assert!(
            placement.y + (placement.height as i32) <= LAPTOP.y + (LAPTOP.height as i32),
            "the bottom edge must land inside the work area"
        );
    }

    #[test]
    fn a_fitted_window_is_fully_inside_the_work_area_on_every_screen_size() {
        // The property the fix exists for, checked across the sizes users
        // actually run, including the 1600x900 from the off-screen report.
        let screens = [
            Rect::new(0, 0, 1366, 728),
            Rect::new(0, 0, 1600, 860),
            Rect::new(0, 0, 1920, 1040),
            Rect::new(0, 0, 2560, 1400),
            Rect::new(0, 0, 3840, 2120),
        ];

        for screen in screens {
            let p = fit_to_work_area(screen, DESIRED, MIN);
            assert!(p.x >= screen.x, "left edge off screen on {screen:?}");
            assert!(p.y >= screen.y, "top edge off screen on {screen:?}");
            assert!(
                p.x + (p.width as i32) <= screen.x + (screen.width as i32),
                "right edge off screen on {screen:?}"
            );
            assert!(
                p.y + (p.height as i32) <= screen.y + (screen.height as i32),
                "bottom edge off screen on {screen:?}"
            );
        }
    }

    #[test]
    fn placement_follows_a_monitor_that_starts_at_a_negative_origin() {
        // A second monitor to the left of the primary has negative desktop
        // coordinates. Treating the origin as zero would place the window on
        // the wrong screen, or in the gap between them.
        let left_monitor = Rect::new(-1920, -200, 1920, 1040);
        let p = fit_to_work_area(left_monitor, DESIRED, MIN);
        assert_eq!(p.x, -1920 + (1920 - 470) / 2);
        assert_eq!(p.y, -200 + (1040 - 820) / 2);
        assert!(p.x + (p.width as i32) <= left_monitor.x + (left_monitor.width as i32));
    }

    #[test]
    fn a_work_area_offset_by_a_side_taskbar_is_respected() {
        // A taskbar docked left shifts the work area's origin on the primary
        // monitor, so x=0 is no longer usable space.
        let work = Rect::new(80, 0, 1840, 1080);
        let p = fit_to_work_area(work, DESIRED, MIN);
        assert!(p.x >= 80, "the window must clear the side taskbar");
        assert!(p.x + (p.width as i32) <= 1920);
    }

    #[test]
    fn a_work_area_smaller_than_the_minimum_still_fits_on_screen() {
        // The minimum is a preference, not a floor that can push the window
        // off the screen. On a work area smaller than the minimum the window
        // takes the whole work area: cramped but wholly reachable.
        let tiny = Rect::new(0, 0, 320, 240);
        let p = fit_to_work_area(tiny, DESIRED, MIN);
        assert_eq!((p.width, p.height), (320, 240), "fitting the screen outranks the minimum");
        assert_eq!((p.x, p.y), (0, 0));
    }

    #[test]
    fn an_exact_fit_is_left_where_it_is() {
        let exact = Rect::new(0, 0, 470, 820);
        let p = fit_to_work_area(exact, DESIRED, MIN);
        assert_eq!((p.width, p.height), DESIRED);
        assert_eq!((p.x, p.y), (0, 0), "no room to centre in, and none needed");
    }

    /// The conversion `fit_window_to_monitor` applies before clamping, split
    /// out so the unit arithmetic can be checked without a live window.
    fn to_physical(points: (u32, u32), scale: f64) -> (u32, u32) {
        (((points.0 as f64) * scale).round() as u32, ((points.1 as f64) * scale).round() as u32)
    }

    #[test]
    fn a_scaled_window_is_measured_against_the_physical_work_area() {
        // 1920x1080 at 150%: Windows reports the work area in physical pixels
        // (1920x1040), while the app's 820pt height is 1230 physical pixels.
        // Comparing 820 against 1040 would conclude it fits and leave a third
        // of the window below the screen - the unit mix-up this guards.
        let scale = 1.5;
        let work = Rect::new(0, 0, 1920, 1040);
        let p = fit_to_work_area(work, to_physical(DESIRED, scale), to_physical(MIN, scale));

        assert_eq!(p.height, 1040, "1230 physical pixels does not fit 1040");
        assert_eq!(p.width, 705, "470pt at 150% is 705 physical pixels");
        assert!(p.y + (p.height as i32) <= 1040);
    }

    #[test]
    fn a_125_percent_laptop_still_lands_on_screen() {
        // 1366x768 at 125% is the worst reported combination: a small panel
        // and a scale factor that inflates the window on top of it.
        let scale = 1.25;
        let work = Rect::new(0, 0, 1366, 728);
        let p = fit_to_work_area(work, to_physical(DESIRED, scale), to_physical(MIN, scale));

        assert!(p.x >= 0 && p.y >= 0);
        assert!(p.x + (p.width as i32) <= 1366);
        assert!(p.y + (p.height as i32) <= 728);
    }

    #[test]
    fn the_fitted_size_never_exceeds_the_work_area_it_was_given() {
        // Guards the ordering inside `fit_to_work_area`: capping to the work
        // area has to come before centring, otherwise an oversized window gets
        // a negative offset and lands off screen.
        for height in [200u32, 600, 728, 819, 820, 821, 2000] {
            let work = Rect::new(0, 0, 1000, height);
            let p = fit_to_work_area(work, DESIRED, MIN);
            assert!(
                p.height <= work.height,
                "height {} overflows a {}px work area",
                p.height,
                height
            );
        }
    }
}
