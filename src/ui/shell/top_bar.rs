use gpui_kit::{
    App, Entity, IntoElement, ParentElement, Styled,
    assets::IconName,
    component::{
        ActiveTheme as _, Disableable as _, Sizable as _,
        button::{Button, ButtonVariants as _},
        menu::{DropdownMenu as _, PopupMenuItem},
    },
    div,
};

use crate::session::Session;
use crate::ui::{connections::open_connection_dialog, workspace::Workspace};

pub fn topbar(
    state: &Entity<Session>,
    workspace: &Entity<Workspace>,
    cx: &mut App,
) -> impl IntoElement {
    let explorer = state.read(cx);
    let connection_id = explorer.active_connection_id().map(str::to_owned);
    let connection_menu = connection_menu(state);
    let connection_picker = connection_picker(state, cx);
    let database_state = state.downgrade();
    let database_connection_id = connection_id.clone();
    let database_picker = create_picker(
        "database-picker",
        "Database",
        IconName::Database,
        explorer.database(),
        explorer.databases(),
        move |name, cx| {
            let _ = database_state.update(cx, |state, cx| {
                if state.active_connection_id() == database_connection_id.as_deref() {
                    state.select_database(name, cx);
                }
            });
        },
    );
    let schema_state = state.downgrade();
    let database = explorer.database().to_owned();
    let schema_picker = create_picker(
        "schema-picker",
        "Schema",
        IconName::Layers,
        explorer.schema(),
        explorer.schemas(),
        move |name, cx| {
            let _ = schema_state.update(cx, |state, cx| {
                if state.active_connection_id() == connection_id.as_deref() {
                    state.select_schema(&database, name, cx);
                }
            });
        },
    );

    let workspace = workspace.downgrade();
    div()
        .w_full()
        .h_10()
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(connection_menu)
        .child(connection_picker)
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(database_picker),
        )
        .child("/")
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(schema_picker),
        )
        .child(div().flex_1())
        .child(
            Button::new("new-sql-scratchpad")
                .ghost()
                .small()
                .icon(IconName::Plus)
                .accessibility_label("New SQL scratchpad")
                .tooltip("New SQL scratchpad")
                .on_click(move |_, window, cx| {
                    let _ =
                        workspace.update(cx, |workspace, cx| workspace.open_scratchpad(window, cx));
                }),
        )
}

fn connection_menu(state: &Entity<Session>) -> impl IntoElement {
    let state = state.clone();
    Button::new("connection-menu")
        .ghost()
        .small()
        .icon(IconName::Menu)
        .accessibility_label("Connections menu")
        .tooltip("Connections")
        .dropdown_menu(move |menu, _, _| {
            let state = state.clone();
            menu.item(
                PopupMenuItem::new("New connection…").on_click(move |_, window, cx| {
                    open_connection_dialog(state.clone(), window, cx)
                }),
            )
        })
}

fn connection_picker(state: &Entity<Session>, cx: &App) -> impl IntoElement {
    let explorer = state.read(cx);
    let selected = explorer.active_connection_id().map(str::to_owned);
    let options = explorer
        .profiles()
        .iter()
        .map(|profile| {
            (
                profile.id().to_owned(),
                profile.name().to_owned(),
                profile.tier(),
                profile.is_configured(),
            )
        })
        .collect::<Vec<_>>();
    let loading = explorer.profiles_loading();
    let selected_name = explorer.active_connection_name().map(str::to_owned);
    let state = state.downgrade();

    Button::new("connection-picker")
        .ghost()
        .small()
        .dropdown_caret(true)
        .accessibility_label(selected_name.as_ref().map_or_else(
            || "Connection: none selected".to_owned(),
            |name| format!("Connection: {name}"),
        ))
        .label(selected_name.unwrap_or_else(|| "No connection".to_owned()))
        .dropdown_menu(move |mut menu, window, _| {
            if options.len() > 10 {
                menu = menu.scrollable(true).max_h(window.rem_size() * 20.);
            }
            if loading {
                menu = menu.item(PopupMenuItem::new("Loading connections…").disabled(true));
            } else if options.is_empty() {
                menu = menu.item(PopupMenuItem::new("No saved connections").disabled(true));
            }
            for (id, name, tier, configured) in &options {
                let state = state.clone();
                let id = id.clone();
                let label = if *configured {
                    format!("{} · {}", name, tier.label())
                } else {
                    format!("{} · {} — URL missing or invalid", name, tier.label())
                };
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .checked(selected.as_deref() == Some(id.as_str()))
                        .disabled(!configured)
                        .on_click(move |_, _, cx| {
                            let _ = state.update(cx, |state, cx| state.select_connection(&id, cx));
                        }),
                );
            }
            menu
        })
}

fn create_picker(
    id: &'static str,
    label: &'static str,
    icon: IconName,
    selected: &str,
    options: &[String],
    on_select: impl Fn(String, &mut App) + Clone + 'static,
) -> impl IntoElement {
    let button = Button::new(id)
        .ghost()
        .xsmall()
        .icon(icon)
        .label(if selected.is_empty() {
            label.to_owned()
        } else {
            selected.to_owned()
        });
    if options.is_empty() {
        return button.disabled(true).into_any_element();
    }

    let selected = selected.to_owned();
    let options = options.to_vec();
    button
        .dropdown_menu(move |menu, window, _| {
            let mut menu = if options.len() > 10 {
                menu.scrollable(true).max_h(window.rem_size() * 20.)
            } else {
                menu
            };
            for name in &options {
                let checked = name == &selected;
                let name = name.clone();
                let on_select = on_select.clone();
                menu = menu.item(
                    PopupMenuItem::new(name.clone())
                        .checked(checked)
                        .on_click(move |_, _, cx| on_select(name.clone(), cx)),
                );
            }
            menu
        })
        .into_any_element()
}
