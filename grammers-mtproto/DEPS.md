# Dependencies

## crc32fast

Needed by the full transport mode.

## getrandom

Needed to generate secure values, such as nonces, during the generation of an Authorization Key.

## grammers-crypto

Mainly used to encrypt and decrypt messages exchanged with Telegram's servers, but also contains
other miscellaneous functions such as integer factorization.

## grammers-tl-types

Used to serialize and deserialize the messages exchanged with Telegram's servers.

## flate2

Messages may be gzip-encoded to reduce bandwidth, so this crate is used for both decompressing
incoming messages and compressing them when it is worth it (based on some simple heuristics).

## libc

On Linux and Android, to read the kernel's coarse clocks (`CLOCK_MONOTONIC_COARSE` and
`CLOCK_REALTIME_COARSE`), which the standard library has no way to ask for. It is already
part of every build through the runtime and the random number generator.

## num-bigint

Used during the generation of the Authorization Key.

## sha1

Used during the generation of the Authorization Key.

## toml

Used to test that this file lists all dependencies from `Cargo.toml`.

## tracing

Used to help debug what's going on at the MTP level (such as when future salts are asked for).
