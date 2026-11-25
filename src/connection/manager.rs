use std::sync::Arc;

use uuid::Uuid;

use crate::database::{
    driver::{DatabaseConnection, DatabaseDriver, DatabaseMetadata},
    query::QueryOutput,
    table::{ApplyError, EditSnapshot, StagedEdits, TablePreview},
};

use super::ConnectionTier;

/// Opens driver connections and assigns each successful connection a fresh identity.
pub(crate) struct ConnectionManager;

impl ConnectionManager {
    pub(crate) async fn connect(
        driver: &dyn DatabaseDriver,
        database: &str,
    ) -> Result<ManagedConnection, String> {
        let session = driver.connect(database).await?;
        Ok(ManagedConnection {
            id: Uuid::new_v4(),
            engine: driver.engine(),
            profile_id: driver.id().to_owned(),
            database: database.to_owned(),
            tier: driver.tier(),
            session: Arc::from(session),
        })
    }

    /// Reuse requires the same engine, profile, database, and safety tier.
    /// A profile revision will also be needed when profile editing is supported.
    pub(crate) async fn load_metadata(
        driver: &dyn DatabaseDriver,
        database: &str,
        preferred_schema: Option<String>,
        require_schema: bool,
        existing: Option<ManagedConnection>,
    ) -> Result<(ManagedConnection, DatabaseMetadata), String> {
        let session = match existing {
            Some(session)
                if session.engine == driver.engine()
                    && session.matches_scope(driver.id(), database)
                    && session.tier == driver.tier() =>
            {
                session
            }
            Some(_) => return Err("The connection no longer matches the selected database".into()),
            None => Self::connect(driver, database).await?,
        };
        let metadata = session.metadata(preferred_schema, require_schema).await?;
        Ok((session, metadata))
    }
}

/// A shared handle to a single opened connection, not a mutable global selection.
#[derive(Clone)]
pub(crate) struct ManagedConnection {
    id: Uuid,
    engine: &'static str,
    profile_id: String,
    database: String,
    tier: ConnectionTier,
    session: Arc<dyn DatabaseConnection>,
}

impl ManagedConnection {
    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    pub(crate) fn matches_scope(&self, profile_id: &str, database: &str) -> bool {
        self.profile_id == profile_id && self.database == database
    }

    pub(crate) async fn metadata(
        &self,
        preferred_schema: Option<String>,
        require_schema: bool,
    ) -> Result<DatabaseMetadata, String> {
        self.session
            .metadata(preferred_schema, require_schema)
            .await
    }

    pub(crate) async fn ping(&self) -> Result<(), String> {
        self.session.ping().await
    }

    pub(crate) async fn execute_sql(
        &self,
        schema: String,
        sql: String,
    ) -> Result<QueryOutput, String> {
        if !self.tier.allows_execution() {
            return Err("SQL execution is unavailable on Production".into());
        }
        self.session.execute_sql(schema, sql).await
    }

    pub(crate) async fn preview_table(
        &self,
        schema: &str,
        table: &str,
    ) -> Result<(TablePreview, Option<EditSnapshot>), String> {
        let allow_editing = self.tier.allows_execution();
        let (preview, mut snapshot) = self
            .session
            .preview_table(schema, table, allow_editing)
            .await?;
        if allow_editing {
            if let Some(snapshot) = &mut snapshot {
                snapshot.bind(self.id, schema, table);
            }
        } else {
            snapshot = None;
        }
        Ok((preview, snapshot))
    }

