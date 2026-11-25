use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use url::Url;

use super::ConnectionTier;

// PgConnectOptions' Debug implementation contains credentials. Never derive Debug
// for a type containing it, or include SQLx errors in user-visible messages.
#[derive(Clone)]
pub(crate) struct ConnectionConfig {
    id: String,
    name: String,
    tier: ConnectionTier,
    options: PgConnectOptions,
    password_in_keyring: bool,
}

impl ConnectionConfig {
    #[cfg(test)]
    pub(crate) fn local() -> Self {
        Self {
            id: "local".into(),
            name: "Local".into(),
            tier: ConnectionTier::Local,
            options: PgConnectOptions::new()
                .host("127.0.0.1")
                // SQLx otherwise inherits PGHOSTADDR, which takes precedence over host.
                .hostaddr(std::net::Ipv4Addr::LOCALHOST)
                .port(5432)
                .username("dbviewer")
                .password("dbviewer")
                .database("dbviewer")
                .ssl_mode(PgSslMode::Disable),
            password_in_keyring: false,
        }
    }

    pub(crate) fn remote(tier: ConnectionTier, value: &str) -> Option<Self> {
        let url = Url::parse(value).ok()?;
        if !matches!(url.scheme(), "postgres" | "postgresql") {
            return None;
        }

        let mut explicit_host = url.host_str().is_some_and(|host| !host.is_empty());
        let mut explicit_database = !url.path().trim_start_matches('/').is_empty();
        let mut explicit_hostaddr = false;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "host" => explicit_host = !value.trim().is_empty(),
                "dbname" => explicit_database = !value.trim().is_empty(),
                "hostaddr" => explicit_hostaddr = true,
                // SQLx logs unknown parameters, including their values. Reject
                // them before parsing, rather than risk logging a URL secret.
                "sslmode"
                | "ssl-mode"
                | "sslrootcert"
                | "ssl-root-cert"
                | "ssl-ca"
                | "sslcert"
                | "ssl-cert"
                | "sslkey"
                | "ssl-key"
                | "statement-cache-capacity"
                | "port"
                | "user"
                | "password"
                | "application_name"
                | "options" => {}
                key if key.starts_with("options[") && key.ends_with(']') => {}
                _ => return None,
            }
        }
        // SQLx otherwise inherits PGHOSTADDR even with an explicit URL host.
        if !explicit_host
            || !explicit_database
            || (!explicit_hostaddr && std::env::var_os("PGHOSTADDR").is_some())
        {
            return None;
        }
        let options = PgConnectOptions::from_str(value).ok()?;
        if options.get_socket().is_some()
            || options.get_host().trim().is_empty()
            || options.get_host().starts_with('/')
            || options
                .get_database()
                .is_none_or(|database| database.trim().is_empty())
        {
            return None;
        }
        Some(Self {
            id: tier.label().to_lowercase(),
            name: tier.label().into(),
            tier,
            options: options.ssl_mode(PgSslMode::VerifyFull),
            password_in_keyring: false,
        })
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn tier(&self) -> ConnectionTier {
        self.tier
    }

    pub(crate) fn database(&self) -> &str {
        self.options
            .get_database()
            .expect("validated connection database")
    }

    #[cfg(test)]
    pub(crate) fn with_test_identity(mut self, id: &str, name: &str, tier: ConnectionTier) -> Self {
        self.id = id.into();
        self.name = name.into();
        self.tier = tier;
        self
    }

    #[cfg(test)]
    pub(crate) fn options(&self) -> &PgConnectOptions {
        &self.options
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    version: u8,
    connections: Vec<ProfileSpec>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileSpec {
    id: String,
    name: String,
    tier: ConnectionTier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default)]
    tls: TlsPolicy,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    password_in_keyring: bool,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum TlsPolicy {
    #[default]
    VerifyFull,
    Disable,
}

/// A validated PostgreSQL profile and its connection availability.
#[derive(Clone)]
pub struct ConnectionProfile {
    id: String,
    name: String,
    tier: ConnectionTier,
    connection: Option<ConnectionConfig>,
}

impl ConnectionProfile {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn tier(&self) -> ConnectionTier {
        self.tier
    }
    pub fn is_configured(&self) -> bool {
        self.connection.is_some()
    }

