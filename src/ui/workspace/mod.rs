pub(crate) mod scratchpad;
mod table;

#[cfg(test)]
mod tests;

use gpui_kit::component::dock::{DockArea, DockEvent, DockPlacement, PanelId, panel_handle};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable, KeyBinding, Subscription, Window, actions,
};
use tokio::runtime::Handle;

use crate::session::Session;
use crate::ui::explorer::DatabaseTarget;
use scratchpad::SqlScratchpad;
use table::TableTab;

actions!(workspace_table, [CopySelection, EditCell]);

pub(crate) fn init_table(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-c", CopySelection, Some("WorkspaceTable")),
        KeyBinding::new("f2", EditCell, Some("WorkspaceTable")),
    ]);
}

#[derive(Clone)]
struct TableTarget {
    database: DatabaseTarget,
    schema: String,
    name: String,
}

impl TableTarget {
    fn matches_scope(&self, connection_id: &str, database: &str, schema: &str) -> bool {
        self.database.matches(connection_id, database) && self.schema == schema
    }

    fn is_current(&self, state: &Session) -> bool {
        state
            .active_connection_id()
            .is_some_and(|id| self.matches_scope(id, state.database(), state.schema()))
    }

    fn same_table(&self, other: &Self) -> bool {
        self.matches_scope(
            other.database.connection_id(),
            other.database.name(),
            &other.schema,
        ) && self.name == other.name
    }
}

struct TableTabEntry {
    target: TableTarget,
    panel: Entity<TableTab>,
    _subscription: Subscription,
}

pub(crate) struct Workspace {
    dock_area: Entity<DockArea>,
    session: Entity<Session>,
    runtime: Handle,
    table_tabs: Vec<TableTabEntry>,
    scratchpads: Vec<Entity<SqlScratchpad>>,
    next_scratchpad_number: u64,
    _dock_subscription: Subscription,
}

impl Workspace {
    pub fn new(
        runtime: Handle,
        session: Entity<Session>,
        dock_area: Entity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let dock_subscription =
            cx.subscribe_in(&dock_area, window, |this, area, event, window, cx| {
                if matches!(event, DockEvent::LayoutChanged) {
                    this.sync_dock(area, window, cx);
                }
            });
        let workspace = Self {
            dock_area,
            session,
            runtime,
            table_tabs: Vec::new(),
            scratchpads: Vec::new(),
            next_scratchpad_number: 1,
            _dock_subscription: dock_subscription,
        };
        // workspace.open_scratchpad(window, cx);
        workspace
    }

    fn sync_dock(&mut self, area: &Entity<DockArea>, _: &mut Window, cx: &mut Context<Self>) {
        let dock = area.read(cx);
        self.table_tabs
            .retain(|tab| dock.panel(PanelId::from(tab.panel.entity_id())).is_some());
        self.scratchpads
            .retain(|panel| dock.panel(PanelId::from(panel.entity_id())).is_some());
        cx.notify();
    }

    pub fn is_center_empty(&self, cx: &App) -> bool {
        self.dock_area.read(cx).is_empty(DockPlacement::Center, cx)
    }

    pub fn loading_table(&self, cx: &App) -> Option<String> {
        self.table_tabs
            .iter()
            .find_map(|tab| tab.panel.read(cx).loading_table())
    }

    pub fn open_scratchpad(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab_number = self.next_scratchpad_number;
        self.next_scratchpad_number += 1;
        let panel = cx.new(|cx| {
            SqlScratchpad::new(
                tab_number,
                self.runtime.clone(),
                self.session.clone(),
                self.dock_area.clone(),
                window,
                cx,
            )
        });
        self.scratchpads.push(panel.clone());
        let panel_id = PanelId::from(panel.entity_id());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(
                panel_handle(panel.clone()),
                DockPlacement::Center,
                None,
                window,
                cx,
            );
            area.select_panel(panel_id, window, cx);
        });
        let focus = panel.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    pub fn open_table(
        &mut self,
        database: DatabaseTarget,
        schema: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = TableTarget {
            database,
            schema,
            name,
        };
        if !target.database.is_live() || !target.is_current(self.session.read(cx)) {
            return;
        }
        let dock = self.dock_area.read(cx);
        self.table_tabs
            .retain(|tab| dock.panel(PanelId::from(tab.panel.entity_id())).is_some());
        let existing = self
            .table_tabs
            .iter()
            .position(|tab| tab.target.same_table(&target));
        let ix = match existing {
            Some(ix) => ix,
            None => {
                let panel = cx.new(|cx| {
                    TableTab::new(
                        self.runtime.clone(),
                        self.session.clone(),
                        self.dock_area.clone(),
                        target.clone(),
                        window,
                        cx,
                    )
                });
                let subscription = cx.observe(&panel, |_, _, cx| cx.notify());
                self.dock_area.update(cx, |area, cx| {
                    area.add_panel_view(
                        panel_handle(panel.clone()),
                        DockPlacement::Center,
                        None,
                        window,
                        cx,
                    );
                });
                self.table_tabs.push(TableTabEntry {
                    target: target.clone(),
                    panel,
                    _subscription: subscription,
                });
                self.table_tabs.len() - 1
            }
        };
        let id = PanelId::from(self.table_tabs[ix].panel.entity_id());
        self.dock_area.update(cx, |area, cx| {
            for placement in [
                DockPlacement::Left,
                DockPlacement::Right,
                DockPlacement::Bottom,
            ] {
                if area
                    .layout(placement)
                    .is_some_and(|tree| tree.find_panel_node(id).is_some())
                    && !area.is_dock_open(placement)
                {
                    area.toggle_dock(placement, window, cx);
                }
            }
            area.select_panel(id, window, cx);
        });
        self.table_tabs[ix].panel.update(cx, |panel, cx| {
            panel.open_table(target.database, target.schema, target.name, window, cx);
        });
        cx.notify();
    }
}
