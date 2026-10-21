// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::net::AddrParseError;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use libsql::{named_params, params};
use tokio::sync::Mutex as AsyncMutex;

use crate::types::{
    ChannelKind, ChannelState, DcOption, PeerAuth, PeerId, PeerInfo, PeerKind, UpdateState,
    UpdatesState,
};
use crate::{BoxFuture, DEFAULT_DC, KNOWN_DC_OPTIONS, Session};

const VERSION: i64 = 1;

struct Database(libsql::Connection);

struct Cache {
    pub home_dc: i32,
    pub dc_options: HashMap<i32, DcOption>,
}

/// SQLite-based storage. This is the recommended option.
pub struct SqliteSession {
    database: AsyncMutex<Database>,
    cache: Mutex<Cache>,
}

#[derive(Debug)]
pub enum SqliteSessionError {
    Poisoned,
    AddrParse(std::net::AddrParseError),
    Sql(libsql::Error),
    InvalidAuthKeyLength(usize),
    /// The native file could not be opened or inspected. Paths are not retained.
    Io(io::ErrorKind),
    /// The file is not an empty database or a supported native session schema.
    UnsupportedSchema,
    /// Native file validation requires a closed, stable, checkpointed database.
    UnsafeSource,
}

impl std::error::Error for SqliteSessionError {}

impl fmt::Display for SqliteSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SqliteSessionError::Poisoned => write!(f, "session lock is poisoned"),
            SqliteSessionError::AddrParse(_) => write!(f, "invalid socket address syntax"),
            SqliteSessionError::Sql(err) => write!(f, "{err}"),
            SqliteSessionError::InvalidAuthKeyLength(actual) => {
                write!(f, "invalid auth_key length: expected 256, got {actual}")
            }
            SqliteSessionError::Io(_) => f.write_str("cannot access native session file"),
            SqliteSessionError::UnsupportedSchema => f.write_str(
                "unsupported native session schema; use the explicit importer for foreign sessions",
            ),
            SqliteSessionError::UnsafeSource => {
                f.write_str("native session file must be closed and stable without active sidecars")
            }
        }
    }
}

impl From<AddrParseError> for SqliteSessionError {
    fn from(x: AddrParseError) -> Self {
        Self::AddrParse(x)
    }
}

impl From<libsql::Error> for SqliteSessionError {
    fn from(x: libsql::Error) -> Self {
        Self::Sql(x)
    }
}

impl From<io::Error> for SqliteSessionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

#[repr(u8)]
enum PeerSubtype {
    UserSelf = 1,
    UserBot = 2,
    UserSelfBot = 3,
    Megagroup = 4,
    Broadcast = 8,
    Gigagroup = 12,
    Community = 16,
}

impl Database {
    fn peer_from_row(peer: PeerId, row: libsql::Row) -> libsql::Result<PeerInfo> {
        let subtype = row.get::<Option<i64>>(2)?.map(|s| s as u8);
        Ok(match peer.kind() {
            PeerKind::User => PeerInfo::User {
                id: PeerId::user_unchecked(row.get::<i64>(0)?).bare_id_unchecked(),
                auth: row.get::<Option<i64>>(1)?.map(PeerAuth::from_hash),
                bot: subtype.map(|s| s & PeerSubtype::UserBot as u8 != 0),
                is_self: subtype.map(|s| s & PeerSubtype::UserSelf as u8 != 0),
            },
            PeerKind::Chat => PeerInfo::Chat {
                id: peer.bare_id_unchecked(),
            },
            PeerKind::Channel => PeerInfo::Channel {
                id: peer.bare_id_unchecked(),
                auth: row.get::<Option<i64>>(1)?.map(PeerAuth::from_hash),
                kind: subtype.and_then(|s| {
                    if (s & PeerSubtype::Gigagroup as u8) == PeerSubtype::Gigagroup as u8 {
                        Some(ChannelKind::Gigagroup)
                    } else if s & PeerSubtype::Broadcast as u8 != 0 {
                        Some(ChannelKind::Broadcast)
                    } else if s & PeerSubtype::Megagroup as u8 != 0 {
                        Some(ChannelKind::Megagroup)
                    } else {
                        None
                    }
                }),
            },
        })
    }

    async fn init(&self) -> libsql::Result<()> {
        let mut user_version: i64 = self
            .fetch_one("PRAGMA user_version", params![], |row| row.get(0))
            .await?
            .unwrap_or(0);
        if user_version == VERSION {
            return Ok(());
        }

        if user_version == 0 {
            self.migrate_v0_to_v1().await?;
            user_version += 1;
        }
        if user_version == VERSION {
            // Can't bind PRAGMA parameters, but `VERSION` is not user-controlled input.
            self.0
                .execute(&format!("PRAGMA user_version = {VERSION}"), params![])
                .await?;
        }
        Ok(())
    }

