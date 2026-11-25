use std::rc::{Rc, Weak};

use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement, Render, Role,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    assets::IconName,
    base::{TreeItem, TreeState, dock::PanelEvent, h_flex},
    component::{
        ActiveTheme as _,
        dock::{BasePanel, Panel},
        list::ListItem,
        tree::tree,
    },
    div,
    prelude::FluentBuilder as _,
};

use crate::session::{ConnectionTier, Session};

pub struct ExplorerPanel {
    focus_handle: FocusHandle,
    tree_state: Entity<TreeState>,
    state: Entity<Session>,
    snapshot: Option<TableSnapshot>,
    snapshot_generation: Rc<()>,
    _state_subscription: Subscription,
}

/// A database selection scoped to its connection and originating tree snapshot.
#[derive(Clone, Debug)]
pub(crate) struct DatabaseTarget {
    scope: DatabaseScope,
    generation: Weak<()>,
}

impl DatabaseTarget {
    pub(crate) fn new(
        connection_id: String,
        connection_name: String,
        tier: ConnectionTier,
        name: String,
        generation: Weak<()>,
    ) -> Self {
        Self {
            scope: DatabaseScope {
                connection_id,
                connection_name,
                tier,
                name,
            },
            generation,
        }
    }

    pub(crate) fn is_live(&self) -> bool {
        self.generation.upgrade().is_some()
    }

    pub fn tier(&self) -> ConnectionTier {
        self.scope.tier
    }

    pub fn connection_id(&self) -> &str {
        &self.scope.connection_id
    }
    pub fn connection_name(&self) -> &str {
        &self.scope.connection_name
    }

    pub fn name(&self) -> &str {
        &self.scope.name
    }

    pub(crate) fn matches(&self, connection_id: &str, database: &str) -> bool {
        self.connection_id() == connection_id && self.name() == database
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DatabaseScope {
    connection_id: String,
    connection_name: String,
    tier: ConnectionTier,
    name: String,
}

#[derive(Clone, PartialEq, Eq)]
struct TableSnapshot {
    database: DatabaseScope,
    schema: String,
    tables: Vec<String>,
}

impl TableSnapshot {
    fn from_state(state: &Session) -> Self {
        Self {
            database: DatabaseScope {
                connection_id: state.active_connection_id().unwrap_or("").to_owned(),
                connection_name: state.active_connection_name().unwrap_or("").to_owned(),
                tier: state.connection_tier(),
                name: state.database().to_owned(),
            },
            schema: state.schema().to_owned(),
            tables: state.tables().to_vec(),
        }
    }

    fn open_event(
        &self,
        current: &Self,
        item_id: &str,
        generation: Weak<()>,
    ) -> Option<ExplorerEvent> {
        if self != current || generation.upgrade().is_none() {
            return None;
        }
        let name = self
            .tables
            .iter()
            .find(|name| table_id(self, name) == item_id)?;
        Some(ExplorerEvent::OpenTable {
            database: DatabaseTarget::new(
                self.database.connection_id.clone(),
                self.database.connection_name.clone(),
                self.database.tier,
                self.database.name.clone(),
                generation,
            ),
            schema: self.schema.clone(),
            name: name.clone(),
        })
    }
}

fn table_id(snapshot: &TableSnapshot, name: &str) -> String {
    format!(
        "table:{}:{}:{}:{}:{}:{}:{name}",
        snapshot.database.connection_id.len(),
        snapshot.database.connection_id,
        snapshot.database.name.len(),
        snapshot.database.name,
        snapshot.schema.len(),
        snapshot.schema,
    )
}

#[derive(Clone, Debug)]
pub(crate) struct TableDrag {
    pub(crate) database: DatabaseTarget,
    pub(crate) schema: String,
    pub(crate) name: String,
}

impl Render for TableDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .child(IconName::Table2)
            .child(self.name.clone())
    }
}

pub(crate) enum ExplorerEvent {
    OpenTable {
        database: DatabaseTarget,
        schema: String,
        name: String,
    },
}

