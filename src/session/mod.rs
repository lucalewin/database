use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

use gpui_kit::{AsyncApp, Context, EventEmitter};
use tokio::runtime::Handle;

use crate::connection::manager::{ConnectionManager, ManagedConnection};
use crate::database::driver::ColumnMetadata;

use crate::connection::postgres::{
    ConnectionConfig, PreparedProfile, ProfileSaveResult, load_profiles, read_last_connection,
    restore_connection,
};
use crate::connection::remember::LastConnectionWriter;
pub use crate::connection::{ConnectionTier, postgres::ConnectionProfile};

#[cfg(test)]
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionEvent {
    LoadFailed(String),
    LoadSucceeded,
    ConfigurationFailed(String),
    RememberFailed(String),
}

impl EventEmitter<SessionEvent> for Session {}

enum ConnectionSelection {
    Load(Box<ConnectionConfig>),
    Changed,
    Invalid,
    Unchanged,
}

struct Metadata {
    connection: ConnectionConfig,
    session: ManagedConnection,
    database: String,
    schema: String,
    databases: Vec<String>,
    schemas: Vec<String>,
    tables: Vec<String>,
    columns: BTreeMap<String, Vec<ColumnMetadata>>,
}

/// Retained metadata for the active PostgreSQL connection.
///
/// Selections and refreshes retain the last successful snapshot until the new
/// snapshot is complete. An empty database or schema means none is selected.
pub struct Session {
    runtime: Handle,
    profiles: Vec<ConnectionProfile>,
    profiles_loading: bool,
    remember_writer: LastConnectionWriter,
    config_error: Option<String>,
    metadata: Option<Metadata>,
    revision: u64,
    loading: bool,
    error: Option<String>,
}