    async fn migrate_v0_to_v1(&self) -> libsql::Result<()> {
        let transaction = self.begin_transaction().await?;
        transaction
            .execute(
                "CREATE TABLE dc_home (
                dc_id INTEGER NOT NULL,
                PRIMARY KEY(dc_id))",
                params![],
            )
            .await?;
        transaction
            .execute(
                "CREATE TABLE dc_option (
                dc_id INTEGER NOT NULL,
                ipv4 TEXT NOT NULL,
                ipv6 TEXT NOT NULL,
                auth_key BLOB,
                PRIMARY KEY (dc_id))",
                params![],
            )
            .await?;
        transaction
            .execute(
                "CREATE TABLE peer_info (
                peer_id INTEGER NOT NULL,
                hash INTEGER,
                subtype INTEGER,
                PRIMARY KEY (peer_id))",
                params![],
            )
            .await?;
        transaction
            .execute(
                "CREATE TABLE update_state (
                pts INTEGER NOT NULL,
                qts INTEGER NOT NULL,
                date INTEGER NOT NULL,
                seq INTEGER NOT NULL)",
                params![],
            )
            .await?;
        transaction
            .execute(
                "CREATE TABLE channel_state (
                peer_id INTEGER NOT NULL,
                pts INTEGER NOT NULL,
                PRIMARY KEY (peer_id))",
                params![],
            )
            .await?;

        transaction.commit().await?;
        Ok(())
    }

    async fn begin_transaction(&self) -> libsql::Result<libsql::Transaction> {
        self.0.transaction().await
    }

    async fn fetch_one<
        T,
        P: libsql::params::IntoParams,
        F: FnOnce(libsql::Row) -> libsql::Result<T>,
    >(
        &self,
        statement: &str,
        params: P,
        select: F,
    ) -> libsql::Result<Option<T>> {
        let mut statement = self.0.prepare(statement).await?;
        let result = statement.query_row(params).await;
        match result {
            Ok(value) => Ok(Some(select(value)?)),
            Err(libsql::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn fetch_all<
        T,
        P: libsql::params::IntoParams,
        F: FnMut(libsql::Row) -> Result<T, SqliteSessionError>,
    >(
        &self,
        statement: &str,
        params: P,
        mut select: F,
    ) -> Result<Vec<T>, SqliteSessionError> {
        let statement = self.0.prepare(statement).await?;
        let mut rows = statement.query(params).await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            result.push(select(row)?);
        }
        Ok(result)
    }
}

impl SqliteSession {
    /// Opens only an empty database or a supported native grammers session file.
    ///
    /// Creates a missing file. Existing files are inspected using an immutable,
    /// read-only connection before opening them for writes. Foreign schemas,
    /// unsupported versions, views, triggers and active journals are rejected.
    /// Use the explicitly named foreign-session importers for other libraries.
    ///
    /// The file must stay closed and stable during this call; concurrent file
    /// replacement or modification is unsupported. This guard does not recover
    /// interrupted journals. The legacy [`Self::open`] remains available when
    /// application-controlled storage requires ordinary SQLite recovery.
    ///
    /// # Errors
    /// Returns a sanitized schema/source/I/O error if preflight fails, or an
    /// ordinary storage error if opening or reading the validated session fails.
    pub async fn open_native(path: impl AsRef<Path>) -> Result<Self, SqliteSessionError> {
        let path = native_path(path.as_ref())?;
        let before = native_source(&path)?;
        let flags =
            libsql::OpenFlags::SQLITE_OPEN_READ_ONLY | libsql::OpenFlags::from_bits_retain(0x40); // SQLITE_OPEN_URI
        let database = libsql::Builder::new_local(native_immutable_uri(&path)?)
            .flags(flags)
            .build()
            .await
            .map_err(|_| SqliteSessionError::UnsupportedSchema)?;
        let connection = database
            .connect()
            .map_err(|_| SqliteSessionError::UnsupportedSchema)?;
        validate_native_schema(&connection)
            .await
            .map_err(|_| SqliteSessionError::UnsupportedSchema)?;
        drop(connection);
        drop(database);
        if before != native_source(&path)? {
            return Err(SqliteSessionError::UnsafeSource);
        }
        Self::open(path).await
    }

    /// Open a connection to the SQLite database at `path`,
    /// creating one if it doesn't exist.
    pub async fn open<P: AsRef<Path>>(path: P) -> Result<Self, SqliteSessionError> {
        let conn = libsql::Builder::new_local(path).build().await?.connect()?;
        let db = Database(conn);
        db.init().await?;

        let home_dc = db
            .fetch_one("SELECT * FROM dc_home LIMIT 1", named_params![], |row| {
                row.get::<i32>(0)
            })
            .await?
            .unwrap_or(DEFAULT_DC);

        let dc_options = db
            .fetch_all("SELECT * FROM dc_option", named_params![], |row| {
                Ok(DcOption {
                    id: row.get::<i32>(0)?,
                    ipv4: row.get::<String>(1)?.parse()?,
                    ipv6: row.get::<String>(2)?.parse()?,
                    auth_key: match row.get::<Option<Vec<u8>>>(3)? {
                        None => None,
                        Some(auth_key) => Some(auth_key.try_into().map_err(|v: Vec<u8>| {
                            SqliteSessionError::InvalidAuthKeyLength(v.len())
                        })?),
                    },
                })
            })
            .await?
            .into_iter()
            .map(|dc_option| (dc_option.id, dc_option))
            .collect();

        Ok(SqliteSession {
            database: AsyncMutex::new(db),
            cache: Mutex::new(Cache {
                home_dc,
                dc_options,
            }),
        })
    }
}

fn native_path(path: &Path) -> Result<PathBuf, SqliteSessionError> {
    // create_new never truncates or overwrites a file that appeared meanwhile.
    if let Err(error) = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(error.into());
    }
    Ok(fs::canonicalize(path)?)
}

