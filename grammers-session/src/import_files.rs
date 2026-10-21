// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Read-only, offline imports of explicitly selected foreign session files.
//!
//! Requires `session-import-files`; does not enable the destination SQLite storage.
//! Supports Pyrogram schema 3, Kurigram schema 7, Telethon schemas 7/8, and gotd
//! Loader JSON v1. Other database versions are rejected without migration.
//! Close the source application and use a checkpointed, stable local file.
//! Nonempty WAL, SHM, or rollback journals are rejected. SQLite is opened with
//! read-only + URI flags and `immutable=1`, so it cannot checkpoint or create
//! sidecars. Concurrent modification is unsupported: immutable reads cannot
//! establish a consistent snapshot of a file another process is changing.
//! No peers, temporary keys, update state, network or authorization are imported.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use crate::import::{
    self, ImportError, ImportOptions, ImportedSession, SessionFormat, endpoint_environment,
    validate_endpoint, validate_key,
};

/// A sanitized file import failure. Underlying paths, SQL and data are discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportFileError {
    /// Opening or reading the supplied existing file failed.
    Io(io::ErrorKind),
    /// The database schema or its version is not explicitly supported.
    UnsupportedSchema,
    /// The database is corrupt, mistyped, incomplete or has ambiguous rows.
    MalformedDatabase,
    /// A nonempty sidecar or a changed source requires a closed, checkpointed copy.
    UnsafeSource,
    /// Format or session fields failed local validation.
    InvalidData(ImportError),
    /// The import future was polled outside a Tokio runtime.
    MissingRuntime,
    /// The blocking worker failed to complete.
    WorkerFailed,
}

impl fmt::Display for ImportFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => f.write_str("cannot read session source file"),
            Self::UnsupportedSchema => f.write_str("unsupported session database schema or version"),
            Self::MalformedDatabase => f.write_str("malformed session database"),
            Self::UnsafeSource => f.write_str("session source is not stable; close the application and provide a checkpointed copy without active sidecars"),
            Self::InvalidData(error) => error.fmt(f),
            Self::MissingRuntime => f.write_str("session file import requires a Tokio runtime"),
            Self::WorkerFailed => f.write_str("session import worker failed"),
        }
    }
}

impl std::error::Error for ImportFileError {}

impl From<ImportError> for ImportFileError {
    fn from(error: ImportError) -> Self {
        Self::InvalidData(error)
    }
}

fn io_error(error: io::Error) -> ImportFileError {
    ImportFileError::Io(error.kind())
}

fn sql_error(_: libsql::Error) -> ImportFileError {
    ImportFileError::MalformedDatabase
}

/// Imports an existing Pyrogram v3 or Kurigram v7 SQLite file without modifying it.
///
/// Requires a Tokio runtime. The source must stay closed and stable throughout.
/// No destination file is created; the result can become an independent memory session.
///
/// # Errors
/// Returns a sanitized I/O, schema, database, unsafe-source or field validation error.
pub async fn import_pyrogram_file(
    path: impl AsRef<Path>,
    options: ImportOptions,
) -> Result<ImportedSession, ImportFileError> {
    import_sqlite(path.as_ref().to_owned(), options, true).await
}

/// Imports an existing Telethon v7/v8 SQLite file without modifying it.
///
/// Requires a Tokio runtime and a closed, stable source. Temporary keys, entities
/// and takeout metadata are deliberately not imported. The exact endpoint is retained.
///
/// # Errors
/// Returns a sanitized I/O, schema, database, unsafe-source or field validation error.
pub async fn import_telethon_file(
    path: impl AsRef<Path>,
    options: ImportOptions,
) -> Result<ImportedSession, ImportFileError> {
    import_sqlite(path.as_ref().to_owned(), options, false).await
}

/// Reads a bounded gotd Loader v1 JSON file and imports it offline.
///
/// Requires a Tokio runtime. Reading and parsing run on its blocking pool.
/// The one MiB limit is checked both before allocation and while reading.
///
/// # Errors
/// Returns a sanitized I/O or format/field validation error.
pub async fn import_gotd_file(
    path: impl AsRef<Path>,
    options: ImportOptions,
) -> Result<ImportedSession, ImportFileError> {
    tokio::runtime::Handle::try_current().map_err(|_| ImportFileError::MissingRuntime)?;
    let path = path.as_ref().to_owned();
    tokio::task::spawn_blocking(move || {
        let file = File::open(path).map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() {
            return Err(ImportFileError::Io(io::ErrorKind::InvalidInput));
        }
        if metadata.len() > import::JSON_LIMIT as u64 {
            return Err(ImportError::InputTooLarge.into());
        }
        let input = read_json(file)?;
        import::parse_gotd_json(&input, options).map_err(Into::into)
    })
    .await
    .map_err(|_| ImportFileError::WorkerFailed)?
}

