# zoflib_archive format, version 1

All integers are little-endian. Sizes are in bytes. `postcard` means the
[postcard](https://docs.rs/postcard) encoding of the Rust structs in `src/lib.rs`.

## Layout

```
header (64)
segment: KeyCheck
segment: Data | Index ...       data groups, with an Index segment after every checkpoint
segment: Footer
tail (16): footer offset u64, "ZOFAEND1"
```

An archive without the footer and tail is incomplete (interrupted or cut short). Everything an Index segment
names can still be read, and `pack --resume` continues after the last Index segment.

`add` grows an archive without changing anything an index points at: it cuts only what follows the last Index
segment (the old footer and tail, or data no index names yet), writes the new and changed files with their own Index
segments, then a new footer and tail. Records are never removed.

## Header

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `ZOFARCH1` |
| 8 | 2 | format version, 1 |
| 10 | 2 | flags; bit 0 = no passphrase. Other bits set: refuse to open |
| 12 | 4 | Argon2id memory in KiB (8192 to 1048576; written as 131072) |
| 16 | 4 | Argon2id passes (1 to 16; written as 3) |
| 20 | 4 | Argon2id lanes (1 to 8; written as 1) |
| 24 | 16 | random salt |
| 40 | 24 | zero |

## Key

- With a passphrase: key = Argon2id v0x13 (passphrase as UTF-8, salt, the settings above), 32 bytes.
- Without one (flag bit 0): key = BLAKE3 `derive_key("zoflib_archive v1 archive without passphrase", salt)`.
  Anyone can read such an archive; the key only lets damage be found the same way.

Cipher: XChaCha20-Poly1305 with that key.

## Segment

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | `ZSG1` |
| 4 | 1 | kind: 0 KeyCheck, 1 Data, 2 Index, 3 Footer |
| 5 | 8 | id |
| 13 | 4 | ciphertext length (16 to 2^30 - 1, tag included) |
| 17 | 24 | random nonce |
| 41 | n | ciphertext |

Associated data: BLAKE3 hash of the 64 header bytes (32), then kind (1), then id (8). A segment that fails
authentication is never used. A reader that meets a damaged frame, or a Data segment whose end is not the start of
another segment, searches forward for the next `ZSG1`.

- **KeyCheck** (id 0): plaintext `zoflib`. A failure here means a wrong passphrase or a damaged start.
- **Data** (id = group id): one group, below.
- **Index** (id = 0, 1, 2 ... in order): postcard `Index`. It covers only groups written before it. The
  archive's index is the union of all Index segments; for a path that appears more than once, the last record wins.
- **Footer** (id 0): postcard `Footer`, the offsets of every Index segment in order, the file count and total size.

Nonces are random, so ids may repeat after a resume. The authentication tag, not the id, decides whether a segment is good.

## Index records

- `GroupRec { id, at, len }`: a Data segment's offset and full length.
- `UnitRec { id, hash, len, group, slot }`: a unit is a piece of file data (at most 64 MiB). `hash` is BLAKE3 of the
  unit, `slot` its place in the group. Units with the same hash and length are stored once.
- `FileRec { path, size, mtime, hash, form, units, added }`: `path` uses `/`, relative, never `..`, absolute or with a
  drive or stream name. `hash` is BLAKE3 of the whole file and is checked on every restore. `added` is the start of
  the run that stored the record, seconds since 1970. Earlier records of the same path are older versions and stay
  readable.
  - `Plain`: the file is its units joined in order.
  - `Mail`: an Outlook PST/OST file. The units are the data blocks in logical order, then a rest unit, then a layout
    unit. The layout rebuilds the file from them byte for byte (see `src/mail.rs`).
- `dirs`: folders, including empty ones.

## Group

A group holds units of one kind of data (text, programs, mail, packed files, media, other). It can be decoded
without any other group.

```
unit count u32
per unit: 6 stream lengths u32
block count u32
per block: stream u8, codec u8, raw length u32, packed length u32
packed blocks, in the order above
```

Each unit is first split into six streams: 0 operations, 1 main bytes, 2 plain bytes, 3 corrections, 4 recoded JPEG,
5 PNG pixels. Deflate data (zip, gzip, zlib, PNG) is replaced by its decoded bytes plus what preflate needs to redo the
exact deflate; JPEG is replaced by its Lepton coding. The operation stream records where each one was. It is decoded by
`unexpand` in `src/streams.rs`; that code is the definition of the operations for version 1.

Each stream of the group (all units joined) is cut into blocks and each block is compressed:
0 stored, 1 zstd (window up to 2^27), 2 LZMA2 in an xz stream, 3 x86 BCJ then LZMA2 in an xz stream.
Blocks that would grow are stored.

## Compatibility

Rebuilding recoded data needs the same coding as when it was packed: preflate-rs 0.7 and lepton_jpeg 0.5. A new major
version of either becomes a new format version. Unknown versions or flags are refused, never guessed.