    pub(crate) fn connection(&self) -> Option<&ConnectionConfig> {
        self.connection.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn from_connection(connection: ConnectionConfig) -> Self {
        Self {
            id: connection.id.clone(),
            name: connection.name.clone(),
            tier: connection.tier,
            connection: Some(connection),
        }
    }
}

fn parse_profiles(
    contents: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Vec<ConnectionProfile>, &'static str> {
    let file: ProfileFile = serde_json::from_str(contents)
        .map_err(|_| "Invalid connection configuration. Check connections.json.")?;
    if file.version != 1 {
        return Err("Connection configuration must use version 1.");
    }
    let mut ids = HashSet::new();
    let mut profiles = Vec::new();
    for spec in file.connections {
        if spec.id.is_empty()
            || spec.id.len() > 128
            || !spec
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            || !ids.insert(spec.id.clone())
            || spec.name.trim().is_empty()
            || spec.name.chars().any(char::is_control)
            || spec.url_env.is_some() == spec.url.is_some()
            || spec.url_env.as_ref().is_some_and(|name| {
                name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
            || spec.url.as_ref().is_some_and(|url| !is_safe_saved_url(url))
            || (spec.password_in_keyring && spec.url.is_none())
            || matches!(spec.tls, TlsPolicy::Disable) && spec.tier != ConnectionTier::Local
        {
            return Err(
                "Invalid connection profile. Check IDs, names, URL environment variables and TLS policy.",
            );
        }
        let value = spec
            .url
            .clone()
            .or_else(|| spec.url_env.as_deref().and_then(&lookup));
        let connection = value.and_then(|url| {
            let mut connection = ConnectionConfig::remote(spec.tier, &url)?;
            connection.id = spec.id.clone();
            connection.name = spec.name.clone();
            connection.password_in_keyring = spec.password_in_keyring;
            if matches!(spec.tls, TlsPolicy::Disable) {
                connection.options = connection.options.ssl_mode(PgSslMode::Disable);
            }
            Some(connection)
        });
        profiles.push(ConnectionProfile {
            id: spec.id,
            name: spec.name,
            tier: spec.tier,
            connection,
        });
    }
    Ok(profiles)
}

fn is_safe_saved_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "postgres" | "postgresql")
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn profile_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        })?;
    Some(config.join("dbviewer/connections.json"))
}

pub(crate) fn load_profiles() -> Result<Vec<ConnectionProfile>, &'static str> {
    if let Some(path) = profile_path() {
        match std::fs::read_to_string(path) {
            Ok(contents) => return parse_profiles(&contents, |name| std::env::var(name).ok()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(
                    "Could not read connection configuration. Check connections.json permissions.",
                );
            }
        }
    }
    Ok(Vec::new())
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("Configuration directory is unavailable")?;
    std::fs::create_dir_all(parent)
        .map_err(|_| "Could not create the connection configuration directory")?;
    let temporary = parent.join(format!(".dbviewer-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
        .map_err(|_| "Could not save connection configuration. Check directory permissions.".into())
}

fn last_connection_path() -> Option<PathBuf> {
    Some(profile_path()?.with_file_name("last-connection.json"))
}

pub(crate) fn restore_connection(
    profiles: &[ConnectionProfile],
    id: &str,
) -> Option<ConnectionConfig> {
    profiles
        .iter()
        .find(|profile| profile.id == id)?
        .connection
        .clone()
}

pub(crate) fn read_last_connection() -> Option<String> {
    let contents = std::fs::read(last_connection_path()?).ok()?;
    serde_json::from_slice(&contents).ok()
}

pub(crate) fn save_last_connection(id: &str) -> Result<(), String> {
    let path = last_connection_path().ok_or("Configuration directory is unavailable")?;
    let contents = serde_json::to_vec(id).map_err(|_| "Could not encode last connection")?;
    atomic_write(&path, &contents)
}

pub(crate) type ProfileSaveResult = Result<(Vec<ConnectionProfile>, String), String>;

/// Validated input for a blocking keyring and profile-file write.
pub(crate) struct PreparedProfile {
    path: PathBuf,
    spec: ProfileSpec,
    password: String,
}

impl PreparedProfile {
    pub(crate) fn new(
        name: String,
        url: String,
        password: String,
        tier: ConnectionTier,
        disable_tls: bool,
    ) -> Result<Self, String> {
        let name = name.trim();
        let url = url.trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            return Err("Enter a connection name without control characters.".into());
        }
        if !is_safe_saved_url(url) {
            return Err("Enter a PostgreSQL URL without a password, query parameters, or fragment. Enter the password in the password field.".into());
        }
        if disable_tls && tier != ConnectionTier::Local {
            return Err("TLS can only be disabled for Local connections.".into());
        }
        if ConnectionConfig::remote(tier, url).is_none() {
            return Err("Enter a valid PostgreSQL URL with an explicit host and database. Check PGHOSTADDR if it is set.".into());
        }
        let spec = ProfileSpec {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            tier,
            url: Some(url.into()),
            url_env: None,
            password_in_keyring: !password.is_empty(),
            tls: if disable_tls {
                TlsPolicy::Disable
            } else {
                TlsPolicy::VerifyFull
            },
        };
        let path = profile_path().ok_or("Configuration directory is unavailable")?;
        Ok(Self {
            path,
            spec,
            password,
        })
    }

    pub(crate) fn save(self) -> ProfileSaveResult {
        let id = self.spec.id.clone();
        let profiles = save_profile_with_password(&self.path, self.spec, &self.password)?;
        Ok((profiles, id))
    }
}

