# Agent instructions for DB Viewer

## Read first

Before changing GPUI UI code, read `docs/GPUI_CONTEXT.md` and use the installed `gpui-kit` and `gpui-kit-design-guides` skills. Verify exact framework APIs in the installed 0.11.x source; do not invent GPUI methods from web/CSS/React conventions.

## Architecture

- `src/session/` owns active connection/database/schema state and metadata lifecycle; it is not UI state.
- `src/connection/` owns profiles, credentials, persistence, and connection capabilities.
- `src/database/` owns backend operations and driver-facing types.
- `src/ui/workspace/` owns the dock and all workspace tab UI.
- `src/ui/workspace/table/` owns the shared table delegate, table tab, edit workflow, preview load, and sizing.
- `src/ui/workspace/scratchpad/` owns SQL scratchpad and autocomplete UI.

Preserve these boundaries and avoid moving session state back under `src/ui/`.

## GPUI and Rust conventions

- Do not use `use super::...` to reach external or project types through a parent module. Import types from their owning crate/module path.
- Use `pub`, `pub(crate)`, or private visibility. Do not add `pub(super)` or `pub(in ...)`.
- Retain entities, subscriptions, focus handles, editors, and table state across frames. Do not rebuild them in `render`.
- Keep render code declarative; domain work and mutations belong in named methods.
- Preserve stable domain-derived IDs for repeated elements.
- Use GPUI theme tokens and rem/semantic size helpers. Avoid raw UI colors and isolated sizing literals.
- Follow the stable dock `panel_name()` contract; `SqlScratchpad` intentionally keeps `SqlConsole` as persisted identity.

## Workspace-specific invariants

- Never read `TabGroup` from panel render/title code while the dock can be updating it. Observe the group and cache derived presentation state.
- `WorkspaceTableDelegate` is shared by database previews and SQL results.
- Column zero is a synthetic row-number column. Editing coordinates are offset by one from table selection coordinates.
- Do not cancel editing on the initial `ClearSelection` caused by `begin_edit_at`.
- Inline cell editing uses a single-line `Input`, not a `Textarea`.
- The editor overlay is deferred/opaque and may expand over adjacent columns.
- Use `table_active` for selected table cells and `background` for the editor surface.
- PostgreSQL editability is determined from generated/identity status and qualified database types; preserve optimistic row checks.

See `docs/GPUI_CONTEXT.md` for the detailed event, dock, table, completion, and focus patterns.

## Validation

After a focused change, run the smallest relevant tests first. Before finishing UI work, run:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

Useful targeted commands include:

```sh
cargo test ui::workspace
cargo test ui::workspace::table
```

Do not claim ignored PostgreSQL integration tests passed unless their required fixtures were actually run.
