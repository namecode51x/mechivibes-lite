use dioxus::prelude::*;
use lucide_dioxus::{ House, Music, Palette, Settings, CloudSunRain };

#[allow(non_snake_case)]
#[component]
pub fn Dock() -> Element {
    let route = use_route::<crate::libs::routes::Route>();

    rsx! {
      // A flush, full-width segmented strip welded to the window's bottom
      // edge: five equal cells split by hairlines, each labelled with its
      // panel number (01-05) in mono, the active one filled signal orange.
      // The whole look lives in the `.dock` rules in `style.css`. Deliberately
      // no `backdrop-blur-*` class: the panel is opaque, and the Linux shim
      // in `header.rs` only exists as a fallback for blurred elements.
      div { class: "dock",
        // Button Home
        button {
          class: if matches!(route, crate::libs::routes::Route::Home {}) { "dock-active" } else { "" },
          onclick: move |_| {
              crate::libs::routes::navigate("/");
          },
          House { class: "w-4 h-4" }
          span { class: "dock-label",
            span { class: "dock-num", "01" }
            "Home"
          }
        }
        // Button Soundpacks
        button {
          class: if matches!(route, crate::libs::routes::Route::Soundpacks {}) { "dock-active" } else { "" },
          onclick: move |_| {
              crate::libs::routes::navigate("/soundpacks");
          },
          Music { class: "w-4 h-4" }
          span { class: "dock-label",
            span { class: "dock-num", "02" }
            "Packs"
          }
        }
        // Button Customize
        button {
          class: if matches!(route, crate::libs::routes::Route::Customize {}) { "dock-active" } else { "" },
          onclick: move |_| {
              crate::libs::routes::navigate("/customize");
          },
          Palette { class: "w-4 h-4" }
          span { class: "dock-label",
            span { class: "dock-num", "03" }
            "Custom"
          }
        }
        // Button Mood
        button {
          class: if matches!(route, crate::libs::routes::Route::Mood {}) { "dock-active" } else { "" },
          onclick: move |_| {
              crate::libs::routes::navigate("/mood");
          },
          CloudSunRain { class: "w-4 h-4" }
          span { class: "dock-label",
            span { class: "dock-num", "04" }
            "Mood"
          }
        }
        // Button Settings
        button {
          class: if matches!(route, crate::libs::routes::Route::Settings {}) { "dock-active" } else { "" },
          onclick: move |_| {
              crate::libs::routes::navigate("/settings");
          },
          Settings { class: "w-4 h-4" }
          span { class: "dock-label",
            span { class: "dock-num", "05" }
            "Setup"
          }
        }
      }
    }
}