fn append_profile(path: &Path, profile: ProfileSpec) -> Result<Vec<ConnectionProfile>, String> {
    let mut file = match std::fs::read_to_string(path) {
        Ok(contents) => {
            // Validate before rewriting so a malformed or newer file is never overwritten.
            parse_profiles(&contents, |name| std::env::var(name).ok()).map_err(str::to_owned)?;
            serde_json::from_str::<ProfileFile>(&contents)
                .map_err(|_| "Invalid connection configuration")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProfileFile {
            version: 1,
            connections: Vec::new(),
        },
        Err(_) => return Err("Could not read connection configuration. Check permissions.".into()),
    };
    file.connections.push(profile);
    let contents = serde_json::to_vec_pretty(&file)
        .map_err(|_| "Could not encode connection configuration")?;
    let profiles = parse_profiles(
        std::str::from_utf8(&contents).map_err(|_| "Could not encode connection configuration")?,
        |name| std::env::var(name).ok(),
    )
    .map_err(str::to_owned)?;
    atomic_write(path, &contents)?;
    Ok(profiles)
}

const KEYRING_SERVICE: &str = "dbviewer.postgresql";

fn keyring_entry(id: &str) -> Result<keyring::Entry, String> {
    #[cfg(not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "macos",
        target_os = "ios",
        target_os = "windows"
    )))]
    return Err("The system keyring is not supported on this platform".into());

    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "macos",
        target_os = "ios",
        target_os = "windows"
    ))]
    keyring::Entry::new(KEYRING_SERVICE, id)
        .map_err(|_| "Could not access the system keyring".into())
}

fn read_keyring_password(id: &str) -> Result<String, String> {
    password_from_entry(&keyring_entry(id)?)
}

fn password_from_entry(entry: &keyring::Entry) -> Result<String, String> {
    entry.get_password().map_err(|error| match error {
        keyring::Error::NoEntry => "The saved password is missing from the system keyring".into(),
        _ => "Could not read the password from the system keyring. Unlock it and try again".into(),
    })
}

fn save_profile_with_password(
    path: &Path,
    spec: ProfileSpec,
    password: &str,
) -> Result<Vec<ConnectionProfile>, String> {
    let entry = if spec.password_in_keyring {
        Some(keyring_entry(&spec.id)?)
    } else {
        None
    };
    save_profile_with_entry(path, spec, password, entry.as_ref())
}

fn save_profile_with_entry(
    path: &Path,
    spec: ProfileSpec,
    password: &str,
    entry: Option<&keyring::Entry>,
) -> Result<Vec<ConnectionProfile>, String> {
    if let Some(entry) = entry {
        entry.set_password(password).map_err(|_| {
            "Could not save the password in the system keyring. Unlock it and try again".to_owned()
        })?;
    }
    match append_profile(path, spec) {
        Ok(profiles) => Ok(profiles),
        Err(error) => {
            if let Some(entry) = entry
                && entry.delete_credential().is_err()
            {
                return Err(format!("{error} A keyring entry may need manual removal."));
            }
            Err(error)
        }
    }
}