fn read_json(reader: impl Read) -> Result<String, ImportFileError> {
    let mut input = String::new();
    reader
        .take(import::JSON_LIMIT as u64 + 1)
        .read_to_string(&mut input)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                ImportFileError::InvalidData(ImportError::InvalidJson)
            } else {
                io_error(error)
            }
        })?;
    if input.len() > import::JSON_LIMIT {
        return Err(ImportError::InputTooLarge.into());
    }
    Ok(input)
}

async fn import_sqlite(
    path: PathBuf,
    options: ImportOptions,
    pyrogram: bool,
) -> Result<ImportedSession, ImportFileError> {
    let runtime =
        tokio::runtime::Handle::try_current().map_err(|_| ImportFileError::MissingRuntime)?;
    tokio::task::spawn_blocking(move || runtime.block_on(async move {
        let path = fs::canonicalize(path).map_err(io_error)?;
        let before = stable_source(&path)?;
        let uri = immutable_uri(&path)?;
        // libsql exposes only three named flags, but retains SQLite's URI bit.
        // SQLITE_OPEN_URI (0x40) enables immutable independently of global config.
        let flags = libsql::OpenFlags::SQLITE_OPEN_READ_ONLY
            | libsql::OpenFlags::from_bits_retain(0x40);
        let database = libsql::Builder::new_local(uri).flags(flags).build().await.map_err(sql_error)?;
        let connection = database.connect().map_err(sql_error)?;
        // Reject views and virtual tables before any schema-controlled query.
        let version_column = if pyrogram { "number" } else { "version" };
        schema(&connection, "version", &[(version_column, "INTEGER")]).await?;
        let version = single_row(&connection, if pyrogram {
            "SELECT CASE WHEN typeof(number)='integer' THEN number END FROM version LIMIT 2"
        } else { "SELECT CASE WHEN typeof(version)='integer' THEN version END FROM version LIMIT 2" }).await?;
        let version = integer(&version, 0)?;
        let imported = if pyrogram {
            read_pyrogram(&connection, version, options).await?
        } else {
            read_telethon(&connection, version, options).await?
        };
        drop(connection);
        drop(database);
        let after = stable_source(&path)?;
        if before != after {
            return Err(ImportFileError::UnsafeSource);
        }
        Ok(imported)
    }))
    .await.map_err(|_| ImportFileError::WorkerFailed)?
}