    pub(crate) async fn apply_table_edits(
        &self,
        schema: &str,
        table: &str,
        snapshot: &EditSnapshot,
        edits: &StagedEdits,
    ) -> Result<(), ApplyError> {
        if !self.tier.allows_execution() {
            return Err(ApplyError::Rejected(
                "Table editing is unavailable on Production".into(),
            ));
        }
        if !snapshot.matches_scope(self.id, schema, table) {
            return Err(ApplyError::Rejected(
                "The connection or table changed since this preview loaded. Discard changes and reload before editing.".into(),
            ));
        }
        self.session
            .apply_table_edits(schema, table, snapshot, edits)
            .await
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        engine: &'static str,
        profile_id: &str,
        database: &str,
        tier: ConnectionTier,
        session: impl DatabaseConnection + 'static,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            engine,
            profile_id: profile_id.into(),
            database: database.into(),
            tier,
            session: Arc::new(session),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures_util::future::BoxFuture;

    use super::*;

    struct FakeDriver {
        engine: &'static str,
        id: &'static str,
        tier: ConnectionTier,
        executions: Arc<AtomicUsize>,
        fail: bool,
    }

    struct FakeConnection {
        database: String,
        executions: Arc<AtomicUsize>,
    }

    impl DatabaseDriver for FakeDriver {
        fn engine(&self) -> &'static str {
            self.engine
        }
        fn id(&self) -> &str {
            self.id
        }
        fn tier(&self) -> ConnectionTier {
            self.tier
        }
        fn connect<'a>(
            &'a self,
            database: &'a str,
        ) -> BoxFuture<'a, Result<Box<dyn DatabaseConnection>, String>> {
            Box::pin(async move {
                if self.fail {
                    return Err("connection failed".into());
                }
                Ok(Box::new(FakeConnection {
                    database: database.into(),
                    executions: self.executions.clone(),
                }) as Box<dyn DatabaseConnection>)
            })
        }
    }

    impl DatabaseConnection for FakeConnection {
        fn metadata(
            &self,
            preferred_schema: Option<String>,
            _: bool,
        ) -> BoxFuture<'_, Result<DatabaseMetadata, String>> {
            Box::pin(async move {
                Ok(DatabaseMetadata {
                    schema: preferred_schema.unwrap_or_default(),
                    databases: vec![self.database.clone()],
                    schemas: Vec::new(),
                    tables: vec!["example".into()],
                    columns: std::collections::BTreeMap::from([(
                        "example".into(),
                        vec![crate::database::driver::ColumnMetadata {
                            name: "id".into(),
                            data_type: "bigint".into(),
                            udt_name: "int8".into(),
                            nullable: false,
                            has_default: false,
                            identity: None,
                            generated: None,
                            primary_key: true,
                            foreign_key: false,
                            description: None,
                            postgis_type: None,
                            postgis_srid: None,
                            postgis_dimensions: None,
                        }],
                    )]),
                })
            })
        }
        fn ping(&self) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
        fn execute_sql<'a>(
            &'a self,
            _: String,
            _: String,
        ) -> BoxFuture<'a, Result<QueryOutput, String>> {
            Box::pin(async move {
                self.executions.fetch_add(1, Ordering::SeqCst);
                Ok(QueryOutput::default())
            })
        }
    }

    struct TableConnection {
        preview_flags: Arc<AtomicUsize>,
        applies: Arc<AtomicUsize>,
        unknown: bool,
    }

    impl DatabaseConnection for TableConnection {
        fn metadata(
            &self,
            _: Option<String>,
            _: bool,
        ) -> BoxFuture<'_, Result<DatabaseMetadata, String>> {
            Box::pin(async { Err("not used".into()) })
        }
        fn ping(&self) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
        fn execute_sql<'a>(
            &'a self,
            _: String,
            _: String,
        ) -> BoxFuture<'a, Result<QueryOutput, String>> {
            Box::pin(async { Err("not used".into()) })
        }
        fn preview_table<'a>(
            &'a self,
            schema: &'a str,
            table: &'a str,
            allow_editing: bool,
        ) -> BoxFuture<'a, Result<(TablePreview, Option<EditSnapshot>), String>> {
            Box::pin(async move {
                assert_eq!((schema, table), ("public", "example"));
                self.preview_flags
                    .store(if allow_editing { 2 } else { 1 }, Ordering::SeqCst);
                let preview = TablePreview {
                    columns: vec!["value".into()],
                    rows: vec![vec![Some("old".into())]],
                };
                let snapshot = EditSnapshot::new(
                    vec![crate::database::table::EditColumn {
                        name: "value".into(),
                        editable: true,
                    }],
                    vec![crate::database::table::EditRow {
                        originals: vec![Some("old".into())],
                    }],
                    "fake token",
                );
                // Return a token even when editing is disallowed to test the manager's guard.
                Ok((preview, Some(snapshot)))
            })
        }
        fn apply_table_edits<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            snapshot: &'a EditSnapshot,
            edits: &'a StagedEdits,
        ) -> BoxFuture<'a, Result<(), ApplyError>> {
            Box::pin(async move {
                assert_eq!(snapshot.token::<&str>(), Some(&"fake token"));
                assert_eq!(edits.get(0, 0), Some(&Some("new".into())));
                self.applies.fetch_add(1, Ordering::SeqCst);
                if self.unknown {
                    Err(ApplyError::CommitUnknown("verify before retrying".into()))
                } else {
                    Ok(())
                }
            })
        }
    }

    fn table_connection(
        tier: ConnectionTier,
        flags: Arc<AtomicUsize>,
        applies: Arc<AtomicUsize>,
        unknown: bool,
    ) -> ManagedConnection {
        ManagedConnection::for_test(
            "fake",
            "profile",
            "main",
            tier,
            TableConnection {
                preview_flags: flags,
                applies,
                unknown,
            },
        )
    }

    #[tokio::test]
    async fn preview_and_edits_dispatch_and_bind_the_snapshot() {
        let flags = Arc::new(AtomicUsize::new(0));
        let applies = Arc::new(AtomicUsize::new(0));
        let connection =
            table_connection(ConnectionTier::Local, flags.clone(), applies.clone(), false);
        let (preview, snapshot) = connection.preview_table("public", "example").await.unwrap();
        assert_eq!(flags.load(Ordering::SeqCst), 2);
        assert_eq!(preview.rows, [vec![Some("old".into())]]);
        let snapshot = snapshot.unwrap();
        let mut edits = StagedEdits::default();
        assert!(edits.stage(&snapshot, 0, 0, Some("new".into())));
        connection
            .apply_table_edits("public", "example", &snapshot, &edits)
            .await
            .unwrap();
        assert_eq!(applies.load(Ordering::SeqCst), 1);

        let other = table_connection(ConnectionTier::Local, flags, applies.clone(), false);
        for (session, schema, table) in [
            (&other, "public", "example"),
            (&connection, "other", "example"),
            (&connection, "public", "other"),
        ] {
            assert!(matches!(
                session
                    .apply_table_edits(schema, table, &snapshot, &edits)
                    .await,
                Err(ApplyError::Rejected(_))
            ));
        }
        assert_eq!(applies.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn production_preview_is_readonly_and_edits_never_reach_the_driver() {
        let flags = Arc::new(AtomicUsize::new(0));
        let applies = Arc::new(AtomicUsize::new(0));
        let local = table_connection(ConnectionTier::Local, flags.clone(), applies.clone(), false);
        let snapshot = local
            .preview_table("public", "example")
            .await
            .unwrap()
            .1
            .unwrap();
        let mut edits = StagedEdits::default();
        assert!(edits.stage(&snapshot, 0, 0, Some("new".into())));
        let production = table_connection(
            ConnectionTier::Production,
            flags.clone(),
            applies.clone(),
            false,
        );
        assert!(
            production
                .preview_table("public", "example")
                .await
                .unwrap()
                .1
                .is_none()
        );
        assert_eq!(flags.load(Ordering::SeqCst), 1);
        assert!(
            matches!(production.apply_table_edits("public", "example", &snapshot, &edits).await, Err(ApplyError::Rejected(message)) if message.contains("Production"))
        );
        assert_eq!(applies.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unknown_commit_outcomes_survive_the_capability_boundary() {
        let flags = Arc::new(AtomicUsize::new(0));
        let applies = Arc::new(AtomicUsize::new(0));
        let connection = table_connection(ConnectionTier::Staging, flags, applies.clone(), true);
        let snapshot = connection
            .preview_table("public", "example")
            .await
            .unwrap()
            .1
            .unwrap();
        let mut edits = StagedEdits::default();
        assert!(edits.stage(&snapshot, 0, 0, Some("new".into())));
        assert!(
            matches!(connection.apply_table_edits("public", "example", &snapshot, &edits).await, Err(ApplyError::CommitUnknown(message)) if message == "verify before retrying")
        );
        assert_eq!(applies.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unsupported_table_capabilities_return_explicit_errors() {
        let connection = ManagedConnection::for_test(
            "fake",
            "profile",
            "main",
            ConnectionTier::Local,
            FakeConnection {
                database: "main".into(),
                executions: Arc::new(AtomicUsize::new(0)),
            },
        );
        assert!(
            matches!(connection.preview_table("public", "example").await, Err(message) if message.contains("not supported"))
        );
        let mut snapshot = EditSnapshot::new(Vec::new(), Vec::new(), ());
        snapshot.bind(connection.id(), "public", "example");
        assert!(
            matches!(connection.apply_table_edits("public", "example", &snapshot, &StagedEdits::default()).await, Err(ApplyError::Rejected(message)) if message.contains("not supported"))
        );
    }

    #[tokio::test]
    async fn manager_opens_backend_connections_with_distinct_stable_identities() {
        let driver = FakeDriver {
            engine: "fake",
            id: "profile-a",
            tier: ConnectionTier::Local,
            executions: Arc::new(AtomicUsize::new(0)),
            fail: false,
        };
        let first = ConnectionManager::connect(&driver, "main").await.unwrap();
        let clone = first.clone();
        let second = ConnectionManager::connect(&driver, "main").await.unwrap();
        assert_eq!(first.id(), clone.id());
        assert_ne!(first.id(), second.id());
        assert!(first.matches_scope("profile-a", "main"));
        assert!(!first.matches_scope("profile-b", "main"));
        assert!(!first.matches_scope("profile-a", "other"));
        let metadata = first.metadata(Some("schema".into()), true).await.unwrap();
        assert_eq!(metadata.schema, "schema");
        assert_eq!(metadata.databases, ["main"]);
        assert_eq!(metadata.tables, ["example"]);
        let (reused, _) = ConnectionManager::load_metadata(
            &driver,
            "main",
            Some("other".into()),
            true,
            Some(first.clone()),
        )
        .await
        .unwrap();
        assert_eq!(reused.id(), first.id());
        assert!(first.ping().await.is_ok());
        assert_eq!(
            first
                .execute_sql("schema".into(), "SELECT 1".into())
                .await
                .unwrap(),
            QueryOutput::default()
        );
        assert_eq!(driver.executions.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn production_rejects_sql_before_calling_any_driver() {
        let driver = FakeDriver {
            engine: "fake",
            id: "production",
            tier: ConnectionTier::Production,
            executions: Arc::new(AtomicUsize::new(0)),
            fail: false,
        };
        let connection = ConnectionManager::connect(&driver, "main").await.unwrap();
        assert!(
            connection
                .execute_sql(String::new(), "SELECT 1".into())
                .await
                .is_err()
        );
        assert_eq!(driver.executions.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn metadata_reuse_rejects_a_changed_safety_tier() {
        let executions = Arc::new(AtomicUsize::new(0));
        let local = FakeDriver {
            engine: "fake",
            id: "profile-a",
            tier: ConnectionTier::Local,
            executions: executions.clone(),
            fail: false,
        };
        let production = FakeDriver {
            engine: "fake",
            id: "profile-a",
            tier: ConnectionTier::Production,
            executions,
            fail: false,
        };
        let connection = ConnectionManager::connect(&local, "main").await.unwrap();
        assert!(
            ConnectionManager::load_metadata(&production, "main", None, false, Some(connection),)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn metadata_reuse_rejects_a_changed_engine() {
        let executions = Arc::new(AtomicUsize::new(0));
        let first = FakeDriver {
            engine: "fake-a",
            id: "profile-a",
            tier: ConnectionTier::Local,
            executions: executions.clone(),
            fail: false,
        };
        let other = FakeDriver {
            engine: "fake-b",
            id: "profile-a",
            tier: ConnectionTier::Local,
            executions,
            fail: false,
        };
        let connection = ConnectionManager::connect(&first, "main").await.unwrap();
        assert!(
            ConnectionManager::load_metadata(&other, "main", None, false, Some(connection))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_open_does_not_create_a_connection() {
        let driver = FakeDriver {
            engine: "fake",
            id: "profile-a",
            tier: ConnectionTier::Local,
            executions: Arc::new(AtomicUsize::new(0)),
            fail: true,
        };
        assert!(
            matches!(ConnectionManager::connect(&driver, "main").await, Err(error) if error == "connection failed")
        );
    }
}