impl ExplorerPanel {
    pub fn new(state: Entity<Session>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let state_subscription = cx.observe(&state, |this, _, cx| this.rebuild(cx));
        let mut panel = Self {
            tree_state,
            focus_handle: cx.focus_handle(),
            state,
            snapshot: None,
            snapshot_generation: Rc::new(()),
            _state_subscription: state_subscription,
        };
        panel.rebuild(cx);
        panel
    }

    fn activate_selected(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        let Some(item) = self.tree_state.read(cx).selected_item() else {
            return false;
        };
        let Some(event) = snapshot.open_event(
            &TableSnapshot::from_state(self.state.read(cx)),
            item.id.as_ref(),
            Rc::downgrade(&self.snapshot_generation),
        ) else {
            return false;
        };
        cx.emit(event);
        true
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let snapshot = TableSnapshot::from_state(self.state.read(cx));
        if self.snapshot.as_ref() != Some(&snapshot) {
            // Old row callbacks and queued open events must not survive a tree replacement.
            self.snapshot_generation = Rc::new(());
            let children = if snapshot.tables.is_empty() {
                vec![TreeItem::new("no-tables", "No tables").disabled(true)]
            } else {
                snapshot
                    .tables
                    .iter()
                    .map(|name| TreeItem::new(table_id(&snapshot, name), name.clone()))
                    .collect()
            };
            let items = if snapshot.schema.is_empty() {
                Vec::new()
            } else {
                vec![
                    TreeItem::new("tables", format!("Tables ({})", snapshot.tables.len()))
                        .expanded(true)
                        .children(children),
                ]
            };
            self.tree_state
                .update(cx, |tree, cx| tree.set_items(items, cx));
            self.snapshot = Some(snapshot);
        }
        cx.notify();
    }
}

impl EventEmitter<ExplorerEvent> for ExplorerPanel {}
impl EventEmitter<PanelEvent> for ExplorerPanel {}

impl Focusable for ExplorerPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ExplorerPanel {
    fn panel_name(&self) -> &'static str {
        "Explorer"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for ExplorerPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        "Explorer"
    }
}

