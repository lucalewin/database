# GPUI context for DB Viewer

This document is a handoff for agents working on this repository's GPUI UI. It records project-specific patterns and pitfalls discovered in the current implementation. It is not a replacement for the installed `gpui-kit` and `gpui-kit-design-guides` skills or the current GPUI Kit source; verify exact APIs there before changing code.

## Source of truth

- This project uses `gpui-kit` 0.11.x as a single application dependency.
- Import GPUI through `gpui_kit::*` and component layers through `gpui_kit::component` / `gpui_kit::base` / `gpui_kit::assets`.
- Exact signatures must be checked in the installed registry source under `~/.cargo/registry/src/.../gpui-*-0.11.0/`.
- First use the installed `gpui-kit` and `gpui-kit-design-guides` skills. Then use targeted pages such as `https://gpui-kit.com/component/editor.md`; do not fetch `llms-full.txt`.

## Repository layout

- `src/main.rs`: application bootstrap and root shell composition.
- `src/session/`: active connection/database/schema session state and metadata lifecycle. This is not UI state.
- `src/connection/`: profile, credentials, persistence, and connection capabilities.
- `src/database/`: backend operations and driver-facing types.
- `src/ui/shell/`: top bar and status bar.
- `src/ui/connections/`: connection dialog and connection-quality UI.
- `src/ui/explorer/`: Explorer panel and table targets exposed for opening/dragging.
- `src/ui/workspace/`: dock coordinator and all workspace tabs.
- `src/ui/workspace/table/`: shared table tab, delegate, edit workflow, preview load, and sizing.
- `src/ui/workspace/scratchpad/`: SQL scratchpad and completion provider.

Do not move session or persistence state back under `src/ui/`.

## State and lifecycle rules

- GPUI state that survives frames belongs in an `Entity<T>`; do not recreate entities, focus handles, subscriptions, editors, or table state from `render`.
- `render` must stay declarative. Mutations, async starts, parsing, and database work belong in named methods or services.
- After coherent rendering mutations call `cx.notify()` once.
- Preserve subscriptions in fields. The `_...subscription` naming is intentional: a stored subscription must not be dropped.
- Async results must be guarded against obsolete revisions, scopes, connections, and closed entities.
- `Workspace` owns the dock and tab collections. `TableTab` owns table preview/edit lifecycle. `SqlScratchpad` owns editor/results/execution state.

## Imports and visibility

Project policy:

- Do not use `use super::...` in UI modules to re-import framework or application types through a parent.
- Import external types from `gpui_kit`, `lsp_types`, and other crates directly.
- Import project types from their owning path, for example `crate::session::Session` and `crate::ui::workspace::table::WorkspaceTableDelegate`.
- Use `pub`, `pub(crate)`, or private visibility. Do not introduce `pub(in ...)` or `pub(super)`.
- `#[cfg(test)] mod tests` and test modules may import directly from the module under test by crate path; avoid wildcard parent imports.

## Dock and tabs

- Workspace center content is represented by dock panels in a `DockArea` with `PanelStyle::TabBar`.
- A panel implements `EventEmitter<PanelEvent>`, `Focusable`, GPUI Base `BasePanel`, and component `Panel`.
- `panel_name()` is persisted dock identity. It must not be renamed casually. `SqlScratchpad` intentionally keeps the stable historical name `SqlConsole`.
- The default dock cannot reliably close a group's last tab. Workspace table/scratchpad tabs render their own close buttons and remove themselves through `DockArea::remove_panel`.
- Middle-click removal uses `on_mouse_down(MouseButton::Middle, ...)` on a title that expands into the tab's 12px padding with `mx(-12)` / `px(12)`.
- Custom tab separators are inset absolute children, not borders. Borders combine with the dock's native edges and caused doubled lines next to the active tab.
- Never read a `TabGroup` from a panel title render. Doing so can re-enter and panic with “cannot read ... TabGroup while it is already being updated.”
- If tab-relative presentation is needed, retain a `WeakEntity<TabGroup>` only long enough to install `cx.observe`; cache the derived boolean in the panel and read only local state during title rendering.

## Explorer drag and drop

- Table drag payloads contain `DatabaseTarget`, schema, and table name.
- `TableDrag` implements `Render` as the floating preview.
- Tree table rows use `ListItem::on_drag`.
- The workspace shell uses `drag_over::<TableDrag>` and `on_drop` on the full dock region.
- Drop still calls `Workspace::open_table`; stale `DatabaseTarget` generation/scope checks remain authoritative.

## Shared workspace table

`WorkspaceTableDelegate` in `src/ui/workspace/table/delegate.rs` is used by both database table previews and SQL result tables.

Important state:

- `columns` and `rows` contain display-order data. Moving columns also moves each row's values.
- Column zero is a synthetic row-number column; database result coordinates start at column 1.
- `CellRange` always uses table coordinates `(row, column including row header offset)`.
- Staged edits and database snapshots use data coordinates `(row, col - 1)`.
- Scratchpad result tables use the same delegate but have no edit snapshot.
- Table copy is CSV-based and includes rectangular range selection; SQL NULL copies as `NULL`.

Rendering details:

