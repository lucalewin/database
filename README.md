# DB Viewer

## Run locally

Start the development-only PostgreSQL 17 instance with `docker compose up -d`, then run `cargo run -p viewer`. It binds to `127.0.0.1:5432` with database/user/password `dbviewer`/`dbviewer`/`dbviewer`, no TLS. The seed contains `public.customers`, `public.orders`, and `inventory.products` with sample rows and foreign keys.

There is no built-in connection. Open the Connections menu in the top bar and choose **New connection…**. Use the connection name in the top bar to switch connections, then select an accessible database and schema; the active environment appears in the status bar. Select tables in Explorer to open preview tabs of up to 500 rows each. Selecting an already open table activates and refreshes its tab in the workspace's dock tab strip; tabs can be selected, rearranged, or closed independently using the close button or a middle click on the tab title. Each tab retains its original connection, database, and schema as a snapshot even when the top-bar selection changes. An in-progress preview is interrupted if its scope changes. The numbered column shows 1-based positions in the current preview, not stable database row identifiers; click a row number to select the row. Drag across cells to select a rectangular range, or use Shift-click or Shift-arrow keys to extend a selection. Press `Ctrl+C` while the table is focused to copy selected values as CSV without headers or row numbers; a single column uses newlines rather than comma separators between rows. CSV quoting preserves commas, quotes, and line breaks in values. SQL NULL is copied as `NULL`. Copied values are not sanitized for spreadsheet formulas, so treat untrusted data carefully when pasting into spreadsheet software. The preview has no guaranteed ordering, and large values can consume significant memory. SQL NULL is shown as `NULL`; PostgreSQL's JSON conversion is used to display other values. Reselect a connection to retry an initial connection error, and select a table again to retry its preview. The current window does not expose a metadata refresh command; restart the app to reload metadata.

## Editing table previews

On Local and Staging, select an editable cell and press `F2`, double-click it, or use **Edit cell**. Enter stages a value, Shift+Enter inserts a newline, and Escape cancels the current edit. **Set SQL NULL** stages SQL NULL, which is distinct from an empty string. Review the staged values before applying or discarding them.

Editing is limited to supported built-in column types on ordinary PostgreSQL tables with a valid primary key; primary-key, generated, and identity columns are not editable. Production previews are readonly. Applying changes uses a transaction and checks the original table and row identities for conflicts. If the commit outcome is unknown, reload and verify the table before retrying. Tabs cannot be closed or refreshed while edits are pending; staged edits are not saved across app restarts.

## SQL scratchpad

The workspace supports multiple independent **SQL scratchpad** tabs with syntax-highlighted GPUI Kit editors, line numbers, and resizable results panes below each editor. Enter SQL and choose **Run SQL** or press `Ctrl+Enter`. Tree-sitter-based autocomplete suggests SQL keywords, accessible schemas and tables, and PostgreSQL columns loaded with the selected database metadata; suggestions open while typing and can be requested with `Ctrl+Space`. Results are filtered and ranked as you type—exact and prefix matches such as `products` appear first—while character-by-character matching highlights fuzzy matches. The completion catalog stores column types, nullability, defaults, identity/generated state, keys, descriptions, and optional PostGIS geometry/geography type, SRID, and dimensions. Column completions are limited to tables referenced by the current `FROM` and `JOIN`; aliases and `table.` qualification are supported. PostGIS-enabled schemas also receive `ST_*` function suggestions. Each scratchpad runs against the currently selected connection, database, and schema at execution time; the target is shown above its editor, and results retain their original scope if the selection changes during or after execution. Tabs can be closed, and drafts and results are not saved across app restarts.

The scratchpad executes arbitrary SQL, including writes, on Local and Staging. **Production execution is disabled**, including reads; the viewer does not yet offer a production unlock. This is an application convenience, not a security boundary: use restricted PostgreSQL roles. SQL can change data immediately. A script can contain multiple statements, and explicit commits may persist earlier changes even if a later statement fails. The entire editor is executed; there is no selection-based execution or confirmation. PostgreSQL's normal transaction behavior applies. A new disposable session is used for each run, so session settings, temporary objects, and transactions do not carry over. The selected schema is the initial `search_path` (or the connection default if none is selected). The initial statement timeout is 30 seconds; SQL can change it. COPY streaming is not supported.

Results appear in a table of PostgreSQL text values; use **Previous** and **Next** to navigate statement results. The fixed numbered column shows 1-based positions within each result, not database row identifiers. The editor and results use JetBrains Mono when installed. SQL NULL appears as `NULL`, while an empty string and the literal `NULL` appear quoted. Select a cell and press `Ctrl+C` to copy its underlying value (SQL NULL is copied as `NULL`). Display is limited to 500 rows in total, a 256 KiB result budget, 2 KiB per value, 512 columns per result, and 500 statements. These are **display limits only**: the server still executes the full SQL and all rows are drained, so use `LIMIT` where appropriate. Very large individual values can still consume significant memory. Empty result sets do not show column names. Errors and any partial results appear in the results area. Schema or data changes are not automatically reflected in Explorer or existing previews; reopen a table for its data, or restart the app to reload metadata.

The app starts with the embedded One Dark Pro theme from `themes/one-dark.json`, even when launched outside the project directory. When launched from the project directory, GPUI Kit also watches `themes/` for changes if `themes/one-dark.json` exists. Edit the theme's `colors` section for component colors and its `highlight` section for editor and syntax colors.

The named volume persists across restarts. The image runs `database/init.sql` only when initializing an empty volume. To reset **and delete all local data**, run `docker compose down -v`, then `docker compose up -d`. The fixed local credentials and Compose service are for local development only.

