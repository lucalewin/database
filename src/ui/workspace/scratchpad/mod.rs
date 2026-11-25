use std::{cell::RefCell, rc::Rc};

use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, ParentElement as _, Render,
    Styled as _, Subscription, WeakEntity, Window, actions,
    assets::IconName,
    component::{
        ActiveTheme as _, Disableable as _, Sizable as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, DockArea, Panel, PanelEvent, PanelId, TabGroup},
        input::{Editor, EditorState, highlight_characters},
        resizable::{ResizableState, resizable_panel, v_resizable},
        table::{DataTable, TableState},
    },
    div,
    prelude::FluentBuilder as _,
    px,
};

use tokio::runtime::Handle;

use crate::database::query::QueryOutput;
use crate::session::{ConnectionTier, Session};
use crate::ui::workspace::table::{PREVIEW_TABLE_SIZE, columns_for_preview};

use crate::ui::workspace::table::WorkspaceTableDelegate;
use autocomplete::{SqlCompletionCatalog, SqlCompletionProvider};

mod autocomplete;

actions!(scratchpad, [RunSql, ShowSqlCompletions]);

pub(crate) fn init_scratchpad(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-enter", RunSql, Some("SqlScratchpad")),
        KeyBinding::new("ctrl-space", ShowSqlCompletions, Some("SqlScratchpad")),
    ]);
}

pub(crate) struct SqlScratchpad {
    tab_number: u64,

    dock_area: WeakEntity<DockArea>,
    show_separator: bool,
    _tab_group_subscription: Option<Subscription>,
    editor: Entity<EditorState>,
    split: Entity<ResizableState>,
    explorer: Entity<Session>,
    runtime: Handle,
    running: bool,
    result_scope: Option<String>,
    result: Option<QueryOutput>,
    result_error: Option<String>,
    selected_statement: usize,
    table: Option<Entity<TableState<WorkspaceTableDelegate>>>,
    _explorer_subscription: Subscription,
    _editor_subscription: Subscription,
}

