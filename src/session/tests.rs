use super::*;

const DEFAULT_DATABASE: &str = "dbviewer";

fn metadata(tier: ConnectionTier, database: &str, schema: &str) -> Metadata {
    let connection = ConnectionConfig::local().with_test_identity(
        &tier.label().to_lowercase(),
        tier.label(),
        tier,
    );
    metadata_for_connection(connection, database, schema)
}

fn metadata_for_connection(connection: ConnectionConfig, database: &str, schema: &str) -> Metadata {
    let pool = PgPoolOptions::new().connect_lazy_with(PgConnectOptions::new());
    let session = crate::database::postgres::test_connection(
        connection.id(),
        database,
        connection.tier(),
        pool,
    );
    Metadata {
        connection,
        session,
        database: database.into(),
        schema: schema.into(),
        databases: vec![database.into()],
        schemas: vec![schema.into()],
        tables: vec!["example".into()],
        columns: BTreeMap::from([(
            "example".into(),
            vec![ColumnMetadata {
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
    }
}

fn empty_state(profiles_loading: bool) -> Session {
    Session {
        runtime: Handle::current(),
        profiles_loading,
        remember_writer: LastConnectionWriter::new(&Handle::current()),
        profiles: Vec::new(),
        config_error: None,
        metadata: None,
        revision: 0,
        loading: false,
        error: None,
    }
}

#[tokio::test]
async fn startup_load_restores_only_configured_profiles_and_reports_errors() {
    let mut state = empty_state(true);
    let profile = ConnectionProfile::from_connection(ConnectionConfig::local());
    let restored = state.finish_profile_loading((Ok(vec![profile]), Some("local".into())));
    assert_eq!(restored.unwrap().id(), "local");
    assert!(!state.profiles_loading());
    assert_eq!(state.profiles().len(), 1);
    assert_eq!(state.error(), None);

    let mut missing = empty_state(true);
    assert!(
        missing
            .finish_profile_loading((Ok(Vec::new()), Some("missing".into())))
            .is_none()
    );
    assert_eq!(
        missing.error(),
        Some(
            "The last connection is unavailable. Select another connection or check its configuration."
        )
    );

    let mut failed = empty_state(true);
    assert!(
        failed
            .finish_profile_loading((Err("bad configuration".into()), Some("missing".into())))
            .is_none()
    );
    assert_eq!(failed.error(), Some("bad configuration"));
    assert!(!failed.profiles_loading());
}

#[tokio::test]
async fn startup_results_do_not_restore_over_a_newer_selection() {
    let mut state = empty_state(true);
    state.revision = 1;
    state.loading = true;
    let profile = ConnectionProfile::from_connection(ConnectionConfig::local());
    assert!(
        state
            .finish_profile_loading((Ok(vec![profile]), Some("local".into())))
            .is_none()
    );
    assert!(!state.profiles_loading());
    assert_eq!(state.profiles().len(), 1);
    assert!(state.loading());
    assert_eq!(state.error(), None);
}

#[tokio::test]
async fn startup_results_cannot_replace_a_newly_saved_profile_list() {
    let mut state = empty_state(true);
    let saved = ConnectionConfig::local().with_test_identity("new", "New", ConnectionTier::Local);
    state.install_saved_profiles(vec![ConnectionProfile::from_connection(saved)]);
    assert!(
        state
            .finish_profile_loading((Err("stale failure".into()), Some("old".into())))
            .is_none()
    );
    assert_eq!(state.profiles()[0].id(), "new");
    assert_eq!(state.error(), None);
    assert!(!state.profiles_loading());
}

#[tokio::test]
async fn remember_errors_only_affect_the_current_successful_selection() {
    let mut state = empty_state(false);
    state.metadata = Some(metadata(ConnectionTier::Local, "app", "public"));
    state.revision = 2;
    assert!(!state.finish_remember(1, "local", Err("stale".into())));
    assert!(!state.finish_remember(2, "other", Err("wrong profile".into())));
    assert!(!state.finish_remember(2, "local", Ok(())));
    assert_eq!(state.error(), None);
    assert!(state.finish_remember(2, "local", Err("write failed".into())));
    assert_eq!(
        state.error(),
        Some("Connected, but could not remember this connection. write failed")
    );
    assert!(!state.finish_remember(2, "local", Err("second failure".into())));
}

#[tokio::test]
async fn remembers_only_successful_current_completions() {
    let mut state = Session {
        runtime: Handle::current(),
        profiles_loading: false,
        remember_writer: LastConnectionWriter::new(&Handle::current()),
        profiles: vec![],
        config_error: None,
        metadata: None,
        revision: 2,
        loading: true,
        error: None,
    };
    let mut remembered = "old".to_owned();
    assert_eq!(
        state.finish_and_remember(
            1,
            Ok(metadata(ConnectionTier::Local, "stale", "public")),
            |id| remembered = id.to_owned(),
        ),
        None,
    );
    assert_eq!(
        state.finish_and_remember(1, Err("stale failure".into()), |_| {}),
        None,
    );
    assert_eq!(remembered, "old");
    assert_eq!(
        state.finish_and_remember(2, Err("failed".into()), |id| {
            remembered = id.to_owned();
        }),
        Some(SessionEvent::LoadFailed("failed".into())),
    );
    assert_eq!(remembered, "old");
    state.revision = 3;
    assert_eq!(
        state.finish_and_remember(
            3,
            Ok(metadata(ConnectionTier::Staging, "db", "public")),
            |id| remembered = id.to_owned(),
        ),
        Some(SessionEvent::LoadSucceeded),
    );
    assert_eq!(remembered, "staging");
    assert_eq!(state.active_connection_id(), Some("staging"));
}

#[tokio::test]
async fn switching_profiles_with_the_same_tier_keeps_identity_distinct() {
    let first = ConnectionConfig::local();
    let second = first
        .clone()
        .with_test_identity("another", "Another", ConnectionTier::Local);
    let mut state = Session {
        runtime: Handle::current(),
        profiles_loading: false,
        remember_writer: LastConnectionWriter::new(&Handle::current()),
        profiles: vec![
            ConnectionProfile::from_connection(first),
            ConnectionProfile::from_connection(second),
        ],
        config_error: None,
        metadata: Some(metadata(ConnectionTier::Local, "app", "public")),
        revision: 1,
        loading: false,
        error: None,
    };
    let connection = match state.prepare_connection_selection("another") {
        ConnectionSelection::Load(connection) => connection,
        _ => panic!("a distinct profile must be loaded"),
    };
    assert_eq!(state.active_connection_id(), Some("local"));
    let snapshot = metadata_for_connection(*connection, "app", "public");
    assert!(state.finish(1, Ok(snapshot)));
    assert_eq!(state.active_connection_id(), Some("another"));
}

#[tokio::test]
async fn query_connection_rejects_production_and_pending_or_missing_connections() {
    let mut state = Session {
        runtime: Handle::current(),
        profiles_loading: false,
        remember_writer: LastConnectionWriter::new(&Handle::current()),
        profiles: vec![ConnectionProfile::from_connection(ConnectionConfig::local())],
        config_error: None,
        metadata: None,
        revision: 0,
        loading: false,
        error: None,
    };
    assert!(state.query_connection().is_none());
    state.metadata = Some(metadata(ConnectionTier::Local, "app", "public"));
    assert!(state.query_connection().is_some());
    state.loading = true;
    assert!(state.query_connection().is_none());
    state.loading = false;
    state.metadata = Some(metadata(ConnectionTier::Staging, "app", "public"));
    assert!(state.query_connection().is_some());
    state.metadata = Some(metadata(ConnectionTier::Production, "app", "public"));
    assert!(state.query_connection().is_none());
}

#[tokio::test]
async fn selecting_active_tier_cancels_pending_switch_even_with_matching_names() {
    let mut state = Session {
        runtime: Handle::current(),
        profiles_loading: false,
        remember_writer: LastConnectionWriter::new(&Handle::current()),
        profiles: vec![ConnectionProfile::from_connection(ConnectionConfig::local())],
        config_error: None,
        metadata: Some(metadata(ConnectionTier::Local, "app", "public")),
        revision: 42,
        loading: true,
        error: None,
    };
    assert!(matches!(
        state.prepare_connection_selection("local"),
        ConnectionSelection::Changed
    ));
    assert!(!state.loading());
    assert!(!state.finish(42, Ok(metadata(ConnectionTier::Staging, "app", "public"))));
    assert!(!state.finish(42, Err("stale failure".into())));
    assert_eq!(state.connection_tier(), ConnectionTier::Local);
    assert_eq!(state.database(), "app");
    assert_eq!(state.error(), None);
    assert!(matches!(
        state.prepare_connection_selection("local"),
        ConnectionSelection::Unchanged
    ));
}

#[tokio::test]
async fn unavailable_tier_cancels_pending_switch_and_keeps_snapshot() {
    let mut state = Session {
        runtime: Handle::current(),
        profiles_loading: false,
        remember_writer: LastConnectionWriter::new(&Handle::current()),
        profiles: vec![ConnectionProfile::from_connection(ConnectionConfig::local())],
        config_error: None,
        metadata: Some(metadata(ConnectionTier::Local, "app", "public")),
        revision: 5,
        loading: true,
        error: None,
    };
    assert!(state.connection_config("local").is_some());
    assert!(state.connection_config("production").is_none());
    assert!(matches!(
        state.prepare_connection_selection("production"),
        ConnectionSelection::Invalid
    ));
    assert!(!state.finish(5, Ok(metadata(ConnectionTier::Staging, "app", "public"))));
    assert_eq!(state.connection_tier(), ConnectionTier::Local);
    assert_eq!(state.database(), "app");
    assert!(!state.loading());
    assert_eq!(
        state.error(),
        Some("Connection is not configured or its URL is invalid")
    );
}

#[tokio::test]
async fn switch_commits_tier_with_same_database_and_schema() {
    let mut state = Session {
        runtime: Handle::current(),
        profiles_loading: false,
        remember_writer: LastConnectionWriter::new(&Handle::current()),
        profiles: vec![
            ConnectionProfile::from_connection(ConnectionConfig::local()),
            ConnectionProfile::from_connection(
                ConnectionConfig::remote(
                    ConnectionTier::Staging,
                    "postgres://reader:secret@db.example.test/app?hostaddr=192.0.2.1",
                )
                .unwrap(),
            ),
        ],
        config_error: None,
        metadata: Some(metadata(ConnectionTier::Local, "app", "public")),
        revision: 1,
        loading: true,
        error: None,
    };
    let connection = match state.prepare_connection_selection("staging") {
        ConnectionSelection::Load(connection) => connection,
        _ => panic!("expected a staging connection"),
    };
    assert_eq!(connection.database(), "app");
    assert_eq!(state.connection_tier(), ConnectionTier::Local);
    assert_eq!(state.database(), "app");
    let snapshot = metadata_for_connection(*connection, "app", "public");
    assert!(state.finish(1, Ok(snapshot)));
    assert_eq!(state.connection_tier(), ConnectionTier::Staging);
    let committed = state.metadata.as_ref().unwrap().connection.options();
    assert_eq!(committed.get_host(), "db.example.test");
    assert!(matches!(committed.get_ssl_mode(), PgSslMode::VerifyFull));
}

#[tokio::test]
async fn metadata_errors_do_not_include_connection_secrets() {
    let connection = ConnectionConfig::remote(
        ConnectionTier::Staging,
        "postgres://reader:secret@db.example.test/app?hostaddr=192.0.2.1",
    )
    .expect("valid configuration");
    let pool = PgPoolOptions::new().connect_lazy_with(connection.options().clone());
    pool.close().await;
    let session =
        crate::database::postgres::test_connection(connection.id(), "app", connection.tier(), pool);
    let error = match load_metadata(connection, "app".into(), None, Some(session), false).await {
        Ok(_) => panic!("closed pool should fail"),
        Err(error) => error,
    };
    assert_eq!(error, "Failed to load databases");
    assert!(!error.contains("secret"));
}

#[tokio::test]
async fn metadata_rejects_reuse_across_profiles_and_databases() {
    let connection = ConnectionConfig::local();
    let pool = PgPoolOptions::new().connect_lazy_with(PgConnectOptions::new());
    let wrong_profile = crate::database::postgres::test_connection(
        "another",
        "app",
        ConnectionTier::Local,
        pool.clone(),
    );
    let wrong_database = crate::database::postgres::test_connection(
        connection.id(),
        "other",
        ConnectionTier::Local,
        pool,
    );
    for session in [wrong_profile, wrong_database] {
        let error =
            match load_metadata(connection.clone(), "app".into(), None, Some(session), false).await
            {
                Ok(_) => panic!("a mismatched connection must not be reused"),
                Err(error) => error,
            };
        assert_eq!(
            error,
            "The connection no longer matches the selected database"
        );
    }
}

#[tokio::test]
async fn stale_completions_cannot_replace_state_or_finish_loading() {
    let mut state = Session {
        runtime: Handle::current(),
        profiles_loading: false,
        remember_writer: LastConnectionWriter::new(&Handle::current()),
        profiles: vec![ConnectionProfile::from_connection(ConnectionConfig::local())],
        config_error: None,
        metadata: Some(metadata(ConnectionTier::Local, "original", "public")),
        revision: 2,
        loading: true,
        error: None,
    };
    assert!(!state.finish(1, Ok(metadata(ConnectionTier::Staging, "stale", "other"))));
    assert!(!state.finish(1, Err("stale error".into())));
    assert!(state.loading());
    assert_eq!(state.database(), "original");
    assert_eq!(state.error(), None);

    assert!(state.finish(
        2,
        Ok(metadata(ConnectionTier::Production, "new", "inventory"))
    ));
    assert!(!state.loading());
    assert_eq!(state.database(), "new");
    assert_eq!(state.schema(), "inventory");
    assert_eq!(state.connection_tier(), ConnectionTier::Production);
    assert_eq!(state.tables(), &["example"]);
}

#[tokio::test]
async fn failure_preserves_the_last_successful_snapshot() {
    let mut state = Session {
        runtime: Handle::current(),
        profiles_loading: false,
        remember_writer: LastConnectionWriter::new(&Handle::current()),
        profiles: vec![ConnectionProfile::from_connection(ConnectionConfig::local())],
        config_error: None,
        metadata: Some(metadata(ConnectionTier::Local, "original", "public")),
        revision: 1,
        loading: true,
        error: None,
    };
    assert!(state.finish(1, Err("connection failed".into())));
    assert!(!state.loading());
    assert_eq!(state.error(), Some("connection failed"));
    assert_eq!(state.database(), "original");
    assert_eq!(state.schema(), "public");
    assert_eq!(state.connection_tier(), ConnectionTier::Local);
    assert_eq!(state.tables(), &["example"]);
}

#[tokio::test]
async fn initial_failure_does_not_invent_a_selection() {
    let mut state = Session {
        runtime: Handle::current(),
        profiles_loading: false,
        remember_writer: LastConnectionWriter::new(&Handle::current()),
        profiles: vec![ConnectionProfile::from_connection(ConnectionConfig::local())],
        config_error: None,
        metadata: None,
        revision: 1,
        loading: true,
        error: None,
    };
    assert!(state.finish(1, Err("connection failed".into())));
    assert_eq!(state.active_connection_tier(), None);
    assert_eq!(state.database(), "");
    assert_eq!(state.schema(), "");
    assert!(state.databases().is_empty());
    assert!(state.schemas().is_empty());
    assert!(state.tables().is_empty());
    assert_eq!(state.error(), Some("connection failed"));
}

/// Requires the seeded Compose database; never mutates it.
#[tokio::test]
#[ignore = "requires the local Compose PostgreSQL database"]
async fn loads_seeded_metadata_and_rejects_missing_schema() {
    let metadata = load_metadata(
        ConnectionConfig::local(),
        DEFAULT_DATABASE.into(),
        None,
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(metadata.database, DEFAULT_DATABASE);
    assert!(metadata.databases.contains(&DEFAULT_DATABASE.to_owned()));
    assert!(!metadata.databases.contains(&"template1".to_owned()));
    assert!(metadata.schemas.contains(&"public".to_owned()));
    assert!(metadata.schemas.contains(&"inventory".to_owned()));
    assert!(!metadata.schemas.contains(&"information_schema".to_owned()));
    assert!(
        !metadata
            .schemas
            .iter()
            .any(|schema| schema.starts_with("pg_"))
    );

    let public = load_metadata(
        metadata.connection.clone(),
        DEFAULT_DATABASE.into(),
        Some("public".into()),
        Some(metadata.session.clone()),
        true,
    )
    .await
    .unwrap();
    assert_eq!(public.session.id(), metadata.session.id());
    assert!(public.tables.contains(&"customers".to_owned()));
    assert!(public.tables.contains(&"orders".to_owned()));
    assert!(
        public.columns["orders"]
            .iter()
            .any(|column| column.name == "id")
    );

    let inventory = load_metadata(
        metadata.connection.clone(),
        DEFAULT_DATABASE.into(),
        Some("inventory".into()),
        Some(metadata.session.clone()),
        true,
    )
    .await
    .unwrap();
    assert_eq!(inventory.session.id(), metadata.session.id());
    assert_eq!(inventory.tables, ["products"]);
    assert!(
        inventory.columns["products"]
            .iter()
            .any(|column| column.name == "id")
    );
    assert!(
        load_metadata(
            metadata.connection.clone(),
            DEFAULT_DATABASE.into(),
            Some("missing_schema".into()),
            Some(metadata.session),
            true,
        )
        .await
        .is_err()
    );

    let other = load_metadata(
        ConnectionConfig::local(),
        "postgres".into(),
        None,
        None,
        false,
    )
    .await
    .unwrap();
    assert_ne!(other.session.id(), public.session.id());
    let actual = other
        .session
        .execute_sql(String::new(), "SELECT current_database()::text".into())
        .await
        .unwrap();
    assert_eq!(
        actual.statements[0].table.as_ref().unwrap().rows[0][0].as_deref(),
        Some("postgres")
    );
}
