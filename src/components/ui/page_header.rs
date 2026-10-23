use dioxus::prelude::*;

#[derive(Props, PartialEq, Clone)]
pub struct PageHeaderProps {
    pub title: String,
    #[props(optional)]
    pub subtitle: Option<String>,
    #[props(optional)]
    pub icon: Option<Element>,
    /// Panel index printed before the title (the same 01-05 series the dock
    /// uses), rendered in signal orange as `mv-idx-lg`.
    #[props(optional)]
    pub index: Option<String>,
}

#[component]
pub fn PageHeader(props: PageHeaderProps) -> Element {
    rsx! {
      // `mv-rise` gives every page a short settle-in on navigation; the
      // animation runs once on mount and never idles. The header itself is
      // an instrument-panel title block: square function key, orange index,
      // mono silkscreen title, dotted leader running to the right edge.
      div { class: "mb-7 flex items-center gap-3 mv-rise",
        if let Some(icon) = props.icon {
          div { class: "mv-tile",
            {icon}
          }
        }
        div { class: "flex-1 min-w-0",
          div { class: "flex items-baseline",
            if let Some(index) = props.index.as_deref() {
              span { class: "mv-idx-lg mr-3", "{index}" }
            }
            h1 { class: "mv-h1", "{props.title}" }
            span { class: "mv-rule" }
          }
          if let Some(subtitle) = props.subtitle {
            p { class: "mv-sub", "{subtitle}" }
          }
        }
      }
    }
}
