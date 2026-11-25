mod connection;
mod database;
mod session;
mod ui;

use std::{path::PathBuf, rc::Rc};

use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, ParentElement as _, Render,
    Styled as _, Subscription, Window, WindowDecorations, WindowOptions, application,
    assets::{AllAssets, IconName},
    base::dock::{DockArea, DockLayout, DockPlacement},
    component::{
        ActiveTheme as _, Theme, ThemeRegistry, TitleBar, WindowExt as _,
        button::Button,
        dock::{DockSkin, PanelStyle, panel_handle},
        notification::Notification,
    },
    div, init, open_window,
    prelude::FluentBuilder as _,
    px,
};
use tokio::runtime::Handle;

use crate::session::{Session, SessionEvent};
use crate::ui::{
    connections::quality::ConnectionMonitor,
    explorer::{ExplorerEvent, ExplorerPanel, TableDrag},
    shell::{status_bar::status_bar, top_bar::topbar},
    workspace::scratchpad::init_scratchpad,
    workspace::{Workspace, init_table},
};

struct ConnectionLoadNotification;
struct ConnectionConfigNotification;
struct ConnectionRememberNotification;

struct ViewerWorkspace {
    dock_area: Entity<DockArea>,
    _dock_skin: Rc<DockSkin>,

    session: Entity<Session>,
    connection_monitor: Entity<ConnectionMonitor>,
    _explorer_panel: Entity<ExplorerPanel>,
    workspace: Entity<Workspace>,

    _sub: Subscription,
    _session_subscription: Subscription,
    _explorer_event_subscription: Subscription,
    _connection_monitor_subscription: Subscription,
    _workspace_subscription: Subscription,
}

impl ViewerWorkspace {
    fn new(runtime: Handle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (dock_area, dock_skin) = DockSkin::dock_area("main-dock", Some(1), window, cx);
        dock_skin.set_panel_style(PanelStyle::TabBar, cx);

        let session = cx.new(|cx| Session::new(runtime.clone(), cx));
        let connection_monitor = cx.new(|_| ConnectionMonitor::new(runtime.clone()));
        let session_subscription = cx.observe(&session, |this, state, cx| {
            let connection = state.read(cx).quality_connection();
            this.connection_monitor
                .update(cx, |monitor, cx| monitor.set_connection(connection, cx));
            cx.notify();
        });
        let explorer_event_subscription = cx.subscribe_in(
            &session,
            window,
            |_, _, event: &SessionEvent, window, cx| match event {
                SessionEvent::LoadFailed(error) => window.push_notification(
                    Notification::error(error.clone())
                        .title("Could not connect or load metadata")
                        .id::<ConnectionLoadNotification>(),
                    cx,
                ),
                SessionEvent::LoadSucceeded => {
                    window.remove_notification::<ConnectionLoadNotification>(cx);
                }
                SessionEvent::ConfigurationFailed(error) => window.push_notification(
                    Notification::error(error.clone())
                        .title("Could not load connections")
                        .id::<ConnectionConfigNotification>(),
                    cx,
                ),
                SessionEvent::RememberFailed(error) => window.push_notification(
                    Notification::warning(error.clone())
                        .title("Connection not remembered")
                        .id::<ConnectionRememberNotification>(),
                    cx,
                ),
            },
        );
        let connection_monitor_subscription =
            cx.observe(&connection_monitor, |_, _, cx| cx.notify());
        let explorer_panel = cx.new(|cx| ExplorerPanel::new(session.clone(), window, cx));
        dock_area.update(cx, |area, cx| {
            area.set_dock(
                DockPlacement::Left,
                DockLayout::tabs().panel_view(panel_handle(explorer_panel.clone()), cx),
                window,
                cx,
            );

            area.set_dock_size(DockPlacement::Left, px(300.), window, cx);
        });

        let workspace = cx.new(|cx| {
            Workspace::new(
                runtime.clone(),
                session.clone(),
                dock_area.clone(),
                window,
                cx,
            )
        });
        let workspace_subscription = cx.observe(&workspace, |_, _, cx| cx.notify());

        let _sub = cx.subscribe_in(
            &explorer_panel,
            window,
            |this, _, event: &ExplorerEvent, window, cx| {
                let ExplorerEvent::OpenTable {
                    database,
                    schema,
                    name,
                } = event;
                this.workspace.update(cx, |workspace, cx| {
                    workspace.open_table(
                        database.clone(),
                        schema.clone(),
                        name.clone(),
                        window,
                        cx,
                    );
                });
            },
        );

        Self {
            dock_area,
            _dock_skin: dock_skin,
            session,
            connection_monitor,
            _explorer_panel: explorer_panel,
            workspace,
            _sub,
            _session_subscription: session_subscription,
            _explorer_event_subscription: explorer_event_subscription,
            _connection_monitor_subscription: connection_monitor_subscription,
            _workspace_subscription: workspace_subscription,
        }
    }
}