## Connections

PostgreSQL is the only supported database today. Internally, connection opening, metadata, health checks, SQL execution, table previews, and editing use driver interfaces and managed connection handles, providing a starting point for future engines such as DuckDB and MySQL. Preview and editing support is capability-specific; profiles and selection are still PostgreSQL-specific, and no other engine is implemented yet.

Open the Connections menu in the top bar and choose **New connection…**, then enter a name, a PostgreSQL URL such as `postgresql://dbviewer@127.0.0.1:5432/dbviewer`, and your password, choose the environment, and select **Add connection**. For the development Compose database, the password is `dbviewer`; choose Local and explicitly disable TLS. The application saves the profile and attempts to connect; errors appear in Explorer. It remembers the last _successful_ connection and tries that profile when restarted. Profiles load in the background at startup. The last connection is also saved in the background; on application quit, pending writes get a brief chance to finish, but a crash or slow storage may leave the previous selection remembered. With no saved last connection, select or add a connection. A failed switch leaves the previous connection active. In-app notifications report saved connections, connection or metadata failures, and failures to remember a connection; details also remain in Explorer. Table preview and apply failures and completed applies are also reported with notifications.

The password field stores the password in the system keyring, not in the profile file. Linux requires a running, unlocked Secret Service keyring (such as GNOME Keyring or KWallet); macOS uses Keychain and Windows uses Credential Manager. If the keyring is unavailable, the profile is not saved. Leave the password field empty for passwordless authentication or to use PostgreSQL's password file or `PGPASSWORD`. The URL must not contain a password, query parameters, or a fragment. Passwords may still be present in application memory while connecting.

Profiles are saved in `$XDG_CONFIG_HOME/dbviewer/connections.json`, or `$HOME/.config/dbviewer/connections.json` when `XDG_CONFIG_HOME` is unset or not absolute. Existing environment-variable URL profiles are still supported. For example:

```json
{
    "version": 1,
    "connections": [
        {
            "id": "development",
            "name": "Development",
            "tier": "local",
            "url_env": "DEV_DATABASE_URL",
            "tls": "disable"
        },
        {
            "id": "reporting",
            "name": "Reporting",
            "tier": "production",
            "url_env": "REPORTING_DATABASE_URL"
        }
    ]
}
```

For profiles using `url_env`, set the named environment variables to PostgreSQL URLs before starting the viewer. Each URL must specify a TCP host and database. IDs must be unique, stable, and use only ASCII letters, numbers, `-`, `_`, or `.`. Multiple profiles may share a tier. Profiles with missing or invalid URLs are disabled; configuration errors appear in Explorer. Adding is available in the app; editing and removing profiles still requires editing the file and restarting. Removing a profile manually does not delete its password: remove the corresponding `dbviewer.postgresql` entry from the system keyring as well. The previous implicit Local, Staging, and Production profiles are no longer created automatically; add them or define them in the file.

The `tier` is `local`, `staging`, or `production` and determines the safety restrictions, not connection quality. Production preview editing and SQL execution remain disabled; these are convenience guards, not security boundaries. Use restricted database roles even for previews, which can execute database-side code.

TLS defaults to `verify-full` (certificate chain and hostname verification against native system roots); `tls: "disable"` is allowed only for a `local` profile and transmits credentials and data without encryption. Configure a trusted CA for verified connections. If `PGHOSTADDR` is set, URLs must explicitly provide `hostaddr` to avoid silently connecting to that inherited address; use an environment-variable URL profile for this, since the dialog does not accept query parameters. Credentials are not stored in the profile file, but environment variables can expose them to processes or tools; avoid passwords in shell history or committed files.

The bottom status bar checks the active pool with `SELECT 1` every 30 seconds. It reports the time including pool acquisition: Good is under 100 ms, Fair is under 500 ms, and Slow is 500 ms or more. A probe that fails or takes over five seconds reports unavailable; it retries on the next interval. This is a point-in-time measurement, not a guarantee of future query performance or connectivity. A busy pool can also affect the measurement.

SSH tunneling is not implemented. Profiles currently use direct PostgreSQL connections; unsupported fields are rejected rather than ignored.

## SQL safety

Catalog filters use bound parameters; PostgreSQL identifiers in previews are quoted and escaped. Browsing can still invoke database-side code, including row-level policies and type functions. Use appropriately restricted PostgreSQL roles, especially in production. The application-level production restrictions are convenience guards, not a security boundary.

## Tests

Run the default validation locally (omit `--offline` if dependencies are not cached):

```sh
cargo fmt --all -- --check
cargo clippy --offline --locked --all-targets -- -D warnings
cargo test --offline --locked
```

`.github/workflows/validate.yml` also runs the existing database tests in separate seeded and disposable instances.

With the Compose database running, run the two seeded-database tests separately:

```sh
cargo test --offline -p viewer loads_seeded_metadata_and_rejects_missing_schema -- --ignored
cargo test --offline -p viewer runs_queries_and_keeps_session_state_out_of_the_pool -- --ignored
```

The edit integration test creates and modifies a table in a uniquely named schema and requires a **separate disposable PostgreSQL instance**. It drops the schema on success and on a test panic; a crash or forced termination can leave it behind. Set `DBVIEWER_TEST_SOCKET` to its Unix socket directory; the test connects as user `postgres` to database `postgres`, so local socket authentication must permit that connection. If the socket uses a non-default port, also set `PGPORT`:

```sh
DBVIEWER_TEST_SOCKET=/path/to/disposable/socket cargo test --offline -p viewer applies_atomically_and_rejects_conflicts -- --ignored
```

Do not point this test at a database containing data you need. Running all ignored tests together also requires both database setups.
