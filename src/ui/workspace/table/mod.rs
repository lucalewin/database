pub(crate) mod delegate;
mod edits;
mod preview_load;
mod sizing;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    button::Button,
    dock::{BasePanel, DockArea, Panel, PanelEvent, PanelId, TabGroup},
    input::{InputEvent, InputState},
    scroll::Scrollbar,
    table::{DataTable, TableSelection, TableState},
};
use gpui_kit::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, HitboxBehavior,
    IntoElement, MouseButton, ParentElement, Render, ScrollEvent, Subscription, WeakEntity, Window,
    canvas, div, px,
};
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::session::Session;

use crate::ui::workspace::{CopySelection, EditCell, TableTarget};

pub(crate) use delegate::{CellRange, WorkspaceTableDelegate, accelerated_offset, review_value};
use gpui_kit::{
    AppContext as _, InteractiveElement as _, StatefulInteractiveElement as _, Styled as _,
    component::{ActiveTheme as _, Disableable as _, Sizable as _, button::ButtonVariants as _},
    prelude::FluentBuilder as _,
};
pub(crate) use sizing::{PREVIEW_TABLE_SIZE, columns_for_preview, preview_cell_display};

use preview_load::PreviewLoad;

use edits::EditWorkflow;

pub(crate) struct TableTab {
    focus_handle: FocusHandle,

    dock_area: WeakEntity<DockArea>,
    show_separator: bool,
    _tab_group_subscription: Option<Subscription>,
    tab_target: TableTarget,
    session: Entity<Session>,
    runtime: Handle,
    table: Option<Entity<TableState<WorkspaceTableDelegate>>>,
    table_subscription: Option<Subscription>,
    target: Option<TableTarget>,
    preview_load: PreviewLoad,
    row_count: usize,
    column_count: usize,
    error: Option<String>,
    editor: Entity<InputState>,
    _editor_subscription: Subscription,
    preview_connection_id: Option<Uuid>,
    edits: EditWorkflow,
    _explorer_subscription: Subscription,
}

impl TableTab {
    pub(crate) fn new(
        runtime: Handle,
        session: Entity<Session>,
        dock_area: Entity<DockArea>,
        tab_target: TableTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| InputState::new(window, cx).submit_on_enter(true));
        let editor_subscription = cx.subscribe_in(
            &editor,
            window,
            |this, editor, event, window, cx| match event {
                InputEvent::PressEnter {
                    shift: false,
                    secondary: false,
                } if this.edits.editing.is_some() => {
                    let value = editor.read(cx).value().to_string();
                    this.stage_edit(Some(value), window, cx);
                }
                InputEvent::Blur if this.edits.editing.is_some() => {
                    this.cancel_edit(window, cx);
                }
                _ => {}
            },
        );
        let explorer_subscription = cx.observe(&session, |this, state, cx| {
            let state = state.read(cx);
            if this.preview_load.target.as_ref().is_some_and(|target| !target.is_current(state)) {
                this.cancel_pending();
                this.error = Some("Preview interrupted because the connection or scope changed. Select the table again to retry.".into());
                cx.notify();
            }
        });
        Self {
            focus_handle: cx.focus_handle(),

            dock_area: dock_area.downgrade(),
            show_separator: false,
            _tab_group_subscription: None,
            tab_target,
            session,
            runtime,
            table: None,
            table_subscription: None,
            target: None,
            preview_load: PreviewLoad::default(),
            row_count: 0,
            column_count: 0,
            error: None,
            editor,
            _editor_subscription: editor_subscription,
            preview_connection_id: None,
            edits: EditWorkflow::default(),
            _explorer_subscription: explorer_subscription,
        }
    }
}

pub(crate) fn remap_selection(selection: TableSelection, from: usize, to: usize) -> TableSelection {
    let remap = |index| {
        if index == from {
            to
        } else if from < to && index > from && index <= to {
            index - 1
        } else if to < from && index >= to && index < from {
            index + 1
        } else {
            index
        }
    };
    match selection {
        TableSelection::Column(col) => TableSelection::Column(remap(col)),
        TableSelection::Cell(row, col) => TableSelection::Cell(row, remap(col)),
        other => other,
    }
}