impl Render for ExplorerPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let loading = state.loading();
        let profiles_loading = state.profiles_loading();
        let error = state.error().map(str::to_owned);
        let has_error = error.is_some();
        let has_schema = !state.schema().is_empty();
        let has_connection = state.active_connection_id().is_some();
        let owner = cx.entity().downgrade();
        let snapshot = self.snapshot.clone();
        let generation = Rc::downgrade(&self.snapshot_generation);

        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .text_sm()
            .child(
                div()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when_some(error, |this, error| {
                        this.child(div().text_sm().text_color(cx.theme().danger).child(error))
                    })
                    .when(profiles_loading, |this| {
                        this.child(
                            div()
                                .id("loading-connections")
                                .role(Role::Status)
                                .aria_label("Loading connections")
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Loading connections…"),
                        )
                    })
                    .when(
                        !profiles_loading && !loading && !has_schema && !has_error,
                        |this| {
                            this.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if has_connection {
                                        "Select an accessible schema to browse tables"
                                    } else {
                                        "Add or select a connection to browse tables"
                                    }),
                            )
                        },
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if event.keystroke.key == "enter" && this.activate_selected(cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .child(tree(&self.tree_state, move |_, entry, selected, _, cx| {
                        let item = entry.item();
                        let item_id = item.id.to_string();
                        let is_folder = entry.is_folder();
                        let is_disabled = entry.is_disabled();
                        let icon = if !is_folder {
                            IconName::Table2
                        } else if entry.is_expanded() {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        };
                        let owner = owner.clone();
                        let snapshot = snapshot.clone();
                        let generation = generation.clone();
                        let drag_snapshot = snapshot.clone();
                        let drag_generation = generation.clone();
                        let drag_item_id = item_id.clone();
                        let mut row = ListItem::new(item.id.clone())
                            .selected(selected)
                            .disabled(is_disabled)
                            .when(entry.depth() > 0, |row| row.pl_8())
                            .child(
                                h_flex()
                                    .h_4()
                                    .text_sm()
                                    .gap_2()
                                    .child(div().child(icon).text_color(cx.theme().blue_light))
                                    .child(item.label.clone()),
                            )
                            .on_click(move |_, _, cx| {
                                if !is_folder
                                    && !is_disabled
                                    && let Some(snapshot) = &snapshot
                                {
                                    let _ = owner.update(cx, |this, cx| {
                                        if this.snapshot.as_ref() != Some(snapshot) {
                                            return;
                                        }
                                        let current =
                                            TableSnapshot::from_state(this.state.read(cx));
                                        if let Some(event) = snapshot.open_event(
                                            &current,
                                            &item_id,
                                            generation.clone(),
                                        ) {
                                            cx.emit(event);
                                        }
                                    });
                                }
                            });
                        if !is_folder
                            && !is_disabled
                            && let (Some(snapshot), Some(current)) =
                                (drag_snapshot.as_ref(), drag_snapshot.as_ref())
                            && let Some(event) =
                                snapshot.open_event(current, &drag_item_id, drag_generation.clone())
                            && let ExplorerEvent::OpenTable {
                                database,
                                schema,
                                name,
                            } = event
                        {
                            row = row.on_drag(
                                TableDrag {
                                    database,
                                    schema,
                                    name,
                                },
                                |drag, _, _, cx| cx.new(|_| drag.clone()),
                            );
                        }
                        row
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::session::ConnectionTier;
    use crate::ui::explorer::{DatabaseScope, ExplorerEvent, TableSnapshot, table_id};

    #[test]
    fn table_ids_keep_scope_and_folder_names_distinct() {
        let first = TableSnapshot {
            database: DatabaseScope {
                connection_id: "local".into(),
                connection_name: "Local".into(),
                tier: ConnectionTier::Local,
                name: "a:b".into(),
            },
            schema: "c".into(),
            tables: vec![],
        };
        let second = TableSnapshot {
            database: DatabaseScope {
                connection_id: "local".into(),
                connection_name: "Local".into(),
                tier: ConnectionTier::Local,
                name: "a".into(),
            },
            schema: "b:c".into(),
            tables: vec![],
        };
        assert_ne!(table_id(&first, "tables"), table_id(&second, "tables"));
        assert_ne!(table_id(&first, "tables"), "tables");
    }

    #[test]
    fn connection_switch_rekeys_tables_and_rejects_stale_activations() {
        let local = TableSnapshot {
            database: DatabaseScope {
                connection_id: "local".into(),
                connection_name: "Local".into(),
                tier: ConnectionTier::Local,
                name: "db".into(),
            },
            schema: "public".into(),
            tables: vec!["orders".into()],
        };
        let mut staging = local.clone();
        staging.database.connection_id = "other-local".into();
        let local_id = table_id(&local, "orders");
        assert_ne!(local_id, table_id(&staging, "orders"));
        let generation = Rc::new(());
        assert!(
            local
                .open_event(&staging, &local_id, Rc::downgrade(&generation))
                .is_none()
        );
        assert!(
            local
                .open_event(&local, "tables", Rc::downgrade(&generation))
                .is_none()
        );
        match local.open_event(&local, &local_id, Rc::downgrade(&generation)) {
            Some(ExplorerEvent::OpenTable {
                database,
                schema,
                name,
            }) => {
                assert!(database.matches("local", "db"));
                assert!(database.is_live());
                drop(generation);
                assert!(!database.is_live());
                assert_eq!(schema, "public");
                assert_eq!(name, "orders");
            }
            None => panic!("current table should open"),
        }
        let mut refreshed = local.clone();
        refreshed.tables.clear();
        let generation = Rc::new(());
        assert!(
            local
                .open_event(&refreshed, &local_id, Rc::downgrade(&generation))
                .is_none()
        );
        let stale_generation = Rc::downgrade(&generation);
        drop(generation);
        // Even after returning to an identical snapshot, an old row stays stale.
        assert!(
            local
                .open_event(&local, &local_id, stale_generation)
                .is_none()
        );
    }
}