impl SqlScratchpad {
    pub fn new(
        tab_number: u64,
        runtime: Handle,
        explorer: Entity<Session>,
        dock_area: Entity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let completion_catalog = Rc::new(RefCell::new(SqlCompletionCatalog::from_explorer(
            explorer.read(cx),
        )));
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .placeholder("Enter SQL to run on the selected database…");
            let lsp = state.lsp_mut();
            lsp.completion_provider = Some(Rc::new(SqlCompletionProvider::new(
                completion_catalog.clone(),
            )));
            lsp.set_completion_highlighter(Rc::new(highlight_characters));
            state
        });
        let catalog = completion_catalog.clone();
        let explorer_subscription = cx.observe(&explorer, move |_, state, cx| {
            *catalog.borrow_mut() = SqlCompletionCatalog::from_explorer(state.read(cx));
            cx.notify();
        });
        let editor_subscription = cx.observe(&editor, |_, _, cx| cx.notify());
        Self {
            tab_number,

            dock_area: dock_area.downgrade(),
            show_separator: false,
            _tab_group_subscription: None,
            editor,
            split: cx.new(|_| ResizableState::default()),
            explorer,
            runtime,
            running: false,
            result_scope: None,
            result: None,
            result_error: None,
            selected_statement: 0,
            table: None,
            _explorer_subscription: explorer_subscription,
            _editor_subscription: editor_subscription,
        }
    }

    fn select_statement(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_statement = ix;
        self.table = self
            .result
            .as_ref()
            .and_then(|output| output.statements.get(ix))
            .and_then(|statement| statement.table.as_ref())
            .map(|preview| {
                let mut columns = columns_for_preview(preview, window, cx);
                for (ix, column) in columns.iter_mut().enumerate() {
                    column.key = format!("sql-result-column-{ix}").into();
                }
                let delegate =
                    WorkspaceTableDelegate::new(preview.clone(), columns, window.rem_size() * 3.5);
                cx.new(|cx| {
                    TableState::new(delegate, window, cx)
                        .cell_selectable(true)
                        .row_header(false)
                        .row_selectable(false)
                        .col_selectable(false)
                        .col_movable(false)
                })
            });
        cx.notify();
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.explorer.read(cx);
        let sql = self.editor.read(cx).value().to_string();
        if self.running || sql.trim().is_empty() {
            return;
        }
        let Some(connection) = state.query_connection() else {
            return;
        };
        let Some(scope) = execution_scope(state) else {
            return;
        };
        let schema = state.schema().to_owned();
        self.running = true;
        self.result_scope = Some(scope);
        self.result = None;
        self.result_error = None;
        self.table = None;
        cx.notify();
        let task = self
            .runtime
            .spawn(async move { connection.execute_sql(schema, sql).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = task
                .await
                .unwrap_or_else(|error| Err(format!("Query task failed: {error}")));
            let _ = this.update_in(cx, |this, window, cx| {
                this.running = false;
                match result {
                    Ok(result) => {
                        this.result = Some(result);
                        this.select_statement(0, window, cx);
                    }
                    Err(error) => {
                        this.result_error = Some(format!("Query failed: {error}"));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

fn execution_scope(state: &Session) -> Option<String> {
    let name = state.active_connection_name()?;
    let tier = state.active_connection_tier()?;
    let id = state.active_connection_id()?;
    let connection = if state
        .profiles()
        .iter()
        .filter(|profile| profile.name() == name)
        .count()
        > 1
    {
        format!("{name} ({id})")
    } else {
        name.to_owned()
    };
    let schema = if state.schema().is_empty() {
        "default search path"
    } else {
        state.schema()
    };
    Some(format!(
        "{connection} · {} / {} / {schema}",
        tier.label(),
        state.database()
    ))
}

impl EventEmitter<PanelEvent> for SqlScratchpad {}
impl Focusable for SqlScratchpad {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}
impl BasePanel for SqlScratchpad {
    fn panel_name(&self) -> &'static str {
        // Stable dock identity; the Rust view was renamed without changing persisted layouts.
        "SqlConsole"
    }
    fn closable(&self, _: &App) -> bool {
        true
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
}
impl Panel for SqlScratchpad {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = format!("SQL scratchpad {}", self.tab_number);
        let area = self.dock_area.clone();
        let panel = cx.entity();
        let separator = self.show_separator;
        let middle_click_area = area.clone();
        let middle_click_panel = panel.clone();
        div()
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
                let _ = middle_click_area.update(cx, |area, cx| {
                    area.remove_panel(middle_click_panel.clone(), window, cx)
                });
            })
            .child(IconName::Terminal)
            .child(name.clone())
            .child(
                Button::new(("close-sql-scratchpad", panel.entity_id()))
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .accessibility_label(format!("Close {name}"))
                    .m_0()
                    .p_0()
                    .tooltip(format!("Close {name}"))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let _ = area
                            .update(cx, |area, cx| area.remove_panel(panel.clone(), window, cx));
                    }),
            )
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
impl Render for SqlScratchpad {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.explorer.read(cx);
        let production = state.active_connection_tier() == Some(ConnectionTier::Production);
        let disabled = self.running
            || state.query_connection().is_none()
            || self.editor.read(cx).value().trim().is_empty();
        let target = execution_scope(state)
            .map(|scope| format!("Target · {scope}"))
            .unwrap_or_else(|| "No connection selected".to_owned());
        let status = if self.running {
            "Running SQL…"
        } else if production {
            "SQL execution is unavailable on Production"
        } else if state.loading() {
            "Waiting for connection selection…"
        } else if state.active_connection_tier().is_none() {
            "Connect to a database to run SQL"
        } else {
            "SQL can change data · Ctrl+Enter to run"
        };
        let min_height = window.rem_size() * 8.;
        let count = self
            .result
            .as_ref()
            .map_or(0, |result| result.statements.len());
        let statement = self
            .result
            .as_ref()
            .and_then(|result| result.statements.get(self.selected_statement));
        let summary =
            statement.map(
                |statement| match (statement.table.as_ref(), statement.rows_affected) {
                    (Some(table), Some(affected)) => format!(
                        "{} displayed · {affected} rows returned or affected",
                        table.rows.len()
                    ),
                    (Some(table), None) => {
                        format!("{} displayed · statement incomplete", table.rows.len())
                    }
                    (None, Some(affected)) => format!("{affected} rows returned or affected"),
                    (None, None) => "Statement incomplete".into(),
                },
            );
        let error = self.result_error.as_ref().or_else(|| {
            self.result
                .as_ref()
                .and_then(|result| result.error.as_ref())
        });
        let truncated = self.result.as_ref().is_some_and(|result| result.truncated);
        let empty = if self.running {
            "Running SQL…"
        } else if self.result.is_none() && error.is_none() {
            "Run SQL to see results"
        } else if count == 0 && error.is_none() {
            "No statement was executed"
        } else if self.table.is_none() {
            "No rows to display"
        } else {
            ""
        };
        div().key_context("SqlScratchpad")
            .on_action(cx.listener(|this, _: &RunSql, window, cx| this.run(window, cx)))
            .on_action(cx.listener(|this, _: &ShowSqlCompletions, window, cx| {
                this.editor
                    .update(cx, |editor, cx| editor.show_completions(window, cx));
            }))
            .size_full().min_h_0().min_w_0().bg(cx.theme().background).text_color(cx.theme().foreground)
            .child(v_resizable("sql-scratchpad-split").with_state(&self.split)
                .child(resizable_panel().size_range(min_height..gpui_kit::Pixels::MAX).child(
                    div().size_full().flex().flex_col().min_h_0().min_w_0()
                        .child(div().flex().items_center().gap_2().px_3().py_1()
                            .child(Button::new("run-sql").small().label("Run SQL").disabled(disabled).on_click(cx.listener(|this, _, window, cx| this.run(window, cx))))
                            .child(div().text_sm().child("Query")))
                        .child(div().px_3().pb_2().text_sm().text_color(cx.theme().muted_foreground).child(target).child(div().child(status)))
                        .child(div().flex_1().min_h_0().min_w_0().child(Editor::new(&self.editor).rounded_none().h_full().font_family("JetBrains Mono").aria_label("SQL query")))
                ))
                .child(resizable_panel().size_range(min_height..gpui_kit::Pixels::MAX).child(
                    div().size_full().flex().flex_col().min_h_0().min_w_0()
                        .child(div().px_3().py_1().text_sm()
                            .child(div().flex().items_center().gap_2().child("Results")
                                .when(count > 1, |bar| bar
                                    .child(Button::new("previous-result").small().label("Previous").disabled(self.selected_statement == 0).on_click(cx.listener(|this, _, window, cx| this.select_statement(this.selected_statement.saturating_sub(1), window, cx))))
                                    .child(format!("Statement {} of {count}", self.selected_statement + 1))
                                    .child(Button::new("next-result").small().label("Next").disabled(self.selected_statement + 1 >= count).on_click(cx.listener(|this, _, window, cx| this.select_statement(this.selected_statement + 1, window, cx)))))
                                .when(count == 1, |bar| bar.child("Statement 1"))
                                .when_some(summary, |bar, summary| bar.child(div().text_color(cx.theme().muted_foreground).child(summary))))
                            .when_some(self.result_scope.as_ref(), |bar, scope| bar.child(div().text_color(cx.theme().muted_foreground).child(scope.clone())))
                            .when(truncated, |bar| bar.child(div().text_color(cx.theme().warning).child("Results truncated (500 rows, 256 KiB, 2 KiB per value, 512 columns, or 500 statements). Display limits do not limit SQL execution.")))
                            .when_some(error, |bar, error| bar.child(div().text_color(cx.theme().danger).child(format!("{error} Earlier statements may already have committed.")))))
                        .when(self.table.is_none(), |panel| panel.child(div().px_3().py_2().text_sm().text_color(cx.theme().muted_foreground).child(empty)))
                        .when_some(self.table.clone(), |panel, table| panel.child(
                            div().flex_1().min_h_0().min_w_0().font_family("JetBrains Mono").text_xs().key_context("WorkspaceTable")
                                .on_action({
                                    let table = table.clone();
                                    move |_: &super::CopySelection, _, cx| {
                                        let state = table.read(cx);
                                        if let Some(csv) = state.delegate().selection_csv(state.selection()) {
                                            cx.write_to_clipboard(ClipboardItem::new_string(csv));
                                        }
                                    }
                                })
                                .child(DataTable::new(&table).with_size(PREVIEW_TABLE_SIZE).bordered(false).stripe(true).fixed_cols_border(true).accessibility_label("SQL results"))
                        ))
                )))
    }
}
