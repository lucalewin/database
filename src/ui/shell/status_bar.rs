use crate::session::ConnectionTier;
use crate::ui::connections::quality::ConnectionQuality;

use gpui_kit::{
    InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _,
    component::{Sizable as _, spinner::Spinner, status_bar::StatusBar, tag::Tag},
    div,
    prelude::FluentBuilder as _,
};

pub fn status_bar(
    metadata_loading: bool,
    table_loading: Option<String>,
    connection_tier: Option<ConnectionTier>,
    quality: ConnectionQuality,
) -> impl IntoElement {
    let message = loading_message(metadata_loading, table_loading.as_deref());
    StatusBar::new()
        .h_7()
        .flex_shrink_0()
        .when_some(connection_tier, |bar, tier| bar.left(tier_badge(tier)))
        .left(
            div()
                .id("connection-quality")
                .role(Role::Status)
                .aria_label(quality.label())
                .child(quality.label()),
        )
        .when_some(message, |bar, message| {
            bar.right(
                div()
                    .id("loading-status")
                    .role(Role::Status)
                    .aria_label(message.clone())
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .child(Spinner::new().xsmall())
                    .child(div().truncate().child(message)),
            )
        })
}

fn tier_badge(tier: ConnectionTier) -> Tag {
    let tag = match tier {
        ConnectionTier::Local => Tag::success(),
        ConnectionTier::Staging => Tag::warning(),
        ConnectionTier::Production => Tag::danger(),
    };
    tag.small().child(tier.label())
}

fn loading_message(metadata_loading: bool, table_loading: Option<&str>) -> Option<String> {
    match (metadata_loading, table_loading) {
        (true, Some(table)) => Some(format!(
            "Loading database metadata… · Loading rows: {table}"
        )),
        (true, None) => Some("Loading database metadata…".into()),
        (false, Some(table)) => Some(format!("Loading rows: {table}")),
        (false, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::shell::status_bar::loading_message;

    #[test]
    fn shows_all_active_loads_and_clears_when_idle() {
        assert_eq!(loading_message(false, None), None);
        assert_eq!(
            loading_message(true, None).as_deref(),
            Some("Loading database metadata…")
        );
        assert_eq!(
            loading_message(false, Some("db / public / orders")).as_deref(),
            Some("Loading rows: db / public / orders")
        );
        assert_eq!(
            loading_message(true, Some("db / public / orders")).as_deref(),
            Some("Loading database metadata… · Loading rows: db / public / orders")
        );
    }
}
