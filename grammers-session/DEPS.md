# Dependencies

## grammers-tl-types

Used for dealing with correct update processing.

The serialization and deserialization traits are also used for storing and loading the session
into and from bytes.

## tracing

Used to log messages during update processing.

## toml

Used to test that this file lists all dependencies from `Cargo.toml`.

## libsql

_Optional._ SQLite-based session storage (`sqlite-storage`) and read-only foreign
session database imports (`session-import-files`). The import feature does not
enable this crate's destination SQLite storage.

## serde

_Optional._ Enables serialization and deserialization of configuration and session-related types

## serde_with

_Optional._ Provides custom serialization helpers

## tokio

Asynchronous locks and runtime for testing asynchronous `Session` implementations.
With `session-import-files`, the runtime's blocking pool keeps file reads, SQLite
queries and JSON parsing off the async reactor. No second SQLite library is added.

## base64

_Optional._ Decodes exported Pyrogram, Telethon, and gotd session credentials
when `session-import` is enabled. Encodes and decodes native credentials when
`session-string` is enabled.

## serde_json

_Optional._ Parses versioned gotd session documents when `session-import` is enabled.

## sha1

_Optional._ Validates gotd's authentication-key fingerprint when `session-import` is enabled.

## sha2

_Optional._ Computes the native session string corruption checksum when
`session-string` is enabled. This is not encryption or authentication.
