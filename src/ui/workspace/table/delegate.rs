use gpui_kit::component::{
    Sizable,
    input::{Input, InputState},
    table::{Column, TableDelegate, TableEvent, TableSelection, TableState},
};
use gpui_kit::{
    App, Context, Entity, Focusable as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement as _, Pixels, SharedString, Window, deferred, div, px,
};

use crate::database::table::{EditSnapshot, StagedEdits, TablePreview};
use crate::ui::workspace::table::preview_cell_display;
use gpui_kit::{
    InteractiveElement as _, StatefulInteractiveElement as _, Styled as _,
    component::ActiveTheme as _, prelude::FluentBuilder as _,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CellRange {
    pub(crate) anchor: (usize, usize),
    pub(crate) end: (usize, usize),
}

impl CellRange {
    pub(crate) fn contains(&self, row: usize, col: usize) -> bool {
        (self.anchor.0.min(self.end.0)..=self.anchor.0.max(self.end.0)).contains(&row)
            && (self.anchor.1.min(self.end.1)..=self.anchor.1.max(self.end.1)).contains(&col)
    }
}

pub(crate) struct WorkspaceTableDelegate {
    pub(crate) row_number_column: Column,
    pub(crate) columns: Vec<Column>,
    pub(crate) rows: Vec<Vec<Option<String>>>,
    pub(crate) selection: Option<CellRange>,
    pub(crate) selecting: bool,
    pub(crate) edit_snapshot: Option<EditSnapshot>,
    pub(crate) edits: StagedEdits,
    pub(crate) editor: Option<Entity<InputState>>,
    pub(crate) editing: Option<(usize, usize)>,
}

impl WorkspaceTableDelegate {
    pub(crate) fn new(
        preview: TablePreview,
        columns: Vec<Column>,
        row_number_width: Pixels,
    ) -> Self {
        let mut key = String::from("__preview_row_number");
        while columns.iter().any(|column| column.key.as_ref() == key) {
            key.push('_');
        }
        Self {
            row_number_column: Column::new(key, "#")
                .width(row_number_width)
                .fixed_left()
                .text_right()
                .resizable(false)
                .movable(false),
            columns,
            rows: preview.rows,
            selection: None,
            selecting: false,
            edit_snapshot: None,
            edits: StagedEdits::default(),
            editor: None,
            editing: None,
        }
    }

    pub(crate) fn begin_selection(&mut self, point: (usize, usize), extend: bool) {
        let anchor = if extend {
            self.selection.map_or(point, |range| range.anchor)
        } else {
            point
        };
        self.selection = Some(CellRange { anchor, end: point });
        self.selecting = true;
    }

    pub(crate) fn extend_selection(&mut self, point: (usize, usize)) -> bool {
        let Some(range) = &mut self.selection else {
            return false;
        };
        if range.end == point {
            return false;
        }
        range.end = point;
        true
    }

    pub(crate) fn selection_csv(&self, selection: TableSelection) -> Option<String> {
        let last_row = self.rows.len().checked_sub(1)?;
        let last_col = self.columns.len();
        if last_col == 0 {
            return None;
        }
        let range = match selection {
            TableSelection::Cell(row, col) if row <= last_row && (1..=last_col).contains(&col) => {
                self.selection
                    .filter(|range| range.end == (row, col))
                    .unwrap_or(CellRange {
                        anchor: (row, col),
                        end: (row, col),
                    })
            }
            TableSelection::Row(row) if row <= last_row => CellRange {
                anchor: (row, 1),
                end: (row, last_col),
            },
            TableSelection::Column(col) if (1..=last_col).contains(&col) => CellRange {
                anchor: (0, col),
                end: (last_row, col),
            },
            _ => return None,
        };
        let first_row = range.anchor.0.min(range.end.0);
        let last_row = range.anchor.0.max(range.end.0);
        let first_col = range.anchor.1.min(range.end.1);
        let last_col = range.anchor.1.max(range.end.1);
        if last_row >= self.rows.len() || first_col == 0 || last_col > self.columns.len() {
            return None;
        }

        let mut csv = String::new();
        for row_ix in first_row..=last_row {
            if row_ix > first_row {
                csv.push('\n');
            }
            for col_ix in first_col..=last_col {
                if col_ix > first_col {
                    csv.push(',');
                }
                let value = self.value_at(row_ix, col_ix - 1).unwrap_or("NULL");
                append_csv_value(&mut csv, value);
            }
        }
        Some(csv)
    }

    pub(crate) fn column_at(&self, col_ix: usize) -> &Column {
        match col_ix.checked_sub(1) {
            Some(ix) => &self.columns[ix],
            None => &self.row_number_column,
        }
    }

    pub(crate) fn value_at(&self, row: usize, col: usize) -> Option<&str> {
        match self.edits.get(row, col) {
            Some(value) => value.as_deref(),
            None => self.rows[row][col].as_deref(),
        }
    }

    pub(crate) fn cell_text_at(&self, row_ix: usize, col_ix: usize) -> String {
        match col_ix.checked_sub(1) {
            None => (row_ix + 1).to_string(),
            Some(ix) => self.value_at(row_ix, ix).unwrap_or("NULL").to_owned(),
        }
    }

    pub(crate) fn cell_label_at(&self, row_ix: usize, col_ix: usize) -> String {
        match col_ix.checked_sub(1) {
            None => format!("Row {}", row_ix + 1),
            Some(ix) => self.value_at(row_ix, ix).unwrap_or("SQL NULL").to_owned(),
        }
    }

    pub(crate) fn move_column_to(&mut self, col_ix: usize, to_ix: usize) {
        let (Some(col_ix), Some(to_ix)) = (col_ix.checked_sub(1), to_ix.checked_sub(1)) else {
            return;
        };
        if col_ix >= self.columns.len() || to_ix >= self.columns.len() {
            return;
        }
        let column = self.columns.remove(col_ix);
        self.columns.insert(to_ix, column);
        for row in &mut self.rows {
            let value = row.remove(col_ix);
            row.insert(to_ix, value);
        }
    }
}

pub(crate) fn review_value(value: &Option<String>) -> String {
    match value {
        None => "SQL NULL".into(),
        Some(value) => {
            let mut chars = value.chars();
            let prefix: String = chars.by_ref().take(160).collect();
            let suffix = if chars.next().is_some() { "…" } else { "" };
            format!("{:?}{suffix}", prefix)
        }
    }
}

pub(crate) fn accelerated_offset(current: Pixels, max: Pixels, delta: Pixels) -> Pixels {
    (current + delta * 4.).clamp(-max.max(px(0.)), px(0.))
}

pub(crate) fn append_csv_value(csv: &mut String, value: &str) {
    if value.is_empty() || value.contains([',', '"', '\r', '\n']) {
        csv.push('"');
        for ch in value.chars() {
            if ch == '"' {
                csv.push('"');
            }
            csv.push(ch);
        }
        csv.push('"');
    } else {
        csv.push_str(value);
    }
}

impl TableDelegate for WorkspaceTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len() + 1
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.column_at(col_ix).clone()
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, _: &App) -> Option<SharedString> {
        Some(self.cell_label_at(row_ix, col_ix).into())
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.cell_text_at(row_ix, col_ix)
    }

    fn move_column(
        &mut self,
        col_ix: usize,
        to_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
        self.move_column_to(col_ix, to_ix);
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if col_ix == 0 {
            return div()
                .id(("workspace-table-row-number", row_ix))
                .w_full()
                .text_right()
                .text_color(cx.theme().muted_foreground)
                .on_click(cx.listener(move |table, _, _, cx| {
                    cx.stop_propagation();
                    if table.delegate().editing.is_none() {
                        table.set_selected_row(row_ix, cx);
                    }
                }))
                .child(self.cell_text_at(row_ix, col_ix))
                .into_any_element();
        }
        if self.editing == Some((row_ix, col_ix - 1))
            && let Some(editor) = &self.editor
        {
            let minimum_width = window.rem_size() * 12.;
            let editor_width = self.columns[col_ix - 1..]
                .iter()
                .take(2)
                .fold(px(0.), |width, column| width + column.width)
                .max(minimum_width);
            return div()
                .relative()
                .size_full()
                .child(deferred(
                    div()
                        .id(format!("workspace-table-editor-{row_ix}-{col_ix}"))
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(editor_width)
                        .overflow_hidden()
                        .bg(cx.theme().background)
                        .border_1()
                        .border_color(cx.theme().primary)
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            Input::new(editor)
                                .xsmall()
                                .appearance(false)
                                .bordered(false)
                                .w_full()
                                .bg(cx.theme().background)
                                .font_family("JetBrains Mono")
                                .text_xs()
                                .p_0()
                                .rounded_none()
                                .h_full()
                                .aria_label("Cell value"),
                        ),
                ))
                .into_any_element();
        }

        let value = self.value_at(row_ix, col_ix - 1);
        let staged = self.edits.get(row_ix, col_ix - 1).is_some();
        let selected = self
            .selection
            .is_some_and(|range| range.contains(row_ix, col_ix));
        let padding = crate::ui::workspace::table::PREVIEW_TABLE_SIZE.table_cell_padding();
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id(format!("workspace-table-cell-{row_ix}-{col_ix}"))
                    .absolute()
                    .left(-padding.left)
                    .right(-padding.right)
                    .top(-padding.top)
                    .bottom(-padding.bottom)
                    .px(padding.left)
                    .py(padding.top)
                    .min_w_0()
                    .truncate()
                    .when(selected, |cell| cell.bg(cx.theme().table_active))
                    .when(staged, |cell| {
                        cell.border_b_1().border_color(cx.theme().primary)
                    })
                    .when(value.is_none(), |cell| {
                        cell.text_color(cx.theme().muted_foreground)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |table, event: &MouseDownEvent, window, cx| {
                            if table.delegate().editing.is_some() {
                                cx.emit(TableEvent::ClearSelection);
                            }
                            table
                                .delegate_mut()
                                .begin_selection((row_ix, col_ix), event.modifiers.shift);
                            table.set_selected_cell(row_ix, col_ix, cx);
                            table.focus_handle(cx).focus(window, cx);
                            if event.click_count == 2 {
                                cx.emit(TableEvent::DoubleClickedCell(row_ix, col_ix));
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_move(cx.listener(move |table, event: &MouseMoveEvent, _, cx| {
                        let delegate = table.delegate_mut();
                        if event.dragging()
                            && delegate.selecting
                            && delegate.extend_selection((row_ix, col_ix))
                        {
                            table.set_selected_cell(row_ix, col_ix, cx);
                        }
                    }))
                    .child(preview_cell_display(value).into_owned()),
            )
            .into_any_element()
    }
}