// All new PostgreSQL pools are opened here. A future tunnel must be retained
// with every pool user, and must preserve the database hostname for TLS checks.
pub(crate) async fn connect_direct(
    connection: &ConnectionConfig,
    database: &str,
) -> Result<PgPool, String> {
    let mut options = connection.options.clone().database(database);
    if connection.password_in_keyring {
        let id = connection.id.clone();
        let password = tokio::task::spawn_blocking(move || read_keyring_password(&id))
            .await
            .map_err(|_| "Could not access the system keyring".to_owned())??;
        options = options.password(&password);
    }
    PgPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options)
        .await
        .map_err(|e| {
            dbg!(e);
            format!("Failed to connect to {}", connection.name)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_profile_rejects_invalid_input_without_exposing_passwords() {
        let cases = [
            (
                "  ",
                "postgresql://reader@localhost/app",
                ConnectionTier::Local,
                false,
                "Enter a connection name without control characters.",
            ),
            (
                "Example",
                "postgresql://reader:secret@localhost/app",
                ConnectionTier::Local,
                false,
                "Enter a PostgreSQL URL without a password, query parameters, or fragment. Enter the password in the password field.",
            ),
            (
                "Example",
                "postgresql://reader@localhost/app",
                ConnectionTier::Staging,
                true,
                "TLS can only be disabled for Local connections.",
            ),
            (
                "Example",
                "postgresql://reader@localhost",
                ConnectionTier::Local,
                false,
                "Enter a valid PostgreSQL URL with an explicit host and database. Check PGHOSTADDR if it is set.",
            ),
        ];
        for (name, url, tier, disable_tls, expected) in cases {
            let error =
                PreparedProfile::new(name.into(), url.into(), "secret".into(), tier, disable_tls)
                    .err()
                    .expect("invalid profile must be rejected");
            assert_eq!(error, expected);
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn prepared_profile_save_preserves_the_version_one_profile_format() {
        let dir = std::env::temp_dir().join(format!("dbviewer-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("connections.json");
        let prepared = PreparedProfile {
            path: path.clone(),
            spec: ProfileSpec {
                id: "profile-id".into(),
                name: "Profile".into(),
                tier: ConnectionTier::Local,
                url: Some("postgresql://reader@localhost/app".into()),
                url_env: None,
                tls: TlsPolicy::Disable,
                password_in_keyring: false,
            },
            password: String::new(),
        };
        let (profiles, id) = prepared.save().unwrap();
        assert_eq!(id, "profile-id");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id(), id);
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            document,
            serde_json::json!({
                "version": 1,
                "connections": [{
                    "id": "profile-id",
                    "name": "Profile",
                    "tier": "local",
                    "url": "postgresql://reader@localhost/app",
                    "tls": "disable"
                }]
            })
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_urls_cannot_contain_credentials_or_query_parameters() {
        assert!(is_safe_saved_url("postgresql://reader@db.example.test/app"));
        for value in [
            "postgresql://reader:secret@db.example.test/app",
            "postgresql://reader@db.example.test/app?password=secret",
            "postgresql://reader@db.example.test/app?sslmode=disable",
            "postgresql://reader@db.example.test/app#secret",
            "https://db.example.test/app",
        ] {
            assert!(!is_safe_saved_url(value));
        }
    }

    #[test]
    fn appending_profile_preserves_existing_profiles_and_refuses_invalid_files() {
        let dir = std::env::temp_dir().join(format!("dbviewer-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("connections.json");
        let profile = ProfileSpec {
            id: "first".into(),
            name: "First".into(),
            tier: ConnectionTier::Local,
            url: Some("postgresql://reader@localhost/app".into()),
            url_env: None,
            password_in_keyring: false,
            tls: TlsPolicy::Disable,
        };
        let profiles = append_profile(&path, profile.clone()).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id(), "first");
        let mut second = profile.clone();
        second.id = "second".into();
        assert_eq!(append_profile(&path, second).unwrap().len(), 2);
        let original = std::fs::read(&path).unwrap();
        assert!(append_profile(&path, profile).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::write(&path, b"{ invalid").unwrap();
        let invalid = std::fs::read(&path).unwrap();
        let another = ProfileSpec {
            id: "third".into(),
            name: "Third".into(),
            tier: ConnectionTier::Local,
            url: Some("postgresql://reader@localhost/app".into()),
            url_env: None,
            password_in_keyring: false,
            tls: TlsPolicy::Disable,
        };
        assert!(append_profile(&path, another).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn password_is_stored_in_keyring_and_never_in_profile() {
        let dir = std::env::temp_dir().join(format!("dbviewer-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("connections.json");
        let entry =
            keyring::Entry::new_with_credential(Box::new(keyring::mock::MockCredential::default()));
        let profile = ProfileSpec {
            id: "profile".into(),
            name: "Profile".into(),
            tier: ConnectionTier::Local,
            url: Some("postgresql://reader@localhost/app".into()),
            url_env: None,
            tls: TlsPolicy::Disable,
            password_in_keyring: true,
        };
        let profiles =
            save_profile_with_entry(&path, profile.clone(), "secret password", Some(&entry))
                .unwrap();
        assert_eq!(password_from_entry(&entry).unwrap(), "secret password");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.contains("secret password"));
        assert!(profiles[0].connection.as_ref().unwrap().password_in_keyring);
        assert!(
            parse_profiles(&contents, |_| None).unwrap()[0]
                .connection
                .as_ref()
                .unwrap()
                .password_in_keyring
        );

        std::fs::write(&path, "invalid JSON").unwrap();
        assert!(save_profile_with_entry(&path, profile, "another secret", Some(&entry)).is_err());
        assert!(matches!(entry.get_password(), Err(keyring::Error::NoEntry)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid JSON");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_keyring_write_does_not_save_profile() {
        let dir = std::env::temp_dir().join(format!("dbviewer-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("connections.json");
        let entry =
            keyring::Entry::new_with_credential(Box::new(keyring::mock::MockCredential::default()));
        let mock = entry
            .get_credential()
            .downcast_ref::<keyring::mock::MockCredential>()
            .unwrap();
        mock.set_error(keyring::Error::NoEntry);
        let profile = ProfileSpec {
            id: "profile".into(),
            name: "Profile".into(),
            tier: ConnectionTier::Local,
            url: Some("postgresql://reader@localhost/app".into()),
            url_env: None,
            tls: TlsPolicy::Disable,
            password_in_keyring: true,
        };
        assert!(save_profile_with_entry(&path, profile, "secret", Some(&entry)).is_err());
        assert!(!path.exists());
        assert!(password_from_entry(&entry).unwrap_err().contains("missing"));
    }

    #[test]
    fn last_connection_file_round_trips_only_an_id() {
        let dir = std::env::temp_dir().join(format!("dbviewer-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("last-connection.json");
        atomic_write(&path, &serde_json::to_vec("profile-id").unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<String>(&std::fs::read(&path).unwrap()).unwrap(),
            "profile-id"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restores_only_the_remembered_configured_profile() {
        let first = ConnectionProfile::from_connection(ConnectionConfig::local());
        let mut second = first.clone();
        second.id = "second".into();
        second.connection.as_mut().unwrap().id = "second".into();
        let unavailable = ConnectionProfile {
            id: "unavailable".into(),
            name: "Unavailable".into(),
            tier: ConnectionTier::Local,
            connection: None,
        };
        let profiles = vec![first, second, unavailable];
        assert_eq!(
            restore_connection(&profiles, "second").unwrap().id,
            "second"
        );
        assert!(restore_connection(&profiles, "missing").is_none());
        assert!(restore_connection(&profiles, "unavailable").is_none());
    }

    #[test]
    fn local_connection_pins_the_tcp_address_to_loopback() {
        use sqlx::ConnectOptions as _;

        let connection = ConnectionConfig::local();
        let url = connection.options.to_url_lossy();
        assert_eq!(connection.options.get_host(), "127.0.0.1");
        assert!(connection.options.get_socket().is_none());
        assert!(
            url.query_pairs()
                .any(|(key, value)| { key == "hostaddr" && value == "127.0.0.1" })
        );
    }

    #[test]
    fn profiles_accept_multiple_connections_in_one_tier_and_keep_secrets_out_of_errors() {
        let contents = r#"{"version":1,"connections":[
            {"id":"first","name":"First","tier":"local","url_env":"FIRST_URL","tls":"disable"},
            {"id":"second","name":"Second","tier":"local","url_env":"SECOND_URL"},
            {"id":"missing","name":"Missing","tier":"production","url_env":"MISSING_URL"}
        ]}"#;
        let profiles = parse_profiles(contents, |name| match name {
            "FIRST_URL" | "SECOND_URL" => {
                Some("postgres://reader:secret@db.example.test/app?hostaddr=192.0.2.1".into())
            }
            _ => None,
        })
        .unwrap();
        assert_eq!(profiles.len(), 3);
        assert_eq!(profiles[0].id(), "first");
        assert_eq!(profiles[1].id(), "second");
        assert!(matches!(
            profiles[0]
                .connection
                .as_ref()
                .unwrap()
                .options
                .get_ssl_mode(),
            PgSslMode::Disable
        ));
        assert!(matches!(
            profiles[1]
                .connection
                .as_ref()
                .unwrap()
                .options
                .get_ssl_mode(),
            PgSslMode::VerifyFull
        ));
        assert!(!profiles[2].is_configured());
        let error = parse_profiles(
            r#"{"version":1,"connections":[],"password":"secret"}"#,
            |_| None,
        )
        .err()
        .unwrap();
        assert!(!error.contains("secret"));
    }

    #[test]
    fn profiles_reject_duplicate_ids_and_unverified_remote_connections() {
        let duplicate = r#"{"version":1,"connections":[{"id":"same","name":"A","tier":"local","url_env":"A"},{"id":"same","name":"B","tier":"local","url_env":"B"}]}"#;
        assert!(parse_profiles(duplicate, |_| None).is_err());
        let unsafe_tls = r#"{"version":1,"connections":[{"id":"prod","name":"Prod","tier":"production","url_env":"PROD","tls":"disable"}]}"#;
        assert!(parse_profiles(unsafe_tls, |_| None).is_err());
        assert!(parse_profiles(r#"{"version":2,"connections":[]}"#, |_| None).is_err());
        assert!(parse_profiles(r#"{"version":1,"connections":[{"id":"x","name":"X","tier":"staging","url_env":"X","transport":"ssh"}]}"#, |_| None).is_err());
    }

    #[test]
    fn remote_urls_require_explicit_tcp_host_and_database() {
        let invalid = [
            "",
            "not a url",
            "https://db.example.test/app",
            "postgres://user:secret@/app",
            "postgres://user:secret@db.example.test",
            "postgres://user:secret@db.example.test/",
            "postgres://user:secret@db.example.test/app?host=",
            "postgres://user:secret@db.example.test/app?dbname=",
            "postgres://user:secret@db.example.test/app?host=%2Ftmp",
            "postgres://user:secret@%2Ftmp/app?host=db.example.test",
            "postgres://user:secret@db.example.test/app?unknown=secret",
            "postgres://user:secret@db.example.test/app?sslmode=not-a-mode",
        ];
        for value in invalid {
            assert!(ConnectionConfig::remote(ConnectionTier::Staging, value).is_none());
        }
    }

    #[test]
    fn remote_urls_force_verified_tls_and_preserve_options_for_other_databases() {
        let connection = ConnectionConfig::remote(
            ConnectionTier::Production,
            "postgres://reader:secret@db.example.test:6543/app?hostaddr=192.0.2.1&sslmode=disable&application_name=viewer",
        ).expect("valid configuration");
        assert_eq!(connection.tier, ConnectionTier::Production);
        assert_eq!(connection.database(), "app");
        let other_database = connection.options.clone().database("other");
        assert_eq!(other_database.get_host(), "db.example.test");
        assert_eq!(other_database.get_port(), 6543);
        assert_eq!(other_database.get_username(), "reader");
        assert_eq!(other_database.get_database(), Some("other"));
        assert_eq!(other_database.get_application_name(), Some("viewer"));
        assert!(matches!(
            other_database.get_ssl_mode(),
            PgSslMode::VerifyFull
        ));

        let query_host = ConnectionConfig::remote(
            ConnectionTier::Staging,
            "postgres:///?host=db.example.test&hostaddr=192.0.2.1&dbname=inventory",
        )
        .expect("explicit query host and database");
        assert_eq!(query_host.options.get_host(), "db.example.test");
        assert_eq!(query_host.database(), "inventory");
        assert!(matches!(
            query_host.options.get_ssl_mode(),
            PgSslMode::VerifyFull
        ));
    }
}