impl Render for ViewerWorkspace {
    fn render(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::prelude::IntoElement {
        let explorer = self.session.read(cx);
        let metadata_loading = explorer.loading();
        let connection_tier = explorer.active_connection_tier();
        let table_loading = self.workspace.read(cx).loading_table(cx);
        let center_empty = self.workspace.read(cx).is_center_empty(cx);
        let left_dock_open = self.dock_area.read(cx).is_dock_open(DockPlacement::Left);
        let left_dock_size = self
            .dock_area
            .read(cx)
            .dock_size(DockPlacement::Left)
            .unwrap_or(px(0.));
        let workspace = self.workspace.downgrade();
        let title_bar = TitleBar::new()
            .child(topbar(&self.session, &self.workspace, cx))
            .text_xs();
        #[cfg(not(target_os = "macos"))]
        let title_bar = title_bar.pl_0();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(title_bar)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .drag_over::<TableDrag>(|element, _, _, cx| {
                        element.bg(cx.theme().tokens.drop_target)
                    })
                    .on_drop({
                        let workspace = workspace.clone();
                        move |drag: &TableDrag, window, cx| {
                            let _ = workspace.update(cx, |workspace, cx| {
                                workspace.open_table(
                                    drag.database.clone(),
                                    drag.schema.clone(),
                                    drag.name.clone(),
                                    window,
                                    cx,
                                );
                            });
                        }
                    })
                    .child(self.dock_area.clone())
                    .when(center_empty, |content| {
                        content.child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .when(left_dock_open, |panel| panel.left(left_dock_size))
                                .when(!left_dock_open, |panel| panel.left_0())
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .child("Actions")
                                .child(
                                    Button::new("empty-workspace-new-sql-scratchpad")
                                        .label("New SQL scratchpad")
                                        .icon(IconName::Terminal)
                                        .on_click(move |_, window, cx| {
                                            let _ = workspace.update(cx, |workspace, cx| {
                                                workspace.open_scratchpad(window, cx)
                                            });
                                        }),
                                ),
                        )
                    }),
            )
            .child(status_bar(
                metadata_loading,
                table_loading,
                connection_tier,
                self.connection_monitor.read(cx).quality(),
            ))
    }
}

// const THEME_NAME: &str = "One Dark Pro";
const ONE_DARK_THEME: &str = include_str!("../themes/one-dark.json");

fn main() {
    let runtime = tokio::runtime::Runtime::new().expect("Failed to start database runtime");
    let handle = runtime.handle().clone();
    application().with_assets(AllAssets).run(move |cx| {
        init(cx);
        init_scratchpad(cx);
        init_table(cx);
        // Theme::change(ThemeMode::Light, None, cx);
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(ONE_DARK_THEME)
            .expect("Failed to load the embedded One Dark Pro theme");

        // The embedded theme works from any directory; watch the local copy when available.
        let themes_dir = PathBuf::from("./themes");
        if themes_dir.join("one-dark.json").is_file()
            && let Err(err) = ThemeRegistry::watch_dir(themes_dir, cx, |_| {})
        {
            eprintln!("Failed to watch themes directory: {err}");
        }
        let theme = ThemeRegistry::global(cx)
            .themes()
            .get("One Dark Pro")
            .cloned()
            .expect("One Dark Pro is missing from the embedded theme file");
        Theme::update(cx, |current| current.apply_config(&theme));

        let options = WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        };

        open_window(options, cx, |window, cx| {
            cx.new(|cx| ViewerWorkspace::new(handle.clone(), window, cx))
        })
        .expect("Failed to open window");
    });
}
//
// #[cfg(test)]
// mod tests {
//     use gpui_kit::{
//         Hsla,
//         component::{Theme, ThemeMode, ThemeRegistry},
//         rgb,
//     };
//
//     use super::{ONE_DARK_THEME, THEME_NAME};
//
//     #[test]
//     fn one_dark_theme_is_registered_and_applies_its_colors() {
//         let mut registry = ThemeRegistry::default();
//         registry.load_themes_from_str(ONE_DARK_THEME).unwrap();
//         let config = registry.themes().get(THEME_NAME).expect("theme name");
//         assert_eq!(config.mode, ThemeMode::Dark);
//
//         let mut theme = Theme::default();
//         theme.apply_config(config);
//         assert_eq!(theme.mode, ThemeMode::Dark);
//         assert_eq!(theme.colors.background, Hsla::from(rgb(0x23272e)));
//         assert_eq!(theme.colors.foreground, Hsla::from(rgb(0xabb2bf)));
//         assert_eq!(theme.colors.border, Hsla::from(rgb(0x3e4452)));
//         assert_eq!(theme.colors.tab_bar, Hsla::from(rgb(0x1e2227)));
//     }
// }