fn native_source(path: &Path) -> Result<(u64, Option<std::time::SystemTime>), SqliteSessionError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(SqliteSessionError::Io(io::ErrorKind::InvalidInput));
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        match fs::metadata(sidecar) {
            Ok(metadata) if metadata.len() != 0 || !metadata.is_file() => {
                return Err(SqliteSessionError::UnsafeSource);
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok((metadata.len(), metadata.modified().ok()))
}

fn native_immutable_uri(path: &Path) -> Result<String, SqliteSessionError> {
    let path = path
        .to_str()
        .ok_or(SqliteSessionError::Io(io::ErrorKind::InvalidInput))?;
    #[cfg(windows)]
    let path = path
        .strip_prefix(r"\\?\")
        .unwrap_or(path)
        .replace('\\', "/");
    if path.starts_with("//") || path.starts_with("UNC/") {
        return Err(SqliteSessionError::Io(io::ErrorKind::InvalidInput));
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

async fn validate_native_schema(connection: &libsql::Connection) -> libsql::Result<()> {
    let invalid = || libsql::Error::InvalidColumnType;
    let mut version = connection.query("PRAGMA user_version", ()).await?;
    let version = version.next().await?.ok_or_else(invalid)?.get::<i64>(0)?;
    let mut objects = connection
        .query(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
            (),
        )
        .await?;
    let count = objects.next().await?.ok_or_else(invalid)?.get::<i64>(0)?;
    if version == 0 && count == 0 {
        return Ok(());
    }
    if version != VERSION || count != 5 {
        return Err(invalid());
    }
    let mut tables = connection.query("SELECT count(*) FROM pragma_table_list WHERE schema='main' AND type='table' AND name IN ('dc_home','dc_option','peer_info','update_state','channel_state')", ()).await?;
    if tables.next().await?.ok_or_else(invalid)?.get::<i64>(0)? != 5 {
        return Err(invalid());
    }
    // Comparing bounded metadata avoids materializing arbitrary schema text.
    type Column = (&'static str, &'static str, i64, i64);
    const TABLES: &[(&str, &[Column])] = &[
        ("dc_home", &[("dc_id", "INTEGER", 1, 1)]),
        (
            "dc_option",
            &[
                ("dc_id", "INTEGER", 1, 1),
                ("ipv4", "TEXT", 1, 0),
                ("ipv6", "TEXT", 1, 0),
                ("auth_key", "BLOB", 0, 0),
            ],
        ),
        (
            "peer_info",
            &[
                ("peer_id", "INTEGER", 1, 1),
                ("hash", "INTEGER", 0, 0),
                ("subtype", "INTEGER", 0, 0),
            ],
        ),
        (
            "update_state",
            &[
                ("pts", "INTEGER", 1, 0),
                ("qts", "INTEGER", 1, 0),
                ("date", "INTEGER", 1, 0),
                ("seq", "INTEGER", 1, 0),
            ],
        ),
        (
            "channel_state",
            &[("peer_id", "INTEGER", 1, 1), ("pts", "INTEGER", 1, 0)],
        ),
    ];
    for (table, columns) in TABLES {
        let mut rows = connection.query("SELECT CASE WHEN octet_length(name)<32 THEN name END, CASE WHEN octet_length(type)<16 THEN type END, \"notnull\", pk, hidden, dflt_value IS NULL FROM pragma_table_xinfo(?) ORDER BY cid LIMIT 6", params![*table]).await?;
        for (name, kind, not_null, primary) in *columns {
            let row = rows.next().await?.ok_or_else(invalid)?;
            if row.get::<String>(0)? != *name
                || row.get::<String>(1)? != *kind
                || row.get::<i64>(2)? != *not_null
                || row.get::<i64>(3)? != *primary
                || row.get::<i64>(4)? != 0
                || row.get::<i64>(5)? != 1
            {
                return Err(invalid());
            }
        }
        if rows.next().await?.is_some() {
            return Err(invalid());
        }
    }
    Ok(())
}

impl Session for SqliteSession {
    type Error = SqliteSessionError;

    fn home_dc_id(&self) -> Result<i32, SqliteSessionError> {
        Ok(self
            .cache
            .lock()
            .map_err(|_| SqliteSessionError::Poisoned)?
            .home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), SqliteSessionError>> {
        let ok = match self.cache.lock() {
            Err(_) => Err(SqliteSessionError::Poisoned),
            Ok(mut x) => {
                x.home_dc = dc_id;
                Ok(())
            }
        };
        Box::pin(async move {
            ok?;

            let transaction = self.database.lock().await.begin_transaction().await?;
            transaction
                .execute("DELETE FROM dc_home", params![])
                .await?;
            let stmt = transaction
                .prepare("INSERT INTO dc_home VALUES (:dc_id)")
                .await?;
            stmt.execute(named_params! {":dc_id": dc_id}).await?;
            transaction.commit().await?;
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, SqliteSessionError> {
        Ok(self
            .cache
            .lock()
            .map_err(|_| SqliteSessionError::Poisoned)?
            .dc_options
            .get(&dc_id)
            .cloned()
            .or_else(|| {
                KNOWN_DC_OPTIONS
                    .iter()
                    .find(|dc_option| dc_option.id == dc_id)
                    .cloned()
            }))
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), SqliteSessionError>> {
        let ok = match self.cache.lock() {
            Err(_) => Err(SqliteSessionError::Poisoned),
            Ok(mut x) => {
                x.dc_options.insert(dc_option.id, dc_option.clone());
                Ok(())
            }
        };

        let dc_option = dc_option.clone();
        Box::pin(async move {
            ok?;

            let db = self.database.lock().await;
            db.0.execute(
                "INSERT OR REPLACE INTO dc_option VALUES (:dc_id, :ipv4, :ipv6, :auth_key)",
                named_params! {
                    ":dc_id": dc_option.id,
                    ":ipv4": dc_option.ipv4.to_string(),
                    ":ipv6": dc_option.ipv6.to_string(),
                    ":auth_key": dc_option.auth_key.map(|k| k.to_vec()),
                },
            )
            .await?;
            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, SqliteSessionError>> {
        Box::pin(async move {
            let db = self.database.lock().await;
            let map_row = |row| Database::peer_from_row(peer, row);

            Ok(if let Some(peer_id) = peer.bot_api_dialog_id() {
                db.fetch_one(
                    "SELECT * FROM peer_info WHERE peer_id = :peer_id LIMIT 1",
                    named_params! {":peer_id": peer_id},
                    map_row,
                )
                .await?
            } else {
                db.fetch_one(
                    "SELECT * FROM peer_info WHERE subtype & :type LIMIT 1",
                    named_params! {":type": PeerSubtype::UserSelf as i64},
                    map_row,
                )
                .await?
            })
        })
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), SqliteSessionError>> {
        Box::pin(async move {
            let db = self.database.lock().await;
            let existing = db
                .fetch_one(
                    "SELECT * FROM peer_info WHERE peer_id = :peer_id LIMIT 1",
                    named_params! {":peer_id": peer.id().bot_api_dialog_id_unchecked()},
                    |row| Database::peer_from_row(peer.id(), row),
                )
                .await?;
            let peer = if let Some(mut existing_peer) = existing {
                existing_peer.extend_info(&peer);
                existing_peer
            } else {
                peer
            };

            let stmt =
                db.0.prepare("INSERT OR REPLACE INTO peer_info VALUES (:peer_id, :hash, :subtype)")
                    .await?;
            let subtype = match peer {
                PeerInfo::User { bot, is_self, .. } => {
                    match (bot.unwrap_or_default(), is_self.unwrap_or_default()) {
                        (true, true) => Some(PeerSubtype::UserSelfBot),
                        (true, false) => Some(PeerSubtype::UserBot),
                        (false, true) => Some(PeerSubtype::UserSelf),
                        (false, false) => None,
                    }
                }
                PeerInfo::Chat { .. } => None,
                PeerInfo::Channel { kind, .. } => kind.map(|kind| match kind {
                    ChannelKind::Megagroup => PeerSubtype::Megagroup,
                    ChannelKind::Broadcast => PeerSubtype::Broadcast,
                    ChannelKind::Gigagroup => PeerSubtype::Gigagroup,
                    ChannelKind::Community => PeerSubtype::Community,
                }),
            };
            let mut params = vec![];
            let peer_id = peer.id().bot_api_dialog_id_unchecked();
            params.push((":peer_id".to_owned(), peer_id));
            let hash = peer.auth().unwrap_or_default().hash();
            if peer.auth().is_some() {
                params.push((":hash".to_owned(), hash));
            }
            if let Some(subtype) = subtype.map(|s| s as i64) {
                params.push((":subtype".to_owned(), subtype));
            }
            stmt.execute(params).await?;
            Ok(())
        })
    }

    fn cache_peers(&self, peers: Vec<PeerInfo>) -> BoxFuture<'_, Result<(), SqliteSessionError>> {
        Box::pin(async move {
            if peers.is_empty() {
                return Ok(());
            }
            let mut result = Ok(());
            let db = self.database.lock().await;
            let tx = db.0.transaction().await?;
            let mut select = tx
                .prepare("SELECT * FROM peer_info WHERE peer_id = :peer_id LIMIT 1")
                .await?;
            let stmt = tx
                .prepare("INSERT OR REPLACE INTO peer_info VALUES (:peer_id, :hash, :subtype)")
                .await?;
            for peer in peers {
                // Read after each prior write so duplicate IDs see only accepted updates.
                // Retain the database lock across reads and writes to avoid lost metadata.
                let existing = select
                    .query_row(named_params! {
                        ":peer_id": peer.id().bot_api_dialog_id_unchecked(),
                    })
                    .await
                    .and_then(|row| Database::peer_from_row(peer.id(), row));
                select.reset();
                let peer = match existing {
                    Ok(mut existing_peer) => {
                        existing_peer.extend_info(&peer);
                        existing_peer
                    }
                    Err(libsql::Error::QueryReturnedNoRows) => peer,
                    Err(e) => {
                        result = Err(e.into());
                        peer
                    }
                };
                let subtype = match peer {
                    PeerInfo::User { bot, is_self, .. } => {
                        match (bot.unwrap_or_default(), is_self.unwrap_or_default()) {
                            (true, true) => Some(PeerSubtype::UserSelfBot),
                            (true, false) => Some(PeerSubtype::UserBot),
                            (false, true) => Some(PeerSubtype::UserSelf),
                            (false, false) => None,
                        }
                    }
                    PeerInfo::Chat { .. } => None,
                    PeerInfo::Channel { kind, .. } => kind.map(|kind| match kind {
                        ChannelKind::Megagroup => PeerSubtype::Megagroup,
                        ChannelKind::Broadcast => PeerSubtype::Broadcast,
                        ChannelKind::Gigagroup => PeerSubtype::Gigagroup,
                        ChannelKind::Community => PeerSubtype::Community,
                    }),
                };
                let mut params = vec![];
                let peer_id = peer.id().bot_api_dialog_id_unchecked();
                params.push((":peer_id".to_owned(), Some(peer_id)));
                params.push((":hash".to_owned(), peer.auth().map(|a| a.hash())));
                params.push((":subtype".to_owned(), subtype.map(|s| s as i64)));
                if let Err(err) = stmt.execute(params).await {
                    result = Err(err.into());
                }
                stmt.reset();
            }
            tx.commit().await?;
            result
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, SqliteSessionError>> {
        Box::pin(async move {
            let db = self.database.lock().await;
            let mut state = db
                .fetch_one(
                    "SELECT * FROM update_state LIMIT 1",
                    named_params![],
                    |row| {
                        Ok(UpdatesState {
                            pts: row.get(0)?,
                            qts: row.get(1)?,
                            date: row.get(2)?,
                            seq: row.get(3)?,
                            channels: Vec::new(),
                        })
                    },
                )
                .await?
                .unwrap_or_default();
            state.channels = db
                .fetch_all("SELECT * FROM channel_state", named_params![], |row| {
                    Ok(ChannelState {
                        id: row.get(0)?,
                        pts: row.get(1)?,
                    })
                })
                .await?;
            Ok(state)
        })
    }

    fn set_update_state(
        &self,
        update: UpdateState,
    ) -> BoxFuture<'_, Result<(), SqliteSessionError>> {
        Box::pin(async move {
            let db = self.database.lock().await;
            let transaction = db.begin_transaction().await?;

            match update {
                UpdateState::All(updates_state) => {
                    transaction
                        .execute("DELETE FROM update_state", params![])
                        .await?;
                    transaction
                        .execute(
                            "INSERT INTO update_state VALUES (:pts, :qts, :date, :seq)",
                            named_params! {
                                ":pts": updates_state.pts,
                                ":qts": updates_state.qts,
                                ":date": updates_state.date,
                                ":seq": updates_state.seq,
                            },
                        )
                        .await?;

                    transaction
                        .execute("DELETE FROM channel_state", params![])
                        .await?;
                    for channel in updates_state.channels {
                        transaction
                            .execute(
                                "INSERT INTO channel_state VALUES (:peer_id, :pts)",
                                named_params! {
                                    ":peer_id": channel.id,
                                    ":pts": channel.pts,
                                },
                            )
                            .await?;
                    }
                }
                UpdateState::Primary { pts, date, seq } => {
                    let previous = db
                        .fetch_one(
                            "SELECT * FROM update_state LIMIT 1",
                            named_params![],
                            |_| Ok(()),
                        )
                        .await?;

                    if previous.is_some() {
                        transaction
                            .execute(
                                "UPDATE update_state SET pts = :pts, date = :date, seq = :seq",
                                named_params! {
                                    ":pts": pts,
                                    ":date": date,
                                    ":seq": seq,
                                },
                            )
                            .await?;
                    } else {
                        transaction
                            .execute(
                                "INSERT INTO update_state VALUES (:pts, 0, :date, :seq)",
                                named_params! {
                                    ":pts": pts,
                                    ":date": date,
                                    ":seq": seq,
                                },
                            )
                            .await?;
                    }
                }
                UpdateState::Secondary { qts } => {
                    let previous = db
                        .fetch_one(
                            "SELECT * FROM update_state LIMIT 1",
                            named_params![],
                            |_| Ok(()),
                        )
                        .await?;

                    if previous.is_some() {
                        transaction
                            .execute(
                                "UPDATE update_state SET qts = :qts",
                                named_params! {":qts": qts},
                            )
                            .await?;
                    } else {
                        transaction
                            .execute(
                                "INSERT INTO update_state VALUES (0, :qts, 0, 0)",
                                named_params! {":qts": qts},
                            )
                            .await?;
                    }
                }
                UpdateState::Channel { id, pts } => {
                    transaction
                        .execute(
                            "INSERT OR REPLACE INTO channel_state VALUES (:peer_id, :pts)",
                            named_params! {
                                ":peer_id": id,
                                ":pts": pts,
                            },
                        )
                        .await?;
                }
            }

            transaction.commit().await?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use {DcOption, KNOWN_DC_OPTIONS, PeerInfo, Session, UpdateState};

    use super::*;

    struct NativeFixture(PathBuf);

    impl NativeFixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "grammers-native-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> PathBuf {
            self.0.join("synthetic # % session.sqlite")
        }
    }

    impl Drop for NativeFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn native_open_rejects_foreign_database_without_mutation() {
        for schema in [
            "CREATE TABLE version (number INTEGER PRIMARY KEY); INSERT INTO version VALUES (3); CREATE TABLE sessions (dc_id INTEGER PRIMARY KEY, api_id INTEGER, test_mode INTEGER, auth_key BLOB, date INTEGER, user_id INTEGER, is_bot INTEGER);",
            "CREATE TABLE version (version INTEGER PRIMARY KEY); INSERT INTO version VALUES (8); CREATE TABLE sessions (dc_id INTEGER PRIMARY KEY, server_address TEXT, port INTEGER, auth_key BLOB, takeout_id INTEGER);",
        ] {
            let fixture = NativeFixture::new();
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(async {
                    let database = libsql::Builder::new_local(fixture.path())
                        .build()
                        .await
                        .unwrap();
                    let connection = database.connect().unwrap();
                    connection.execute_batch(schema).await.unwrap();
                    drop(connection);
                    drop(database);
                    let before = std::fs::read(fixture.path()).unwrap();
                    assert!(matches!(
                        SqliteSession::open_native(fixture.path()).await,
                        Err(SqliteSessionError::UnsupportedSchema)
                    ));
                    assert_eq!(std::fs::read(fixture.path()).unwrap(), before);
                    assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 1);
                });
        }
    }

    #[test]
    fn native_open_creates_and_reopens_native_storage() {
        let fixture = NativeFixture::new();
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let session = SqliteSession::open_native(fixture.path()).await.unwrap();
                session.set_home_dc_id(4).await.unwrap();
                session
                    .set_dc_option(&DcOption {
                        auth_key: Some([42; 256]),
                        ..KNOWN_DC_OPTIONS[0].clone()
                    })
                    .await
                    .unwrap();
                drop(session);
                let before = std::fs::read(fixture.path()).unwrap();
                let session = SqliteSession::open_native(fixture.path()).await.unwrap();
                assert_eq!(session.home_dc_id().unwrap(), 4);
                assert_eq!(
                    session
                        .dc_option(KNOWN_DC_OPTIONS[0].id)
                        .unwrap()
                        .unwrap()
                        .auth_key,
                    Some([42; 256])
                );
                drop(session);
                assert_eq!(std::fs::read(fixture.path()).unwrap(), before);
            });
    }

    #[test]
    fn native_open_accepts_existing_empty_database() {
        let fixture = NativeFixture::new();
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let database = libsql::Builder::new_local(fixture.path())
                    .build()
                    .await
                    .unwrap();
                let connection = database.connect().unwrap();
                connection.execute("VACUUM", ()).await.unwrap();
                drop(connection);
                drop(database);
                drop(SqliteSession::open_native(fixture.path()).await.unwrap());
            });
    }

    #[test]
    fn native_open_rejects_wrong_version_schema_and_corruption_unchanged() {
        for mutation in [
            "PRAGMA user_version=2",
            "DROP TABLE peer_info",
            "DROP TABLE peer_info; CREATE VIEW peer_info AS SELECT 1 AS peer_id, 2 AS hash, 3 AS subtype",
            "ALTER TABLE peer_info RENAME COLUMN hash TO wrong_column",
            "CREATE TRIGGER unexpected AFTER INSERT ON peer_info BEGIN DELETE FROM dc_home; END",
        ] {
            let fixture = NativeFixture::new();
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(async {
                    let session = SqliteSession::open_native(fixture.path()).await.unwrap();
                    session
                        .database
                        .lock()
                        .await
                        .0
                        .execute_batch(mutation)
                        .await
                        .unwrap();
                    drop(session);
                    let before = std::fs::read(fixture.path()).unwrap();
                    assert!(matches!(
                        SqliteSession::open_native(fixture.path()).await,
                        Err(SqliteSessionError::UnsupportedSchema)
                    ));
                    assert_eq!(std::fs::read(fixture.path()).unwrap(), before);
                    std::fs::write(fixture.path(), b"synthetic not a SQLite database").unwrap();
                    assert!(matches!(
                        SqliteSession::open_native(fixture.path()).await,
                        Err(SqliteSessionError::UnsupportedSchema)
                    ));
                    assert_eq!(
                        std::fs::read(fixture.path()).unwrap(),
                        b"synthetic not a SQLite database"
                    );
                });
        }
    }

    #[test]
    fn native_open_rejects_sidecars_without_recovery() {
        for suffix in ["-wal", "-shm", "-journal"] {
            let fixture = NativeFixture::new();
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(async {
                    drop(SqliteSession::open_native(fixture.path()).await.unwrap());
                    let mut sidecar = fixture.path().into_os_string();
                    sidecar.push(suffix);
                    std::fs::write(&sidecar, b"synthetic unfinished journal").unwrap();
                    let before = std::fs::read(fixture.path()).unwrap();
                    assert!(matches!(
                        SqliteSession::open_native(fixture.path()).await,
                        Err(SqliteSessionError::UnsafeSource)
                    ));
                    assert_eq!(std::fs::read(fixture.path()).unwrap(), before);
                    assert_eq!(
                        std::fs::read(&sidecar).unwrap(),
                        b"synthetic unfinished journal"
                    );
                });
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_open_preserves_literal_backslash_in_unix_path() {
        let fixture = NativeFixture::new();
        let path = fixture.0.join("literal\\backslash.sqlite");
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let session = SqliteSession::open_native(&path).await.unwrap();
                session.set_home_dc_id(3).await.unwrap();
                drop(session);
                assert_eq!(
                    SqliteSession::open_native(path)
                        .await
                        .unwrap()
                        .home_dc_id()
                        .unwrap(),
                    3
                );
            });
    }

    #[test]
    fn cache_peers_reports_row_error_and_commits_other_peers() {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let session = SqliteSession::open(":memory:").await.unwrap();
            session.database.lock().await.0.execute(
                "CREATE TRIGGER reject_peer BEFORE INSERT ON peer_info WHEN NEW.peer_id = 1 BEGIN SELECT RAISE(FAIL, 'rejected peer'); END", params![]
            ).await.unwrap();
            assert!(matches!(session.cache_peers(vec![PeerInfo::Chat { id: 1 }, PeerInfo::User { id: 1, auth: None, bot: None, is_self: None }, PeerInfo::Chat { id: 2 }]).await, Err(SqliteSessionError::Sql(_))));
            assert_eq!(session.peer(PeerId::chat_unchecked(2)).await.unwrap(), Some(PeerInfo::Chat { id: 2 }));
        });
    }

    #[test]
    fn cache_peers_duplicate_after_row_error_retains_only_committed_metadata() {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let session = SqliteSession::open(":memory:").await.unwrap();
            session.database.lock().await.0.execute(
                "CREATE TRIGGER reject_hash BEFORE INSERT ON peer_info WHEN NEW.hash = 999 BEGIN SELECT RAISE(FAIL, 'rejected hash'); END", params![]
            ).await.unwrap();
            let result = session.cache_peers(vec![
                PeerInfo::Channel { id: 7, auth: Some(PeerAuth::from_hash(200)), kind: Some(ChannelKind::Megagroup) },
                PeerInfo::Channel { id: 7, auth: Some(PeerAuth::from_hash(999)), kind: None },
                PeerInfo::Channel { id: 7, auth: None, kind: Some(ChannelKind::Gigagroup) },
            ]).await;
            assert!(matches!(result, Err(SqliteSessionError::Sql(_))));
            assert_eq!(session.peer(PeerId::channel_unchecked(7)).await.unwrap(), Some(PeerInfo::Channel {
                id: 7, auth: Some(PeerAuth::from_hash(200)), kind: Some(ChannelKind::Gigagroup),
            }));
        });
    }

    #[test]
    #[ignore = "local in-memory SQLite microbenchmark; run explicitly in release mode"]
    fn benchmark_sqlite_cache_peers_transaction() {
        use std::hint::black_box;
        use std::time::Instant;

        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            for size in [0, 1, 8, 100, 1000] {
                let peers: Vec<_> = (1..=size).map(|id| PeerInfo::Channel {
                    id: id as i64,
                    auth: Some(PeerAuth::from_hash(id as i64)),
                    kind: Some(ChannelKind::Broadcast),
                }).collect();
                let iterations = (1000 / size.max(1)).clamp(3, 200);
                let mut samples: [Vec<f64>; 2] = std::array::from_fn(|_| Vec::new());
                for sample in 0..5 {
                    for offset in 0..2 {
                        let variant = (sample + offset) % 2;
                        let session = SqliteSession::open(":memory:").await.unwrap();
                        session.cache_peers(peers.clone()).await.unwrap();
                        // Database creation, schema, fixture cloning, and initial inserts are excluded.
                        let batches = vec![peers.clone(); iterations];
                        let started = Instant::now();
                        for batch in batches {
                            if variant == 0 {
                                for peer in black_box(batch) {
                                    session.cache_peer(peer).await.unwrap();
                                }
                            } else {
                                session.cache_peers(black_box(batch)).await.unwrap();
                            }
                        }
                        samples[variant].push(started.elapsed().as_secs_f64() * 1e6 / iterations as f64);
                    }
                }
                for sample in &mut samples {
                    sample.sort_by(f64::total_cmp);
                }
                eprintln!("peers={size} iterations={iterations} us/batch: SQLite sequential={:.2} transaction={:.2}", samples[0][2], samples[1][2]);
            }
        });
    }

    #[test]
    fn cache_peers_returns_transaction_error_without_panicking() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let session = SqliteSession::open(":memory:").await.unwrap();
                let transaction = session.database.lock().await.0.transaction().await.unwrap();
                assert!(matches!(
                    session.cache_peers(vec![PeerInfo::Chat { id: 1 }]).await,
                    Err(SqliteSessionError::Sql(_))
                ));
                transaction.rollback().await.unwrap();
            });
    }

    #[test]
    fn cache_peers_returns_prepare_error_without_panicking() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let session = SqliteSession::open(":memory:").await.unwrap();
                session
                    .database
                    .lock()
                    .await
                    .0
                    .execute("DROP TABLE peer_info", params![])
                    .await
                    .unwrap();
                assert!(matches!(
                    session.cache_peers(vec![PeerInfo::Chat { id: 1 }]).await,
                    Err(SqliteSessionError::Sql(_))
                ));
            });
    }

    #[test]
    fn cache_peers_empty_input_does_not_require_database_transaction() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let session = SqliteSession::open(":memory:").await.unwrap();
                let transaction = session.database.lock().await.0.transaction().await.unwrap();
                session.cache_peers(Vec::new()).await.unwrap();
                transaction.rollback().await.unwrap();
            });
    }

    #[test]
    fn exercise_sqlite_session() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(do_exercise_sqlite_session());
    }

    async fn do_exercise_sqlite_session() {
        let session = SqliteSession::open(":memory:").await.unwrap();

        assert_eq!(session.home_dc_id().unwrap(), DEFAULT_DC);
        session.set_home_dc_id(DEFAULT_DC + 1).await.unwrap();
        assert_eq!(session.home_dc_id().unwrap(), DEFAULT_DC + 1);

        assert_eq!(
            session.dc_option(KNOWN_DC_OPTIONS[0].id).unwrap(),
            Some(KNOWN_DC_OPTIONS[0].clone())
        );
        let new_dc_option = DcOption {
            id: KNOWN_DC_OPTIONS
                .iter()
                .map(|dc_option| dc_option.id)
                .max()
                .unwrap()
                + 1,
            ipv4: SocketAddrV4::new(Ipv4Addr::from_bits(0), 1),
            ipv6: SocketAddrV6::new(Ipv6Addr::from_bits(0), 1, 0, 0),
            auth_key: Some([1; 256]),
        };
        assert_eq!(session.dc_option(new_dc_option.id).unwrap(), None);
        session.set_dc_option(&new_dc_option).await.unwrap();
        assert_eq!(
            session.dc_option(new_dc_option.id).unwrap(),
            Some(new_dc_option)
        );

        assert_eq!(session.peer(PeerId::self_user()).await.unwrap(), None);
        assert_eq!(session.peer(PeerId::user_unchecked(1)).await.unwrap(), None);
        let peer = PeerInfo::User {
            id: 1,
            auth: None,
            bot: Some(true),
            is_self: Some(true),
        };
        session.cache_peer(peer.clone()).await.unwrap();
        assert_eq!(
            session.peer(PeerId::self_user()).await.unwrap(),
            Some(peer.clone())
        );
        assert_eq!(
            session.peer(PeerId::user_unchecked(1)).await.unwrap(),
            Some(peer)
        );

        assert_eq!(
            session.peer(PeerId::channel_unchecked(1)).await.unwrap(),
            None
        );
        let peer = PeerInfo::Channel {
            id: 1,
            auth: Some(PeerAuth::from_hash(-1)),
            kind: Some(ChannelKind::Broadcast),
        };
        session.cache_peer(peer.clone()).await.unwrap();
        assert_eq!(
            session.peer(PeerId::channel_unchecked(1)).await.unwrap(),
            Some(peer)
        );

        let peers = vec![
            PeerInfo::Channel {
                id: 2,
                auth: Some(PeerAuth::from_hash(-2)),
                kind: Some(ChannelKind::Megagroup),
            },
            PeerInfo::Channel {
                id: 3,
                auth: Some(PeerAuth::from_hash(-3)),
                kind: None,
            },
        ];
        session.cache_peers(peers.clone()).await.unwrap();
        for peer in peers.iter() {
            assert_eq!(session.peer(peer.id()).await.unwrap().as_ref(), Some(peer));
        }

        // If the inserted peer is going to be updated with some fields changed
        // from `Some` to `None`, the old `Some` is expected to be kept.
        let new_inserted_peers = vec![
            PeerInfo::Channel {
                id: 2,
                auth: None,
                kind: Some(ChannelKind::Broadcast),
            },
            PeerInfo::Channel {
                id: 3,
                auth: Some(PeerAuth::from_hash(-30)),
                kind: None,
            },
        ];
        session.cache_peers(new_inserted_peers).await.unwrap();
        assert_eq!(
            session.peer(PeerId::channel_unchecked(2)).await.unwrap(),
            Some(PeerInfo::Channel {
                id: 2,
                auth: Some(PeerAuth::from_hash(-2)),
                kind: Some(ChannelKind::Broadcast),
            })
        );
        assert_eq!(
            session.peer(PeerId::channel_unchecked(3)).await.unwrap(),
            Some(PeerInfo::Channel {
                id: 3,
                auth: Some(PeerAuth::from_hash(-30)),
                kind: None
            })
        );

        assert_eq!(
            session.updates_state().await.unwrap(),
            UpdatesState::default()
        );
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 1,
                qts: 2,
                date: 3,
                seq: 4,
                channels: vec![
                    ChannelState { id: 5, pts: 6 },
                    ChannelState { id: 7, pts: 8 },
                ],
            }))
            .await
            .unwrap();
        session
            .set_update_state(UpdateState::Primary {
                pts: 2,
                date: 4,
                seq: 5,
            })
            .await
            .unwrap();
        session
            .set_update_state(UpdateState::Secondary { qts: 3 })
            .await
            .unwrap();
        session
            .set_update_state(UpdateState::Channel { id: 7, pts: 9 })
            .await
            .unwrap();
        assert_eq!(
            session.updates_state().await.unwrap(),
            UpdatesState {
                pts: 2,
                qts: 3,
                date: 4,
                seq: 5,
                channels: vec![
                    ChannelState { id: 5, pts: 6 },
                    ChannelState { id: 7, pts: 9 },
                ],
            }
        );
    }
}