pub(crate) fn table_height(row_count: usize) -> gpui_kit::Pixels {
    // The table's horizontal scrollbar is an overlay at the bottom of its viewport.
    let rows = if row_count == 0 { 3 } else { row_count };
    PREVIEW_TABLE_SIZE.table_row_height() * (rows + 1) as f32 + Scrollbar::width() + px(2.)
}

impl Drop for TableTab {
    fn drop(&mut self) {
        self.cancel_pending();
    }
}

impl EventEmitter<PanelEvent> for TableTab {}
impl Focusable for TableTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl BasePanel for TableTab {
    fn panel_name(&self) -> &'static str {
        "TablePreview"
    }

    fn closable(&self, cx: &App) -> bool {
        !self.has_pending(cx)
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = group.upgrade() else {
            return;
        };
        let panel_id = PanelId::from(cx.entity().entity_id());
        self._tab_group_subscription = Some(cx.observe(&group, move |this, group, cx| {
            let group = group.read(cx);
            let show = group
                .panels()
                .iter()
                .position(|panel| panel.panel_id(cx) == panel_id)
                .is_some_and(|ix| ix > 0 && ix != group.active_ix() && ix - 1 != group.active_ix());
            if this.show_separator != show {
                this.show_separator = show;
                cx.notify();
            }
        }));
    }

    fn set_active(&mut self, _: bool, _: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_pending();
        cx.notify();
    }
}
impl Panel for TableTab {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Dock groups do not allow closing their last tab, so previews close
        // through the area directly, including when they are the last one.
        let name = self.tab_target.name.clone();
        let area = self.dock_area.clone();
        let panel = cx.entity();
        let separator = self.show_separator;
        let middle_click_area = area.clone();
        let middle_click_panel = panel.clone();
        div()
            // Match the native tab's 12px content inset so the title also
            // receives middle clicks in that padding, without changing its width.
            .mx(px(-12.))
            .px(px(12.))
            .h_full()
            .relative()
            .when(separator, |tab| {
                tab.child(
                    div()
                        .absolute()
                        .left_1()
                        .top_2()
                        .bottom_2()
                        .w(px(1.))
                        .bg(cx.theme().border),
                )
            })
            .flex()
            .gap_2()
            .items_center()
            .on_mouse_down(MouseButton::Middle, move |_, window, cx| {
                cx.stop_propagation();
                if middle_click_panel.read(cx).has_pending(cx) {
                    return;
                }
                let _ = middle_click_area.update(cx, |area, cx| {
                    area.remove_panel(middle_click_panel.clone(), window, cx)
                });
            })
            .child(IconName::Table2)
            .child(name.clone())
            .child(
                Button::new(("close-table-tab", panel.entity_id()))
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .disabled(self.has_pending(cx))
                    .accessibility_label(format!("Close {name}"))
                    .m_0()
                    .p_0()
                    .tooltip(format!("Close {name}"))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        if panel.read(cx).has_pending(cx) {
                            return;
                        }
                        let _ = area
                            .update(cx, |area, cx| area.remove_panel(panel.clone(), window, cx));
                    }),
            )
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for TableTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let table = self.table.clone();
        let (editable, changes, change_count) = table
            .as_ref()
            .map(|table| {
                let state = table.read(cx);
                let delegate = state.delegate();
                let editable = state
                    .selected_cell()
                    .and_then(|(row, col)| col.checked_sub(1).map(|col| (row, col)))
                    .is_some_and(|(row, col)| {
                        delegate.edit_snapshot.as_ref().is_some_and(|snapshot| {
                            snapshot.rows.get(row).is_some()
                                && snapshot
                                    .columns
                                    .get(col)
                                    .is_some_and(|column| column.editable)
                        })
                    });
                let changes = delegate
                    .edit_snapshot
                    .as_ref()
                    .map(|snapshot| {
                        delegate
                            .edits
                            .iter()
                            .map(|((row, col), value)| {
                                let old = &snapshot.rows[row].originals[col];
                                format!(
                                    "Row {} · {}: {} → {}",
                                    row + 1,
                                    snapshot.columns[col].name,
                                    review_value(old),
                                    review_value(value)
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                (editable, changes, delegate.edits.len())
            })
            .unwrap_or_default();

        let scope = format!(
            "Preview · {} / {} / {} / {}",
            self.tab_target.database.connection_name(),
            self.tab_target.database.name(),
            self.tab_target.schema,
            self.tab_target.name
        );
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .child(
                div()
                    .px_3()
                    .py_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(scope),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .child(
                        Button::new("apply-edits")
                            .small()
                            .success()
                            .icon(IconName::ArrowUp)
                            .accessibility_label("Apply staged changes")
                            .tooltip("Apply staged changes")
                            .disabled(
                                change_count == 0
                                    || self.edits.editing.is_some()
                                    || self.edits.applying
                                    || self.edits.outcome_unknown
                                    || self.preview_load.target.is_some(),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.apply_changes(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("discard-edits")
                            .small()
                            .ghost()
                            .icon(IconName::Close)
                            .accessibility_label("Discard staged changes and current edit")
                            .tooltip("Discard staged changes and current edit")
                            .disabled(
                                (change_count == 0 && self.edits.editing.is_none())
                                    || self.edits.applying,
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.discard_edits(window, cx)),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{change_count} staged")),
                    )
                    .child(
                        Button::new("edit-cell")
                            .small()
                            .ghost()
                            .label("Edit cell")
                            .tooltip("Edit selected cell (F2)")
                            .disabled(
                                !editable
                                    || self.edits.editing.is_some()
                                    || self.edits.applying
                                    || self.edits.outcome_unknown
                                    || self.preview_load.target.is_some(),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.begin_edit(window, cx)),
                            ),
                    )
                    .when(self.edits.editing.is_some(), |this| {
                        this.child(
                            Button::new("stage-null")
                                .small()
                                .ghost()
                                .label("Set SQL NULL")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.stage_edit(None, window, cx)
                                })),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Enter to stage · Esc to cancel"),
                        )
                    })
                    .when(self.edits.applying, |this| this.child("Applying…")),
            )
            .when(!changes.is_empty(), |this| {
                this.child(
                    div()
                        .id("staged-changes")
                        .px_3()
                        .py_1()
                        .max_h_24()
                        .overflow_y_scroll()
                        .text_sm()
                        .child("Staged changes (not yet applied)")
                        .children(changes.into_iter().map(|change| div().child(change))),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when_some(table, |this, table| {
                this.child(
                    div().flex_1().flex().flex_col().min_h_0().min_w_0().child(
                        div()
                            .id("preview-table-container")
                            .key_context("WorkspaceTable")
                            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                                if this.edits.editing.is_some() {
                                    this.cancel_edit(window, cx);
                                }
                            }))
                            .on_action(cx.listener(|this, _: &EditCell, window, cx| {
                                this.begin_edit(window, cx)
                            }))
                            .on_action({
                                let table = table.clone();
                                move |_: &CopySelection, _, cx| {
                                    let state = table.read(cx);
                                    if let Some(csv) =
                                        state.delegate().selection_csv(state.selection())
                                    {
                                        cx.write_to_clipboard(ClipboardItem::new_string(csv));
                                    }
                                }
                            })
                            .capture_key_down(cx.listener(
                                |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                                    if this.edits.editing.is_some()
                                        && event.keystroke.key == "escape"
                                    {
                                        this.cancel_edit(window, cx);
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                            .capture_key_down({
                                let table = table.clone();
                                let editing = self.edits.editing.is_some();
                                move |event, _, cx| {
                                    if editing {
                                        return;
                                    }
                                    let modifiers = event.keystroke.modifiers;
                                    if !modifiers.shift
                                        || modifiers.control
                                        || modifiers.alt
                                        || modifiers.platform
                                    {
                                        return;
                                    }
                                    let delta = match event.keystroke.key.as_str() {
                                        "up" => (-1, 0),
                                        "down" => (1, 0),
                                        "left" => (0, -1),
                                        "right" => (0, 1),
                                        _ => return,
                                    };
                                    let handled = table.update(cx, |table, cx| {
                                        let Some((row, col)) =
                                            table.selected_cell().filter(|(_, col)| *col > 0)
                                        else {
                                            return false;
                                        };
                                        let delegate = table.delegate_mut();
                                        let last_row = delegate.rows.len().saturating_sub(1);
                                        let last_col = delegate.columns.len();
                                        let next = (
                                            row.saturating_add_signed(delta.0).min(last_row),
                                            col.saturating_add_signed(delta.1).clamp(1, last_col),
                                        );
                                        let range = delegate.selection.get_or_insert(CellRange {
                                            anchor: (row, col),
                                            end: (row, col),
                                        });
                                        range.end = next;
                                        delegate.selecting = false;
                                        table.set_selected_cell(next.0, next.1, cx);
                                        table.scroll_to_row(next.0, cx);
                                        table.scroll_to_col(next.1, cx);
                                        true
                                    });
                                    if handled {
                                        cx.stop_propagation();
                                    }
                                }
                            })
                            .on_mouse_up(MouseButton::Left, {
                                let table = table.clone();
                                move |_, _, cx| {
                                    table.update(cx, |table, _| {
                                        table.delegate_mut().selecting = false
                                    })
                                }
                            })
                            .on_mouse_up_out(MouseButton::Left, {
                                let table = table.clone();
                                move |_, _, cx| {
                                    table.update(cx, |table, _| {
                                        table.delegate_mut().selecting = false
                                    })
                                }
                            })
                            .w_full()
                            .h(table_height(self.row_count))
                            .max_h_full()
                            .min_h_0()
                            .font_family("JetBrains Mono")
                            .text_xs()
                            .relative()
                            .child({
                                let table = table.clone();
                                let editing = self.edits.editing.is_some();
                                canvas(
                                    |bounds, window, _| {
                                        window.insert_hitbox(bounds, HitboxBehavior::Normal)
                                    },
                                    move |_, hitbox, window, _| {
                                        let line_height = window.line_height();
                                        window.on_mouse_event(
                                            move |event: &ScrollEvent, phase, window, cx| {
                                                if !phase.capture()
                                                    || !event.modifiers.control
                                                    || editing
                                                    || !hitbox.should_handle_scroll(window)
                                                {
                                                    return;
                                                }
                                                let delta = event.delta.pixel_delta(line_height);
                                                let horizontal = event.modifiers.shift
                                                    || delta.x.abs() > delta.y.abs();
                                                let moved = table.update(cx, |table, cx| {
                                                    let handle = if horizontal {
                                                        table
                                                            .horizontal_scroll_handle
                                                            .as_ref()
                                                            .clone()
                                                    } else {
                                                        table
                                                            .vertical_scroll_handle
                                                            .0
                                                            .borrow()
                                                            .base_handle
                                                            .clone()
                                                    };
                                                    let mut offset = handle.offset();
                                                    let max = handle.max_offset();
                                                    let (current, limit, distance) = if horizontal {
                                                        (
                                                            &mut offset.x,
                                                            max.x,
                                                            if delta.x == px(0.) {
                                                                delta.y
                                                            } else {
                                                                delta.x
                                                            },
                                                        )
                                                    } else {
                                                        (&mut offset.y, max.y, delta.y)
                                                    };
                                                    let next = accelerated_offset(
                                                        *current, limit, distance,
                                                    );
                                                    if next == *current {
                                                        return false;
                                                    }
                                                    *current = next;
                                                    handle.set_offset(offset);
                                                    cx.notify();
                                                    true
                                                });
                                                if moved {
                                                    cx.stop_propagation();
                                                }
                                            },
                                        );
                                    },
                                )
                                .absolute()
                                .inset_0()
                            })
                            .child(
                                DataTable::new(&table)
                                    .with_size(PREVIEW_TABLE_SIZE)
                                    .bordered(false)
                                    .stripe(true)
                                    .fixed_cols_border(true)
                                    .accessibility_label("Table preview"),
                            ),
                    ),
                )
            })
    }
}
