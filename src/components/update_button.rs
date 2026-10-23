//! The "Update now" button and its state machine.
//!
//! Downloading is a deliberate user action: a background check only ever
//! *notifies*, and nothing is transferred until this button is clicked. One
//! click then carries the update all the way through - download, verify,
//! launch the installer, close the app - with no second confirmation:
//!
//! ```text
//! Idle       "Update now"
//!   click -> Downloading  spinner + "Downloading new version..." (disabled)
//!         -> installs and exits automatically once verified
//!         -> Failed       reason + "Retry", until a newer version appears
//! ```
//!
//! An installer left staged by an interrupted session is picked up on the next
//! launch (`restore_staged_update`), so clicking again reuses it rather than
//! downloading a second time.
//!
//! `Failed` records the version it failed on. It persists as a "Retry" button
//! rather than clearing itself, so a failure is never silently forgotten, and
//! a release newer than that version resets it to "Update now" - retrying a
//! version that is no longer the latest would be the wrong offer. The browser
//! link that predates the staged-download flow sits on its own line below
//! `Retry`, because retrying cannot help a release with no installer asset.

use crate::utils::auto_updater::{
    clear_failed_update,
    download_and_stage_update,
    get_update_stage,
    install_staged_update,
    AutoUpdater,
    UpdateInfo,
    UpdateStage,
};
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use lucide_dioxus::{ Download, RefreshCw };

/// Renders the install control for an available update.
///
/// On platforms without a silent installer this collapses to a plain link to
/// the release page - there is no in-place upgrade path to offer there.
#[component]
pub fn UpdateInstallButton(info: UpdateInfo) -> Element {
    let window = use_window();

    // The stage is written from an async task with no handle on any signal,
    // so it is polled. Half a second is well under the time any step takes
    // and costs a single Mutex read.
    let mut stage = use_signal(get_update_stage);

    // A failure sticks: the button becomes "Retry" and stays that way until
    // it succeeds or a newer version appears. `Failed` carries the version it
    // failed on, so a release newer than that one clears it back to a plain
    // "Update now" - retrying a version that is no longer the latest would be
    // the wrong offer.
    let latest_version = info.latest_version.clone();
    use_future(move || {
        let latest_version = latest_version.clone();
        async move {
            loop {
                if let UpdateStage::Failed { version, .. } = get_update_stage() {
                    if version != latest_version {
                        clear_failed_update();
                    }
                }

                let current = get_update_stage();
                if *stage.peek() != current {
                    stage.set(current);
                }
                futures_timer::Delay::new(std::time::Duration::from_millis(500)).await;
            }
        }
    });

    let version = info.latest_version.clone();
    let release_page = AutoUpdater::releases_page_url(&version);

    // No silent installer on this platform: link out, never offer a download
    // that could not be applied anyway.
    if !crate::utils::update_installer::silent_update_supported() {
        return rsx! {
          a {
            href: "{release_page}",
            target: "_blank",
            class: "btn btn-success btn-sm",
            Download { class: "w-4 h-4 mr-1" }
            "Get v{version}"
          }
        };
    }

    let fallback_link = rsx! {
      a {
        href: "{release_page}",
        target: "_blank",
        class: "link link-hover text-xs",
        "Open download page"
      }
    };

    // Shared by the first attempt and the retry: one click downloads,
    // verifies, runs the installer and closes the app.
    let start_update = {
        let info = info.clone();
        let window = window.clone();
        move |_| {
            let info = info.clone();
            let window = window.clone();
            spawn(async move {
                download_and_stage_update(&info).await;

                // `download_and_stage_update` reports through the shared stage
                // rather than a return value, and it has several early exits
                // (already downloading, no asset, transfer failed). Installing
                // is therefore gated on reading the stage back, not on the
                // await returning.
                if !matches!(get_update_stage(), UpdateStage::Ready { .. }) {
                    return;
                }

                match install_staged_update() {
                    Ok(()) => {
                        // Installer is running detached; close the app the
                        // same way the tray Exit does, which also drops the
                        // input worker's stdin and takes that child with it.
                        //
                        // Flush first: the installer may overwrite this build
                        // while its config still has a write scheduled, and
                        // nothing would run it afterwards.
                        crate::state::config_writer::flush_pending();
                        window.close();
                    }
                    Err(e) => {
                        crate::always_eprint!("❌ Could not start the update installer: {}", e);
                    }
                }
            });
        }
    };

    match stage() {
        UpdateStage::Downloading { version } =>
            rsx! {
          button { class: "btn btn-success btn-sm", disabled: true,
            span { class: "loading loading-spinner loading-xs mr-1" }
            "Downloading new version..."
          }
          div { class: "text-xs text-base-content/50 mt-1", "v{version}" }
        },

        // Verified and on disk. The click that started the download installs
        // from here without asking again, so this is only ever shown for the
        // moment between verification and the app exiting - or if launching
        // the installer failed, in which case clicking retries it.
        UpdateStage::Ready { version, .. } =>
            rsx! {
          button { class: "btn btn-success btn-sm", disabled: true,
            span { class: "loading loading-spinner loading-xs mr-1" }
            "Installing..."
          }
          div { class: "text-xs text-base-content/50 mt-1",
            "v{version} downloaded and verified."
          }
        },

        // Sticks until the retry succeeds or a newer version supersedes it.
        // The browser link sits on its own line below the button, not beside
        // it: a retry cannot help when the release has no installer asset or
        // the network is blocked outright, so the way out has to stay visible
        // without crowding the button.
        UpdateStage::Failed { reason, .. } =>
            rsx! {
          div { class: "flex flex-col items-start gap-2",
            div { class: "text-sm text-warning", "{reason}" }
            button {
              class: "btn btn-warning btn-sm",
              onclick: start_update,
              RefreshCw { class: "w-4 h-4 mr-1" }
              "Retry"
            }
            {fallback_link}
          }
        },

        UpdateStage::Idle =>
            rsx! {
          button {
            class: "btn btn-success btn-sm",
            onclick: start_update,
            Download { class: "w-4 h-4 mr-1" }
            "Update now"
          }
        },
    }
}