fn stable_source(path: &Path) -> Result<(u64, Option<std::time::SystemTime>), ImportFileError> {
    let metadata = fs::metadata(path).map_err(io_error)?;
    if !metadata.is_file() {
        return Err(ImportFileError::Io(io::ErrorKind::InvalidInput));
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        match fs::metadata(sidecar) {
            Ok(metadata) if metadata.len() != 0 || !metadata.is_file() => {
                return Err(ImportFileError::UnsafeSource);
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok((metadata.len(), metadata.modified().ok()))
}

fn immutable_uri(path: &Path) -> Result<String, ImportFileError> {
    let path = path
        .to_str()
        .ok_or(ImportFileError::Io(io::ErrorKind::InvalidInput))?;
    // Windows canonicalization adds an extended path prefix; SQLite URI paths
    // use regular absolute drive paths. Reject UNC paths to keep imports local.
    let path = path
        .strip_prefix(r"\\?\")
        .unwrap_or(path)
        .replace('\\', "/");
    if path.starts_with("//") || path.starts_with("UNC/") {
        return Err(ImportFileError::Io(io::ErrorKind::InvalidInput));
    }
    let mut uri = String::from("file:");
    if !path.starts_with('/') {
        uri.push('/');
    }
    use std::fmt::Write as _;
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/:-._~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            write!(&mut uri, "%{byte:02X}").expect("writing a string cannot fail");
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    Ok(uri)
}

async fn single_row(
    connection: &libsql::Connection,
    query: &str,
) -> Result<Vec<libsql::Value>, ImportFileError> {
    let mut rows = connection.query(query, ()).await.map_err(sql_error)?;
    let row = rows
        .next()
        .await
        .map_err(sql_error)?
        .ok_or(ImportFileError::MalformedDatabase)?;
    // libsql's local Row is backed by its statement. Copy the bounded selected
    // values before stepping again, which invalidates that row's column values.
    let values = (0..rows.column_count())
        .map(|index| row.get_value(index))
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    if rows.next().await.map_err(sql_error)?.is_some() {
        return Err(ImportFileError::MalformedDatabase);
    }
    Ok(values)
}

async fn schema(
    connection: &libsql::Connection,
    table: &str,
    columns: &[(&str, &str)],
) -> Result<(), ImportFileError> {
    let mut rows = connection
        .query(
            "SELECT type, substr(sql, 1, 64) FROM sqlite_schema WHERE name = ? LIMIT 2",
            [table],
        )
        .await
        .map_err(sql_error)?;
    let row = rows
        .next()
        .await
        .map_err(sql_error)?
        .ok_or(ImportFileError::UnsupportedSchema)?;
    let kind = row.get::<String>(0).map_err(sql_error)?;
    let ddl = row.get::<String>(1).map_err(sql_error)?;
    if kind != "table"
        || !ddl.to_ascii_uppercase().starts_with("CREATE TABLE")
        || rows.next().await.map_err(sql_error)?.is_some()
    {
        return Err(ImportFileError::UnsupportedSchema);
    }
    let mut rows = connection.query("SELECT substr(name, 1, 64), substr(type, 1, 32), hidden FROM pragma_table_xinfo(?) LIMIT 16", [table])
        .await.map_err(sql_error)?;
    let mut found = Vec::new();
    while let Some(row) = rows.next().await.map_err(sql_error)? {
        let name = row.get::<String>(0).map_err(sql_error)?;
        let kind = row
            .get::<String>(1)
            .map_err(sql_error)?
            .to_ascii_uppercase();
        if row.get::<i64>(2).map_err(sql_error)? != 0
            || !columns.iter().any(|(n, k)| *n == name && *k == kind)
            || found.contains(&name)
        {
            return Err(ImportFileError::UnsupportedSchema);
        }
        found.push(name);
    }
    if found.len() != columns.len() {
        return Err(ImportFileError::UnsupportedSchema);
    }
    Ok(())
}

fn integer(row: &[libsql::Value], index: usize) -> Result<i64, ImportFileError> {
    // libsql's conversions must not accept SQLite's dynamic text/real values.
    match row.get(index) {
        Some(libsql::Value::Integer(value)) => Ok(*value),
        _ => Err(ImportFileError::MalformedDatabase),
    }
}

fn boolean(row: &[libsql::Value], index: usize) -> Result<bool, ImportFileError> {
    match integer(row, index)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ImportError::InvalidBoolean.into()),
    }
}

fn dc_id(row: &[libsql::Value]) -> Result<i32, ImportFileError> {
    i32::try_from(integer(row, 0)?).map_err(|_| ImportError::InvalidDc.into())
}

fn auth_key(row: &[libsql::Value], index: usize) -> Result<[u8; 256], ImportFileError> {
    match row.get(index) {
        Some(libsql::Value::Blob(bytes)) => validate_key(bytes).map_err(Into::into),
        _ => Err(ImportError::InvalidAuthKey.into()),
    }
}

fn endpoint(
    row: &[libsql::Value],
    address: usize,
    port: usize,
) -> Result<SocketAddr, ImportFileError> {
    let Some(libsql::Value::Text(address)) = row.get(address) else {
        return Err(ImportError::InvalidAddress.into());
    };
    let ip: IpAddr = address.parse().map_err(|_| ImportError::InvalidAddress)?;
    let port = u16::try_from(integer(row, port)?).map_err(|_| ImportError::InvalidAddress)?;
    validate_endpoint(SocketAddr::new(ip, port)).map_err(Into::into)
}

async fn read_pyrogram(
    connection: &libsql::Connection,
    version: i64,
    options: ImportOptions,
) -> Result<ImportedSession, ImportFileError> {
    if !matches!(version, 3 | 7) {
        return Err(ImportFileError::UnsupportedSchema);
    }
    let mut columns = vec![
        ("dc_id", "INTEGER"),
        ("api_id", "INTEGER"),
        ("test_mode", "INTEGER"),
        ("auth_key", "BLOB"),
        ("date", "INTEGER"),
        ("user_id", "INTEGER"),
        ("is_bot", "INTEGER"),
    ];
    if version == 7 {
        columns.extend([("server_address", "TEXT"), ("port", "INTEGER")]);
    }
    schema(connection, "sessions", &columns).await?;
    // CASE bounds credential and address materialization even for huge hostile
    // values. Never select peer/user inventories or temporary authentication keys.
    let query = if version == 3 {
        "SELECT CASE WHEN typeof(dc_id)='integer' THEN dc_id END, CASE WHEN typeof(api_id) IN ('integer','null') THEN api_id ELSE -1 END, CASE WHEN typeof(test_mode)='integer' THEN test_mode END, CASE WHEN typeof(auth_key)='blob' AND length(auth_key)=256 THEN auth_key END, CASE WHEN typeof(user_id)='integer' THEN user_id END, CASE WHEN typeof(is_bot)='integer' THEN is_bot END FROM sessions LIMIT 2"
    } else {
        "SELECT CASE WHEN typeof(dc_id)='integer' THEN dc_id END, CASE WHEN typeof(api_id) IN ('integer','null') THEN api_id ELSE -1 END, CASE WHEN typeof(test_mode)='integer' THEN test_mode END, CASE WHEN typeof(auth_key)='blob' AND length(auth_key)=256 THEN auth_key END, CASE WHEN typeof(user_id)='integer' THEN user_id END, CASE WHEN typeof(is_bot)='integer' THEN is_bot END, CASE WHEN typeof(server_address)='text' AND length(CAST(server_address AS BLOB))<=64 THEN server_address END, CASE WHEN typeof(port)='integer' THEN port END FROM sessions LIMIT 2"
    };
    let row = single_row(connection, query).await?;
    let dc = dc_id(&row)?;
    let api_id = match &row[1] {
        libsql::Value::Null | libsql::Value::Integer(0) => None,
        libsql::Value::Integer(value) if (1..=i64::from(i32::MAX)).contains(value) => {
            Some(*value as i32)
        }
        _ => return Err(ImportError::InvalidApiId.into()),
    };
    let test_mode = boolean(&row, 2)?;
    let key = auth_key(&row, 3)?;
    let user_id = u64::try_from(integer(&row, 4)?)
        .ok()
        .filter(|id| *id != 0)
        .ok_or(ImportError::InvalidUserId)?;
    let is_bot = boolean(&row, 5)?;
    let endpoint = if version == 7 {
        let endpoint = endpoint(&row, 6, 7)?;
        if endpoint_environment(endpoint.ip()).is_some_and(|known| known != test_mode) {
            return Err(ImportError::InvalidAddress.into());
        }
        Some(endpoint)
    } else {
        None
    };
    let mut imported =
        ImportedSession::from_parts(SessionFormat::Pyrogram, dc, Some(test_mode), endpoint, key)?;
    imported.api_id = api_id;
    imported.user_id = Some(user_id);
    imported.is_bot = Some(is_bot);
    options.apply(imported).map_err(Into::into)
}

async fn read_telethon(
    connection: &libsql::Connection,
    version: i64,
    options: ImportOptions,
) -> Result<ImportedSession, ImportFileError> {
    if !matches!(version, 7 | 8) {
        return Err(ImportFileError::UnsupportedSchema);
    }
    let mut columns = vec![
        ("dc_id", "INTEGER"),
        ("server_address", "TEXT"),
        ("port", "INTEGER"),
        ("auth_key", "BLOB"),
        ("takeout_id", "INTEGER"),
    ];
    if version == 8 {
        columns.push(("tmp_auth_key", "BLOB"));
    }
    schema(connection, "sessions", &columns).await?;
    let row = single_row(connection, "SELECT CASE WHEN typeof(dc_id)='integer' THEN dc_id END, CASE WHEN typeof(server_address)='text' AND length(CAST(server_address AS BLOB))<=64 THEN server_address END, CASE WHEN typeof(port)='integer' THEN port END, CASE WHEN typeof(auth_key)='blob' AND length(auth_key)=256 THEN auth_key END FROM sessions LIMIT 2").await?;
    let dc = dc_id(&row)?;
    let endpoint = endpoint(&row, 1, 2)?;
    let key = auth_key(&row, 3)?;
    let imported = ImportedSession::from_parts(
        SessionFormat::Telethon,
        dc,
        endpoint_environment(endpoint.ip()),
        Some(endpoint),
        key,
    )?;
    options.apply(imported).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Essential table DDL follows the official source references documented in
    // README.md. Unrelated inventory tables are never queried by the importer.
    const PYROGRAM: &str = "CREATE TABLE sessions(dc_id INTEGER PRIMARY KEY, api_id INTEGER, test_mode INTEGER, auth_key BLOB, date INTEGER NOT NULL, user_id INTEGER, is_bot INTEGER); CREATE TABLE version(number INTEGER PRIMARY KEY); INSERT INTO version VALUES(3);";
    const KURIGRAM: &str = "CREATE TABLE sessions(dc_id INTEGER PRIMARY KEY, server_address TEXT, port INTEGER, api_id INTEGER, test_mode INTEGER, auth_key BLOB, date INTEGER NOT NULL, user_id INTEGER, is_bot INTEGER); CREATE TABLE version(number INTEGER PRIMARY KEY); INSERT INTO version VALUES(7);";
    const TELETHON7: &str = "CREATE TABLE version(version INTEGER PRIMARY KEY); INSERT INTO version VALUES(7); CREATE TABLE sessions(dc_id INTEGER PRIMARY KEY, server_address TEXT, port INTEGER, auth_key BLOB, takeout_id INTEGER);";
    const TELETHON8: &str = "CREATE TABLE version(version INTEGER PRIMARY KEY); INSERT INTO version VALUES(8); CREATE TABLE sessions(dc_id INTEGER PRIMARY KEY, server_address TEXT, port INTEGER, auth_key BLOB, takeout_id INTEGER, tmp_auth_key BLOB);";

    struct Fixture {
        directory: PathBuf,
        path: PathBuf,
        runtime: tokio::runtime::Runtime,
    }

    impl Fixture {
        fn new(schema: &str, insert: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "grammers-fixture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            // These URI metacharacters exercise path escaping as well.
            let path = directory.join("synthetic # percent%.session");
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let fixture = Self {
                directory,
                path,
                runtime,
            };
            fixture.execute(schema);
            fixture.execute(insert);
            fixture
        }

        fn execute(&self, sql: &str) {
            self.runtime.block_on(async {
                let db = libsql::Builder::new_local(&self.path)
                    .build()
                    .await
                    .unwrap();
                let connection = db.connect().unwrap();
                connection.execute_batch(sql).await.unwrap();
            });
        }

        fn pyro(&self, options: ImportOptions) -> Result<ImportedSession, ImportFileError> {
            self.runtime
                .block_on(import_pyrogram_file(&self.path, options))
        }

        fn tele(&self, options: ImportOptions) -> Result<ImportedSession, ImportFileError> {
            self.runtime
                .block_on(import_telethon_file(&self.path, options))
        }

        fn snapshot(&self) -> Vec<(std::ffi::OsString, Vec<u8>)> {
            let mut files: Vec<_> = fs::read_dir(&self.directory)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), fs::read(entry.path()).unwrap())
                })
                .collect();
            files.sort_by(|left, right| left.0.cmp(&right.0));
            files
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            #[cfg(windows)]
            if let Ok(metadata) = fs::metadata(&self.path) {
                let mut permissions = metadata.permissions();
                // Windows-only test fixture cleanup. Unix unlink needs no mode change.
                #[expect(
                    clippy::permissions_set_readonly_false,
                    reason = "Windows-only removal of a synthetic read-only fixture"
                )]
                permissions.set_readonly(false);
                fs::set_permissions(&self.path, permissions).unwrap();
            }
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    fn pyro_insert() -> &'static str {
        "INSERT INTO sessions VALUES(2, 12345, 0, randomblob(256), 0, 9876543210, 1)"
    }

    fn tele_insert(version: u8, address: &str, port: u16) -> String {
        let tail = if version == 8 {
            ", zeroblob(1048576)"
        } else {
            ""
        };
        format!("INSERT INTO sessions VALUES(2, '{address}', {port}, randomblob(256), NULL{tail})")
    }

    #[test]
    fn imports_pyrogram_3_with_metadata_and_only_home_key() {
        let fixture = Fixture::new(PYROGRAM, pyro_insert());
        // An unrelated/maliciously named table and peer inventory are ignored.
        fixture.execute("CREATE TABLE peers(secret TEXT); INSERT INTO peers VALUES('PRIVATE_FIXTURE'); CREATE TABLE \"sessions; DROP TABLE sessions\"(ignored BLOB)");
        let before = fixture.snapshot();
        let imported = fixture.pyro(ImportOptions::default()).unwrap();
        assert_eq!(imported.format(), SessionFormat::Pyrogram);
        assert_eq!(imported.dc_id(), 2);
        assert_eq!(imported.api_id(), Some(12345));
        assert_eq!(imported.user_id(), Some(9876543210));
        assert_eq!(imported.is_bot(), Some(true));
        assert_eq!(imported.test_mode(), Some(false));
        assert!(imported.session_data().peer_infos.is_empty());
        assert_eq!(
            imported
                .session_data()
                .dc_options
                .values()
                .filter(|dc| dc.auth_key.is_some())
                .count(),
            1
        );
        assert_eq!(fixture.snapshot(), before);
    }

    #[test]
    fn pyrogram_zero_and_null_api_id_are_missing_and_test_routes_stay_test() {
        let fixture = Fixture::new(PYROGRAM, pyro_insert());
        for api_id in ["0", "NULL"] {
            fixture.execute(&format!("UPDATE sessions SET api_id={api_id}, test_mode=1"));
            let imported = fixture.pyro(ImportOptions::default()).unwrap();
            assert_eq!(imported.api_id(), None);
            assert_eq!(imported.test_mode(), Some(true));
            assert_eq!(imported.session_data().dc_options.len(), 3);
            assert_eq!(
                imported.session_data().dc_options[&2].ipv4.to_string(),
                "149.154.167.40:80"
            );
        }
    }

    #[test]
    fn kurigram_7_keeps_exact_custom_endpoint_and_flags() {
        let fixture = Fixture::new(
            KURIGRAM,
            "INSERT INTO sessions VALUES(2, '192.0.2.42', 2443, 0, 1, randomblob(256), 0, 1234, 0)",
        );
        let imported = fixture.pyro(ImportOptions::default()).unwrap();
        assert_eq!(imported.api_id(), None);
        assert_eq!(imported.is_bot(), Some(false));
        assert_eq!(
            imported.session_data().dc_options[&2].ipv4.to_string(),
            "192.0.2.42:2443"
        );
        fixture.execute("UPDATE sessions SET server_address='149.154.167.41'");
        assert!(matches!(
            fixture.pyro(ImportOptions::default()),
            Err(ImportFileError::InvalidData(ImportError::InvalidAddress))
        ));
    }

    #[test]
    fn telethon_7_and_8_keep_exact_routes_and_skip_temporary_keys() {
        for (schema, version) in [(TELETHON7, 7), (TELETHON8, 8)] {
            let fixture = Fixture::new(schema, &tele_insert(version, "192.0.2.99", 8443));
            let before = fixture.snapshot();
            let imported = fixture.tele(ImportOptions::default()).unwrap();
            assert_eq!(imported.format(), SessionFormat::Telethon);
            assert_eq!(imported.test_mode(), None);
            assert_eq!(imported.api_id(), None);
            assert_eq!(imported.user_id(), None);
            assert_eq!(imported.session_data().dc_options.len(), 1);
            assert_eq!(
                imported.session_data().dc_options[&2].ipv4.to_string(),
                "192.0.2.99:8443"
            );
            assert_eq!(fixture.snapshot(), before);
        }
    }

    #[test]
    fn telethon_test_endpoints_and_ipv6_require_explicit_preference() {
        let fixture = Fixture::new(TELETHON7, &tele_insert(7, "149.154.167.40", 443));
        let imported = fixture.tele(ImportOptions::default()).unwrap();
        assert_eq!(imported.test_mode(), Some(true));
        assert_eq!(imported.session_data().dc_options.len(), 3);
        assert_eq!(imported.session_data().dc_options[&2].ipv4.port(), 443);
        fixture.execute("UPDATE sessions SET server_address='2001:db8::42', port=9443");
        assert!(matches!(
            fixture.tele(ImportOptions::default()),
            Err(ImportFileError::InvalidData(
                ImportError::Ipv6PreferenceRequired
            ))
        ));
        let imported = fixture.tele(ImportOptions { prefer_ipv6: true }).unwrap();
        assert!(imported.prefer_ipv6());
        assert_eq!(imported.session_data().dc_options.len(), 1);
        assert_eq!(
            imported.session_data().dc_options[&2].ipv6.to_string(),
            "[2001:db8::42]:9443"
        );
        assert!(
            imported.session_data().dc_options[&2]
                .ipv4
                .ip()
                .is_unspecified()
        );
    }

    #[test]
    fn refuses_missing_wrong_and_unsupported_schema_without_migration() {
        for mutation in [
            "DROP TABLE sessions",
            "ALTER TABLE sessions RENAME COLUMN auth_key TO invalid",
            "ALTER TABLE sessions ADD COLUMN unknown TEXT",
            "UPDATE version SET number=2",
            "UPDATE version SET number=99",
            "ALTER TABLE version RENAME COLUMN number TO invalid",
            "DROP TABLE sessions; CREATE VIEW sessions AS SELECT 2 AS dc_id",
        ] {
            let fixture = Fixture::new(PYROGRAM, pyro_insert());
            fixture.execute(mutation);
            let before = fixture.snapshot();
            assert!(matches!(
                fixture.pyro(ImportOptions::default()),
                Err(ImportFileError::UnsupportedSchema)
            ));
            assert_eq!(fixture.snapshot(), before);
        }
        for version in [6, 9] {
            let fixture = Fixture::new(TELETHON7, &tele_insert(7, "192.0.2.1", 443));
            fixture.execute(&format!("UPDATE version SET version={version}"));
            let before = fixture.snapshot();
            assert!(matches!(
                fixture.tele(ImportOptions::default()),
                Err(ImportFileError::UnsupportedSchema)
            ));
            assert_eq!(fixture.snapshot(), before);
        }
    }

    #[test]
    fn refuses_empty_or_ambiguous_rows() {
        for mutation in [
            "DELETE FROM sessions",
            "INSERT INTO sessions SELECT 3, api_id, test_mode, auth_key, date, user_id, is_bot FROM sessions",
            "DELETE FROM version",
            "INSERT INTO version VALUES(7)",
        ] {
            let fixture = Fixture::new(PYROGRAM, pyro_insert());
            fixture.execute(mutation);
            assert!(matches!(
                fixture.pyro(ImportOptions::default()),
                Err(ImportFileError::MalformedDatabase)
            ));
        }
    }

    #[test]
    fn rejects_bad_keys_without_materializing_large_blobs() {
        for key in [
            "NULL",
            "zeroblob(256)",
            "randomblob(255)",
            "randomblob(257)",
            "zeroblob(8*1024*1024)",
            "'not-a-key'",
        ] {
            let fixture = Fixture::new(PYROGRAM, pyro_insert());
            fixture.execute(&format!("UPDATE sessions SET auth_key={key}"));
            assert!(matches!(
                fixture.pyro(ImportOptions::default()),
                Err(ImportFileError::InvalidData(ImportError::InvalidAuthKey))
            ));
        }
    }

    #[test]
    fn rejects_malformed_metadata_and_endpoints() {
        for mutation in [
            "api_id=-1",
            "api_id=2147483648",
            "api_id='private'",
            "test_mode=2",
            "is_bot=-1",
            "user_id=0",
            "user_id=-1",
            "dc_id=0",
            "dc_id=2147483648",
            "test_mode=0.5",
            "user_id=zeroblob(1048576)",
        ] {
            let fixture = Fixture::new(PYROGRAM, pyro_insert());
            fixture.execute(&format!("UPDATE sessions SET {mutation}"));
            assert!(
                fixture.pyro(ImportOptions::default()).is_err(),
                "mutation {mutation}"
            );
        }
        for mutation in [
            "server_address='example.com'",
            "server_address='0.0.0.0'",
            "server_address='224.0.0.1'",
            "server_address=zeroblob(1048576)",
            "port=0",
            "port=65536",
            "port=-1",
            "port=443.5",
            "port='private'",
        ] {
            let fixture = Fixture::new(TELETHON7, &tele_insert(7, "192.0.2.1", 443));
            fixture.execute(&format!("UPDATE sessions SET {mutation}"));
            assert!(
                fixture.tele(ImportOptions::default()).is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn read_only_source_and_empty_sidecars_are_unchanged() {
        let fixture = Fixture::new(PYROGRAM, pyro_insert());
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = fixture.path.as_os_str().to_owned();
            sidecar.push(suffix);
            fs::write(sidecar, []).unwrap();
        }
        let mut permissions = fs::metadata(&fixture.path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&fixture.path, permissions).unwrap();
        let before = fixture.snapshot();
        fixture.pyro(ImportOptions::default()).unwrap();
        assert_eq!(fixture.snapshot(), before);
        assert!(
            fs::metadata(&fixture.path)
                .unwrap()
                .permissions()
                .readonly()
        );
    }

    #[test]
    fn nonempty_wal_shm_or_journal_are_rejected_and_unchanged() {
        for suffix in ["-wal", "-shm", "-journal"] {
            let fixture = Fixture::new(PYROGRAM, pyro_insert());
            let mut sidecar = fixture.path.as_os_str().to_owned();
            sidecar.push(suffix);
            fs::write(sidecar, b"synthetic pending data").unwrap();
            let before = fixture.snapshot();
            assert!(matches!(
                fixture.pyro(ImportOptions::default()),
                Err(ImportFileError::UnsafeSource)
            ));
            assert_eq!(fixture.snapshot(), before);
        }
    }

    #[test]
    fn real_uncheckpointed_wal_is_rejected_without_side_effects() {
        let fixture = Fixture::new(PYROGRAM, pyro_insert());
        fixture.runtime.block_on(async {
            let database = libsql::Builder::new_local(&fixture.path).build().await.unwrap();
            let connection = database.connect().unwrap();
            connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; UPDATE sessions SET api_id=54321").await.unwrap();
            let before = fixture.snapshot();
            assert!(before.iter().any(|(name, bytes)| name.to_string_lossy().ends_with("-wal") && !bytes.is_empty()));
            assert!(matches!(import_pyrogram_file(&fixture.path, ImportOptions::default()).await, Err(ImportFileError::UnsafeSource)));
            assert_eq!(fixture.snapshot(), before);
        });
    }

    #[test]
    fn immutable_uri_connection_is_actually_read_only() {
        let fixture = Fixture::new(PYROGRAM, pyro_insert());
        let before = fixture.snapshot();
        fixture.runtime.block_on(async {
            let uri = immutable_uri(&fs::canonicalize(&fixture.path).unwrap()).unwrap();
            let database = libsql::Builder::new_local(uri)
                .flags(
                    libsql::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | libsql::OpenFlags::from_bits_retain(0x40),
                )
                .build()
                .await
                .unwrap();
            let connection = database.connect().unwrap();
            assert!(
                connection
                    .execute("UPDATE sessions SET api_id=54321", ())
                    .await
                    .is_err()
            );
        });
        assert_eq!(fixture.snapshot(), before);
    }

    #[test]
    fn corrupt_database_errors_are_redacted_and_source_unchanged() {
        let fixture = Fixture::new(PYROGRAM, pyro_insert());
        fs::write(&fixture.path, b"PRIVATE_FIXTURE not sqlite").unwrap();
        let before = fixture.snapshot();
        let error = fixture.pyro(ImportOptions::default()).unwrap_err();
        let message = format!("{error:?} {error}");
        assert!(!message.contains("PRIVATE_FIXTURE"));
        assert!(!message.contains("synthetic"));
        assert_eq!(fixture.snapshot(), before);
    }

    #[test]
    fn gotd_json_file_is_bounded_and_typed() {
        let fixture = Fixture::new(PYROGRAM, pyro_insert());
        let key = base64::engine::general_purpose::STANDARD.encode([42; 256]);
        let json = format!(
            r#"{{"Version":1,"Data":{{"DC":2,"Addr":"192.0.2.42:2443","AuthKey":"{key}","Config":{{"TestMode":false}}}}}}"#
        );
        fs::write(&fixture.path, json).unwrap();
        let before = fixture.snapshot();
        let imported = fixture
            .runtime
            .block_on(import_gotd_file(&fixture.path, ImportOptions::default()))
            .unwrap();
        assert_eq!(imported.format(), SessionFormat::Gotd);
        assert_eq!(imported.session_data().dc_options[&2].ipv4.port(), 2443);
        assert_eq!(fixture.snapshot(), before);
        fs::write(&fixture.path, vec![b' '; import::JSON_LIMIT + 1]).unwrap();
        assert!(matches!(
            fixture
                .runtime
                .block_on(import_gotd_file(&fixture.path, ImportOptions::default())),
            Err(ImportFileError::InvalidData(ImportError::InputTooLarge))
        ));
        // Streaming bound also holds if metadata races or the reader grows.
        assert!(matches!(
            read_json(io::repeat(b' ')),
            Err(ImportFileError::InvalidData(ImportError::InputTooLarge))
        ));
    }

    #[test]
    fn missing_source_is_not_created() {
        let path =
            std::env::temp_dir().join(format!("grammers-missing-{}.session", std::process::id()));
        assert!(!path.exists());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert!(matches!(
            runtime.block_on(import_pyrogram_file(&path, ImportOptions::default())),
            Err(ImportFileError::Io(_))
        ));
        assert!(!path.exists());
        assert!(matches!(
            runtime.block_on(import_telethon_file(&path, ImportOptions::default())),
            Err(ImportFileError::Io(_))
        ));
        assert!(matches!(
            runtime.block_on(import_gotd_file(&path, ImportOptions::default())),
            Err(ImportFileError::Io(_))
        ));
        assert!(!path.exists());
    }

    #[test]
    fn polling_without_tokio_returns_a_sanitized_error() {
        use std::future::Future as _;
        use std::task::{Context, Poll, Waker};
        let mut context = Context::from_waker(Waker::noop());
        let mut import = std::pin::pin!(import_pyrogram_file("unused", ImportOptions::default()));
        assert!(matches!(
            import.as_mut().poll(&mut context),
            Poll::Ready(Err(_))
        ));
        let mut import = std::pin::pin!(import_gotd_file("unused", ImportOptions::default()));
        assert!(matches!(
            import.as_mut().poll(&mut context),
            Poll::Ready(Err(_))
        ));
    }
}
