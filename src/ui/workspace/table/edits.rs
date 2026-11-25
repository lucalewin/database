use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::{App, Context, Focusable as _, Window};

use crate::database::table::ApplyError;
use crate::ui::workspace::table::TableTab;

#[derive(Default)]
pub(crate) struct EditWorkflow {
    pub(crate) editing: Option<(usize, usize)>,
    pub(crate) applying: bool,
    pub(crate) outcome_unknown: bool,
}

impl TableTab {
    pub(crate) fn has_pending(&self, cx: &App) -> bool {
        self.edits.applying
            || self.edits.editing.is_some()
            || self
                .table
                .as_ref()
                .is_some_and(|table| !table.read(cx).delegate().edits.is_empty())
    }

    pub(crate) fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cell = self.table.as_ref().and_then(|table| {
            table
                .read(cx)
                .selected_cell()
                .and_then(|(row, col)| col.checked_sub(1).map(|col| (row, col)))
        });
        if let Some((row, col)) = cell {
            self.begin_edit_at(row, col, window, cx);
        }
    }

    pub(crate) fn begin_edit_at(
        &mut self,
        row: usize,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.edits.applying
            || self.edits.outcome_unknown
            || self.edits.editing.is_some()
            || self.preview_load.target.is_some()
        {
            return;
        }
        let Some(table) = &self.table else {
            return;
        };
        let state = table.read(cx);
        let delegate = state.delegate();
        let Some(snapshot) = &delegate.edit_snapshot else {
            return;
        };
        if snapshot
            .columns
            .get(col)
            .is_none_or(|column| !column.editable)
        {
            return;
        }
        let Some(original) = snapshot
            .rows
            .get(row)
            .and_then(|row| row.originals.get(col))
        else {
            return;
        };
        let value = delegate
            .edits
            .get(row, col)
            .unwrap_or(original)
            .as_deref()
            .unwrap_or("")
            .to_owned();
        self.editor
            .update(cx, |editor, cx| editor.set_value(value, window, cx));
        self.edits.editing = Some((row, col));
        table.update(cx, |table, cx| {
            table.delegate_mut().editing = Some((row, col));
            table.clear_selection(cx);
            cx.notify();
        });
        self.editor.focus_handle(cx).focus(window, cx);
        self.error = None;
        cx.notify();
    }

    pub(crate) fn stage_edit(
        &mut self,
        value: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, col)) = self.edits.editing else {
            return;
        };
        let Some(table) = &self.table else {
            return;
        };
        let staged = table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            let Some(snapshot) = &delegate.edit_snapshot else {
                return false;
            };
            let success = delegate.edits.stage(snapshot, row, col, value);
            if success {
                cx.notify();
            }
            success
        });
        if staged {
            self.cancel_edit(window, cx);
            self.error = None;
            cx.notify();
        }
    }

    pub(crate) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((row, col)) = self.edits.editing.take() {
            if let Some(table) = &self.table {
                table.update(cx, |table, cx| {
                    table.delegate_mut().editing = None;
                    table.set_selected_cell(row, col + 1, cx);
                    cx.notify();
                });
                table.focus_handle(cx).focus(window, cx);
            }
            cx.notify();
        }
    }

    pub(crate) fn discard_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edits.applying {
            return;
        }
        self.cancel_edit(window, cx);
        if let Some(table) = &self.table {
            table.update(cx, |table, cx| {
                table.delegate_mut().edits.clear();
                if self.edits.outcome_unknown {
                    table.delegate_mut().edit_snapshot = None;
                }
                cx.notify();
            });
        }
        self.error = if self.edits.outcome_unknown {
            Some("Reload the table to verify the previous commit before editing again.".into())
        } else {
            None
        };
        self.edits.outcome_unknown = false;
        cx.notify();
    }

    pub(crate) fn apply_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edits.applying
            || self.edits.outcome_unknown
            || self.edits.editing.is_some()
            || self.preview_load.target.is_some()
        {
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
        let connection = match self.session.read(cx).edit_connection(
            target.database.connection_id(),
            target.database.tier(),
            target.database.name(),
            &target.schema,
            &target.name,
            self.preview_connection_id,
        ) {
            Ok(connection) => connection,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        };

        let Some(table) = &self.table else {
            return;
        };
        let delegate = table.read(cx).delegate();
        let Some(snapshot) = delegate.edit_snapshot.clone() else {
            return;
        };
        let edits = delegate.edits.clone();
        if edits.is_empty() {
            return;
        }
        self.edits.applying = true;
        self.error = None;
        cx.notify();
        let table_name = format!(
            "{} / {} / {}",
            target.database.name(),
            target.schema,
            target.name
        );
        let task = self.runtime.spawn(async move {
            connection
                .apply_table_edits(&target.schema, &target.name, &snapshot, &edits)
                .await
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|error| {
                Err(ApplyError::CommitUnknown(format!(
                    "Apply task failed: {error}. Reload the table before retrying."
                )))
            });
            let _ = this.update_in(cx, |this, window, cx| {
                this.edits.applying = false;
                match result {
                    Ok(()) => {
                        window.push_notification(
                            Notification::success(format!("Changes applied to {table_name}")),
                            cx,
                        );
                        if let Some(table) = &this.table {
                            table.update(cx, |table, cx| {
                                table.delegate_mut().edits.clear();
                                table.delegate_mut().edit_snapshot = None;
                                cx.notify();
                            });
                        }
                        this.error = Some(
                            "Changes applied. Select the table again to reload the preview.".into(),
                        );
                        if let Some(target) = this.target.clone()
                            && target.database.is_live()
                            && target.is_current(this.session.read(cx))
                        {
                            this.open_table(
                                target.database,
                                target.schema,
                                target.name,
                                window,
                                cx,
                            );
                        }
                    }
                    Err(ApplyError::Rejected(message)) => {
                        window.push_notification(
                            Notification::error(format!("Could not apply changes to {table_name}. See the table for details.")),
                            cx,
                        );
                        this.error = Some(message);
                    }
                    Err(ApplyError::CommitUnknown(message)) => {
                        window.push_notification(
                            Notification::warning(format!("The commit outcome for {table_name} is unknown. Reload and verify the table before retrying.")),
                            cx,
                        );
                        this.edits.outcome_unknown = true;
                        this.error = Some(message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
