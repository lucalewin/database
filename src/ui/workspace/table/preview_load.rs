use gpui_kit::component::{
    WindowExt as _,
    notification::Notification,
    table::{TableEvent, TableSelection, TableState},
};
use gpui_kit::{AppContext as _, Context, Window};
use tokio::task::AbortHandle;

use crate::ui::explorer::DatabaseTarget;
use crate::ui::workspace::TableTarget;
use crate::ui::workspace::table::{
    CellRange, TableTab, WorkspaceTableDelegate, columns_for_preview, remap_selection,
};

#[derive(Default)]
pub(crate) struct PreviewLoad {
    pub(crate) target: Option<TableTarget>,
    pub(crate) revision: u64,
    pub(crate) pending: Option<AbortHandle>,
}

impl PreviewLoad {
    fn cancel(&mut self) {
        self.target = None;
        self.revision = self.revision.wrapping_add(1);
        if let Some(task) = self.pending.take() {
            task.abort();
        }
    }
}

impl TableTab {
    pub(crate) fn loading_table(&self) -> Option<String> {
        self.preview_load.target.as_ref().map(|target| {
            format!(
                "{} / {} / {}",
                target.database.name(),
                target.schema,
                target.name
            )
        })
    }

    pub(crate) fn open_table(
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
        if self.has_pending(cx) {
            self.error =
                Some("Apply or discard staged changes before refreshing this preview.".into());
            cx.notify();
            return;
        }
        let state = self.session.read(cx);
        if !target.database.is_live() || !target.is_current(state) {
            return;
        }
        let connection =
            state.table_connection(target.database.name(), &target.schema, &target.name);
        self.cancel_pending();
        let revision = self.preview_load.revision;
        self.error = None;
        let Some(connection) = connection else {
            self.error = Some(format!(
                "Table {:?}.{:?} is no longer available. Refresh the Explorer.",
                target.schema, target.name
            ));
            cx.notify();
            return;
        };

        let connection_id = connection.id();
        self.preview_load.target = Some(target.clone());
        cx.notify();
        let task = self
            .runtime
            .spawn(async move { connection.preview_table(&target.schema, &target.name).await });
        self.preview_load.pending = Some(task.abort_handle());
        cx.spawn_in(window, async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|error| Err(format!("Table preview task failed: {error}")));
            let _ = this.update_in(cx, |this, window, cx| {
                if this.preview_load.revision != revision {
                    return;
                }
                if this
                    .preview_load.target
                    .as_ref()
                    .is_none_or(|target| !target.is_current(this.session.read(cx)))
                    || this.session.read(cx).table_connection(
                        this.preview_load.target.as_ref().map_or("", |target| target.database.name()),
                        this.preview_load.target.as_ref().map_or("", |target| target.schema.as_str()),
                        this.preview_load.target.as_ref().map_or("", |target| target.name.as_str()),
                    ).is_none_or(|connection| connection.id() != connection_id)
                {
                    this.cancel_pending();
                    this.error = Some("Preview interrupted because the connection or scope changed. Select the table again to retry.".into());
                    cx.notify();
                    return;
                }
                this.preview_load.pending = None;
                let target = this.preview_load.target.take();
                match result {
                    Ok((preview, edit_snapshot)) => {
                        this.edits.outcome_unknown = false;
                        this.preview_connection_id = Some(connection_id);
                        this.target = target;
                        this.row_count = preview.rows.len();
                        this.column_count = preview.columns.len();
                        let columns = columns_for_preview(&preview, window, cx);
                        let mut delegate = WorkspaceTableDelegate::new(
                            preview, columns, window.rem_size() * 3.5,
                        );
                        delegate.edit_snapshot = edit_snapshot;
                        delegate.editor = Some(this.editor.clone());
                        let table = cx.new(|cx| {
                            TableState::new(delegate, window, cx)
                                .cell_selectable(true)
                                .row_header(false)
                                .row_selectable(true)
                                .col_selectable(true)
                                .col_movable(false)
                        });
                        this.table_subscription =
                            Some(cx.subscribe_in(&table, window, |this, table, event, window, cx| {
                                cx.notify();
                                match event {
                                    TableEvent::DoubleClickedCell(row, col) if *col > 0 => {
                                        this.begin_edit_at(*row, *col - 1, window, cx);
                                    }
                                    TableEvent::MoveColumn(from, to) => {
                                        table.update(cx, |table, cx| {
                                            table.delegate_mut().selection = None;
                                            let selection = table.selection();
                                            let updated = remap_selection(selection, *from, *to);
                                            if updated != selection {
                                                table.set_selection(updated, cx);
                                            }
                                        });
                                    }
                                    TableEvent::SelectCell(..)
                                    | TableEvent::SelectRow(..)
                                    | TableEvent::SelectColumn(..)
                                    | TableEvent::ClearSelection => {
                                        table.update(cx, |table, cx| {
                                            let selection = table.selection();
                                            if this.edits.editing.is_some() {
                                                if selection != TableSelection::None {
                                                    table.clear_selection(cx);
                                                }
                                                return;
                                            }
                                            let delegate = table.delegate_mut();
                                            match selection {
                                                TableSelection::Cell(row, col) if col > 0 => {
                                                    let point = (row, col);
                                                    if delegate.selection.is_none_or(|range| range.end != point) {
                                                        delegate.selection = Some(CellRange { anchor: point, end: point });
                                                        delegate.selecting = false;
                                                        cx.notify();
                                                    }
                                                }
                                                _ => {
                                                    if delegate.selection.take().is_some() {
                                                        delegate.selecting = false;
                                                        cx.notify();
                                                    }
                                                }
                                            }
                                        });
                                    }
                                    _ => {}
                                }
                            }));
                        this.table = Some(table);
                    }
                    Err(error) => {
                        let notification = match &target {
                            Some(target) => format!(
                                "Could not load {} / {} / {}. See the table for details.",
                                target.database.name(), target.schema, target.name
                            ),
                            None => "Could not load the table. See the table for details.".to_owned(),
                        };
                        window.push_notification(Notification::error(notification), cx);
                        this.error = Some(match target {
                            Some(target) => format!(
                                "Failed to load {} / {} / {}: {error}",
                                target.database.name(),
                                target.schema,
                                target.name
                            ),
                            None => error,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn cancel_pending(&mut self) {
        self.preview_load.cancel();
    }
}
