use crate::libs::window_manager::WINDOW_MANAGER;
use crate::utils::constants::APP_NAME;
use crate::state::app::use_update_info;
use dioxus::desktop::use_window;
use dioxus::prelude::*;
use lucide_dioxus::{ Minus, X, EyeClosed, Download };

/// A custom title bar component that allows for window dragging and includes minimize/close buttons
#[component]
pub fn TitleBar() -> Element {
    let window = use_window();
    let window_drag = window.clone();
    let update_info = use_update_info();

    // Get current version for display
    let current_version = crate::utils::constants::APP_VERSION;

    let start_drag = move |_| {
        window_drag.drag();
    };

    // Function to minimize window to tray
    let minimize_to_tray = move |_| {
        // Hide the window to system tray
        WINDOW_MANAGER.hide();
        crate::always_print!("🔽 Window minimized to system tray");
    };

    // Function to close application
    let window_close = window.clone();
    let close = move |_| {
        // A change made in the last debounce window would otherwise be lost:
        // the flusher thread dies with the process, and dioxus exposes no
        // before-exit hook for Alt+F4 to reach. This is the one path the user
        // hits most, so it pays the (single, cheap) flush explicitly.
        crate::state::config_writer::flush_pending();
        window_close.close();
    };

    // Function to minimize application
    let window_minimize = window.clone();
    let minimize = move |_| {
        window_minimize.set_minimized(true);
    };
    rsx! {
      // A flat hairline strip welded to the window top: matte panel, 1px
      // rule below, mono silkscreen wordmark with a status LED and a square
      // key cluster on the right. No blur, no translucency - the whole look
      // lives in the `mv-*` rules in `style.css`.
      div { class: "fixed inset-x-0 top-0 h-10 z-999 flex justify-between items-center select-none gap-0 border-b border-base-content/12 bg-base-200",
      // Left side - app title and draggable area
        div {
          class: "flex items-center gap-2 grow cursor-move px-3 py-2",
          onmousedown: start_drag,
          // Power LED + app title
          span { class: "mv-led mv-led-on" }
          span { class: "mv-title", "{APP_NAME}" }
          // Optional version badge
          span { class: "ml-1 mv-chip",
            "v{current_version}"
          }
        }


        // Right side - window controls
        div { class: "flex items-center space-x-2 px-3 py-2 pl-0",
          // Update notification (separate from draggable area)
          if let Some(update) = update_info.clone() {
            if update.update_available {
              div {
                class: "tooltip tooltip-bottom ml-2",
                "data-tip": "New version {update.latest_version} available!",
                button {
                  class: "btn btn-success btn-xs",
                  // Notification only: this routes to Settings, where the
                  // single install control lives. It deliberately no longer
                  // opens the raw .exe in a browser - that would bypass the
                  // SHA256 verification the in-app path performs, and on
                  // Linux/macOS there is no installable asset to open at all.
                  onclick: move |_| {
                    crate::libs::routes::navigate("/settings");
                  },
                  Download { class: "w-3 h-3" }
                  "{update.latest_version}"
                }
              }
            }
          }
          // Minimize to taskbar button
          div {
            class: "tooltip tooltip-bottom",
            "data-tip": "Minimize",
            button {
              class: "mv-winbtn text-base-content/60 hover:text-base-content",
              onclick: minimize,
              Minus { class: "w-4 h-4" }
            }
          }
          // Minimize to tray button
          div {
            class: "tooltip tooltip-bottom",
            "data-tip": "Hide to tray",
            button {
              class: "mv-winbtn text-base-content/60 hover:text-base-content",
              onclick: minimize_to_tray,
              EyeClosed { class: "w-4 h-4" }
            }
          }
          // Close button
          div { class: "tooltip tooltip-bottom", "data-tip": "Quit",
            button {
              class: "mv-winbtn mv-winbtn-close text-base-content/60",
              title: "Close",
              onclick: close,
              X { class: "w-4 h-4" }
            }
          }
        }
      }
    }
}