- Table size is `Size::XSmall` through `PREVIEW_TABLE_SIZE`.
- Column auto-width measures `single_line_display` and is capped only for initial width; users can resize beyond the initial cap.
- Selection uses `cx.theme().table_active`, configured as a dark blue `#315b7d` in `themes/one-dark.json`.
- A selected cell renders an absolute inner surface over `table_cell_padding()` so adjacent selected columns have one continuous fill with no padding gap.
- Do not use raw selection colors in application code; use the theme token.

## Cell editing

The current cell editor is deliberately a single-line `InputState` / `Input`, not a `Textarea`.

Why:

- A one-row Textarea creates a scrollbar that dominates the row.
- A single-line input keeps long values horizontally scrollable without wrapping or a multiline viewport.
- `InputState::submit_on_enter(true)` emits Enter without inserting a newline.

Workflow:

- Double-click emits `TableEvent::DoubleClickedCell`; the subscription calls `begin_edit_at`.
- F2 dispatches `EditCell` and calls `begin_edit` for the selected cell.
- `begin_edit_at` checks the backend edit snapshot, loads the original/staged value, sets `edits.editing`, updates the delegate, clears table selection, and focuses the input.
- The editor is rendered in `deferred(...)` so it can paint over subsequent cells; this is required for the DataGrip-like expanded width.
- The overlay is explicitly opaque with `cx.theme().background` on both wrapper and Input. Do not use a translucent selection token for the editor surface.
- Input/cell styling matches `XSmall`, `JetBrains Mono`, `text_xs`, zero padding, no border/appearance.
- The editor spans its cell plus up to the next column and has a minimum 12rem width.
- Enter stages the edit. Escape cancels and restores selection to the cell.
- Clicking another table cell cancels the old edit and continues the click; it can start editing the new cell on double-click.
- Clicking inside the editor must not cancel it. The `InputEvent::Blur` subscription and true outside-container interaction own dismissal.
- `begin_edit_at` clears selection, which emits `TableEvent::ClearSelection`. Never interpret that initial event as an outside-click cancellation; doing so closes editing immediately and broke both double-click and F2.

Editable schema policy:

- `editable_metadata` in PostgreSQL loads `name`, schema-qualified type, and generated/identity writability for table columns.
- All writable database columns are currently editable, including primary-key columns and user-defined types. Generated/identity columns remain readonly.
- Update casts use safely quoted qualified database type names (`schema.type`), not a `pg_catalog` whitelist.
- Optimistic concurrency still checks `tableoid`, `ctid`, `xmin`, and primary-key values before update.

## SQL autocomplete

- `CompletionProvider` is attached through `EditorState::lsp_mut().completion_provider`.
- Manual Ctrl+Space requests use `state.show_completions(window, cx)`; it replaces neither focus nor the selected item incorrectly.
- Provider uses `tree-sitter-sequel` to detect relation vs. column context and suppress comments/strings.
- It caches schemas/tables/rich column metadata from the selected session; never query the database per keystroke.
- Ranking is provider-owned: exact > prefix > contains > subsequence. Keep the best item first because GPUI's completion menu starts at index 0 and preserves provider order.
- Use completion `text_edit` ranges, not only `insert_text`; otherwise typed prefixes duplicate (`pr` + `products` -> `prproducts`).
- Keyword/table completions append a space when appropriate. Column completions do not.
- Qualified aliases are mapped to referenced table names before filtering columns.
- For fuzzy match highlighting use `highlight_characters` through the editor LSP completion highlighter.

## Focus, overlays, and events

- `InputEvent::Focus` / `Blur` are the correct lifecycle signals for an editor losing focus.
- Clicking elsewhere in the same table may not produce the same blur timing as clicking outside the table. Handle selection clicks explicitly when an edit is active.
- `cx.stop_propagation()` is for narrow nested interactions only. It is currently used on table cells and editor hit targets to stop table-level click handling from replacing selection/edit state.
- `deferred(...)` paints later and is useful for overlays extending over neighboring cells. A deferred element still needs an opaque background; it does not remove transparency from theme colors.
- `on_mouse_down_out` is useful for an exact element boundary, but coordinate/blur semantics should be verified because the popup/editor may be deferred and input focus changes can race element hit testing.

## Theme policy

- Use semantic `cx.theme()` tokens, not literals in application UI.
- The project embeds `themes/one-dark.json` as its default theme and watches the local file when launched from the repository.
- Current relevant table values:
  - selection: `table_active`
  - theme JSON key: `table.active.background`
  - current dark blue: `#315b7d`
  - normal background: `background`
  - edit/focus border: `primary`

## Validation

Run from the repository root:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

Targeted workflow checks:

```sh
cargo test ui::workspace
cargo test ui::workspace::table
```

Ignored tests require external PostgreSQL fixtures. Report that they were not run; do not claim they passed.

## Known sharp edges

- Do not restore the old live `TabGroup` read from panel render/title.
- Do not revert to generic `selection` for the table highlight; use `table_active`.
- Do not use `TextareaState` for inline cells; it produces scrollbar/layout problems in one-row grid cells.
- Do not cancel editing on the table's `ClearSelection` event emitted by `begin_edit_at`.
- Do not base editability on a PostgreSQL `pg_catalog` type whitelist.
- Be careful with `TableEvent` re-entrancy and mutably updating `TableState` from its own subscription.
- If an editing regression appears, trace the event sequence: mouse down -> `SelectCell` / `ClearSelection` -> `DoubleClickedCell` -> `begin_edit_at` -> `Input::Focus` / `Blur`.