impl Session {
    /// Loads profiles off the UI thread, then restores the last successful connection.
    pub fn new(runtime: Handle, cx: &mut Context<Self>) -> Self {
        let remember_writer = LastConnectionWriter::new(&runtime);
        let shutdown_writer = remember_writer.clone();
        gpui_kit::App::on_app_quit(cx, move |_| {
            let flushed = shutdown_writer.flush();
            async move {
                let _ = flushed.await;
            }
        })
        .detach();
        let task = runtime.spawn_blocking(|| {
            (
                load_profiles().map_err(str::to_owned),
                read_last_connection(),
            )
        });
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = task
                .await
                .unwrap_or_else(|_| (Err("Could not load connection configuration".into()), None));
            let _ = this.update(cx, |state, cx| {
                let was_loading = state.profiles_loading;
                let can_restore = state.revision == 0 && state.metadata.is_none() && !state.loading;
                if let Some(connection) = state.finish_profile_loading(result) {
                    let database = connection.database().to_owned();
                    state.load(connection, database, None, None, false, cx);
                }
                if was_loading {
                    if let Some(error) = &state.config_error {
                        cx.emit(SessionEvent::ConfigurationFailed(error.clone()));
                    } else if can_restore && let Some(error) = &state.error {
                        cx.emit(SessionEvent::LoadFailed(error.clone()));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            remember_writer,
            runtime,
            profiles: Vec::new(),
            profiles_loading: true,
            config_error: None,
            metadata: None,
            revision: 0,
            loading: false,
            error: None,
        }
    }

    fn finish_profile_loading(
        &mut self,
        (profiles, last_id): (Result<Vec<ConnectionProfile>, String>, Option<String>),
    ) -> Option<ConnectionConfig> {
        // A newly saved profile already supplied a newer profile list and selection.
        if !self.profiles_loading {
            return None;
        }
        self.profiles_loading = false;
        match profiles {
            Ok(profiles) => self.profiles = profiles,
            Err(error) => self.config_error = Some(error),
        }
        // Do not restore over a newer selection or an in-flight connection.
        if self.revision != 0 || self.metadata.is_some() || self.loading {
            return None;
        }
        let id = last_id?;
        let connection = restore_connection(&self.profiles, &id);
        if connection.is_none() && self.config_error.is_none() {
            self.error = Some("The last connection is unavailable. Select another connection or check its configuration.".into());
        }
        connection
    }

    /// Returns the tier of the committed snapshot, or Local before the first snapshot.
    /// Include this in preview identities even when database and schema names match.
    pub fn connection_tier(&self) -> ConnectionTier {
        self.metadata
            .as_ref()
            .map_or(ConnectionTier::Local, |metadata| metadata.connection.tier())
    }

    /// Returns the active tier only after a connection has loaded successfully.
    pub fn active_connection_tier(&self) -> Option<ConnectionTier> {
        self.metadata
            .as_ref()
            .map(|metadata| metadata.connection.tier())
    }

    pub fn profiles(&self) -> &[ConnectionProfile] {
        &self.profiles
    }

    pub fn profiles_loading(&self) -> bool {
        self.profiles_loading
    }

    pub fn active_connection_id(&self) -> Option<&str> {
        self.metadata
            .as_ref()
            .map(|metadata| metadata.connection.id())
    }

    pub fn active_connection_name(&self) -> Option<&str> {
        self.metadata
            .as_ref()
            .map(|metadata| metadata.connection.name())
    }

    fn connection_config(&self, id: &str) -> Option<&ConnectionConfig> {
        self.profiles
            .iter()
            .find(|profile| profile.id() == id)?
            .connection()
    }

    pub(super) fn begin_add_connection(
        &self,
        name: String,
        url: String,
        password: String,
        tier: ConnectionTier,
        disable_tls: bool,
    ) -> Result<tokio::task::JoinHandle<ProfileSaveResult>, String> {
        let profile = PreparedProfile::new(name, url, password, tier, disable_tls)?;
        Ok(self.runtime.spawn_blocking(move || profile.save()))
    }

    pub(super) fn finish_add_connection(
        &mut self,
        profiles: Vec<ConnectionProfile>,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        self.install_saved_profiles(profiles);
        self.select_connection(id, cx);
        cx.notify();
    }

    fn install_saved_profiles(&mut self, profiles: Vec<ConnectionProfile>) {
        self.profiles = profiles;
        self.profiles_loading = false;
        self.config_error = None;
    }

    /// Switches only after the target connection and its metadata have loaded.
    pub fn select_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.profiles_loading {
            self.revision = self.revision.wrapping_add(1);
        }
        match self.prepare_connection_selection(id) {
            ConnectionSelection::Load(connection) => {
                let database = connection.database().to_owned();
                self.load(*connection, database, None, None, false, cx);
            }
            ConnectionSelection::Changed => cx.notify(),
            ConnectionSelection::Invalid => {
                if let Some(error) = &self.error {
                    cx.emit(SessionEvent::LoadFailed(error.clone()));
                }
                cx.notify();
            }
            ConnectionSelection::Unchanged => {}
        }
    }

    fn prepare_connection_selection(&mut self, id: &str) -> ConnectionSelection {
        if self
            .metadata
            .as_ref()
            .is_some_and(|metadata| metadata.connection.id() == id)
        {
            return if self.invalidate_pending() {
                ConnectionSelection::Changed
            } else {
                ConnectionSelection::Unchanged
            };
        }
        match self.connection_config(id).cloned() {
            Some(connection) => ConnectionSelection::Load(Box::new(connection)),
            None => {
                self.invalidate_pending();
                self.error = Some("Connection is not configured or its URL is invalid".into());
                ConnectionSelection::Invalid
            }
        }
    }

    pub fn database(&self) -> &str {
        self.metadata
            .as_ref()
            .map_or("", |metadata| &metadata.database)
    }

    pub fn schema(&self) -> &str {
        self.metadata
            .as_ref()
            .map_or("", |metadata| &metadata.schema)
    }

    pub fn databases(&self) -> &[String] {
        self.metadata
            .as_ref()
            .map_or(&[], |metadata| &metadata.databases)
    }

    pub fn schemas(&self) -> &[String] {
        self.metadata
            .as_ref()
            .map_or(&[], |metadata| &metadata.schemas)
    }

    pub fn tables(&self) -> &[String] {
        self.metadata
            .as_ref()
            .map_or(&[], |metadata| &metadata.tables)
    }

    pub(crate) fn columns(&self) -> Option<&BTreeMap<String, Vec<ColumnMetadata>>> {
        self.metadata.as_ref().map(|metadata| &metadata.columns)
    }

    pub(crate) fn quality_connection(&self) -> Option<ManagedConnection> {
        self.metadata
            .as_ref()
            .map(|metadata| metadata.session.clone())
    }

    pub(crate) fn query_connection(&self) -> Option<ManagedConnection> {
        self.metadata.as_ref().and_then(|metadata| {
            (!self.loading && metadata.connection.tier().allows_execution())
                .then(|| metadata.session.clone())
        })
    }

    pub(crate) fn table_connection(
        &self,
        database: &str,
        schema: &str,
        table: &str,
    ) -> Option<ManagedConnection> {
        self.metadata.as_ref().and_then(|metadata| {
            (metadata.database == database
                && metadata.schema == schema
                && metadata.tables.iter().any(|name| name == table))
            .then(|| metadata.session.clone())
        })
    }

    /// Resolve an edit against the committed scope and the preview's connection identity.
    pub(crate) fn edit_connection(
        &self,
        profile_id: &str,
        tier: ConnectionTier,
        database: &str,
        schema: &str,
        table: &str,
        preview_connection_id: Option<uuid::Uuid>,
    ) -> Result<ManagedConnection, &'static str> {
        if self.loading
            || !tier.allows_execution()
            || self.active_connection_id() != Some(profile_id)
            || self.database() != database
            || self.schema() != schema
        {
            return Err(
                "Return to the original Local or Staging connection and table before applying changes.",
            );
        }
        let connection = self
            .query_connection()
            .filter(|_| self.table_connection(database, schema, table).is_some())
            .ok_or("The original table is unavailable. Refresh the Explorer and try again.")?;
        if preview_connection_id != Some(connection.id()) {
            return Err(
                "The connection changed since this preview loaded. Discard changes and reload before editing.",
            );
        }
        Ok(connection)
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn error(&self) -> Option<&str> {
        self.config_error.as_deref().or(self.error.as_deref())
    }

    /// Selects an available database, opening a separate connection before committing.
    pub fn select_database(&mut self, database: String, cx: &mut Context<Self>) {
        if !self.databases().contains(&database) {
            return;
        }
        if self.database() == database {
            self.cancel_pending(cx);
            return;
        }
        let connection = self.metadata.as_ref().unwrap().connection.clone();
        self.load(connection, database, None, None, false, cx);
    }

    /// Selects an available schema in the currently selected database.
    /// The database argument rejects callbacks from an obsolete menu.
    pub fn select_schema(&mut self, database: &str, schema: String, cx: &mut Context<Self>) {
        if self.database() != database || !self.schemas().contains(&schema) {
            return;
        }
        if self.schema() == schema {
            self.cancel_pending(cx);
            return;
        }
        let metadata = self.metadata.as_ref().unwrap();
        let connection = metadata.connection.clone();
        let session = Some(metadata.session.clone());
        self.load(
            connection,
            database.to_owned(),
            Some(schema),
            session,
            true,
            cx,
        );
    }

    fn invalidate_pending(&mut self) -> bool {
        if !self.loading {
            return false;
        }
        self.revision = self.revision.wrapping_add(1);
        self.loading = false;
        true
    }

    fn cancel_pending(&mut self, cx: &mut Context<Self>) {
        if self.invalidate_pending() {
            cx.notify();
        }
    }

    fn load(
        &mut self,
        connection: ConnectionConfig,
        database: String,
        schema: Option<String>,
        session: Option<ManagedConnection>,
        require_schema: bool,
        cx: &mut Context<Self>,
    ) {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        self.loading = true;
        self.error = None;
        cx.notify();

        let task = self.runtime.spawn(async move {
            load_metadata(connection, database, schema, session, require_schema).await
        });
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Metadata task failed".to_owned()));
            let _ = this.update(cx, |state, cx| {
                let writer = state.remember_writer.clone();
                let mut pending = None;
                if let Some(event) = state.finish_and_remember(revision, result, |id| {
                    pending = Some((id.to_owned(), writer.enqueue(id.to_owned())));
                }) {
                    cx.emit(event);
                    if let Some((id, task)) = pending {
                        cx.spawn(async move |this, cx: &mut AsyncApp| {
                            let result = task.await.unwrap_or_else(|_| {
                                Err("Could not save the last connection".into())
                            });
                            let _ = this.update(cx, |state, cx| {
                                if state.finish_remember(revision, &id, result) {
                                    if let Some(error) = &state.error {
                                        cx.emit(SessionEvent::RememberFailed(error.clone()));
                                    }
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn finish_and_remember(
        &mut self,
        revision: u64,
        result: Result<Metadata, String>,
        remember: impl FnOnce(&str),
    ) -> Option<SessionEvent> {
        let event = match &result {
            Ok(_) => SessionEvent::LoadSucceeded,
            Err(error) => SessionEvent::LoadFailed(error.clone()),
        };
        if !self.finish(revision, result) {
            return None;
        }
        if matches!(event, SessionEvent::LoadSucceeded) {
            let id = self
                .active_connection_id()
                .expect("successful snapshot has an ID");
            remember(id);
        }
        Some(event)
    }

    fn finish_remember(&mut self, revision: u64, id: &str, result: Result<(), String>) -> bool {
        if self.revision != revision
            || self.active_connection_id() != Some(id)
            || self.error.is_some()
        {
            return false;
        }
        if let Err(error) = result {
            self.error = Some(format!(
                "Connected, but could not remember this connection. {error}"
            ));
            return true;
        }
        false
    }

    // Only the latest request may change the visible snapshot, loading state, or error.
    fn finish(&mut self, revision: u64, result: Result<Metadata, String>) -> bool {
        if revision != self.revision {
            return false;
        }
        self.loading = false;
        match result {
            Ok(metadata) => {
                self.metadata = Some(metadata);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
}

async fn load_metadata(
    connection: ConnectionConfig,
    database: String,
    preferred_schema: Option<String>,
    session: Option<ManagedConnection>,
    require_schema: bool,
) -> Result<Metadata, String> {
    let (session, metadata) = ConnectionManager::load_metadata(
        &connection,
        &database,
        preferred_schema,
        require_schema,
        session,
    )
    .await?;
    Ok(Metadata {
        connection,
        session,
        database,
        schema: metadata.schema,
        databases: metadata.databases,
        schemas: metadata.schemas,
        tables: metadata.tables,
        columns: metadata.columns,
    })
}
