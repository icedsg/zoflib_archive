# zoflib_archive

A library for encrypted, compressed archives that adapt to the disk they are written to: old USB hard disks, SD cards,
SATA SSDs and NVMe. Status: design and benchmarks only; no archiver yet. Do not store data with it.

## Goal
Store more on any disk and read it back sooner, with one passphrase. On a slow disk, spare CPU time is spent writing
fewer bytes and reads stay sequential. On a fast disk the CPU is the limit, so compression gets light or is skipped.

One Rust core with a C interface, then thin wrappers for Swift, C# and Kotlin.

## Rules
- Standard cryptography only: Argon2id for the passphrase, AES-256-GCM (or XChaCha20-Poly1305) per segment. No own cipher.
- Compress first, then encrypt, in one streaming pass. Wrong passphrase or changed data must fail, never return bad data.
- Start from zstd. An own codec ships only if it passes the gate below.
- The format is documented and versioned, with a small standalone extractor.

## Where a purpose-built format may win
1. Small files packed into large sequential segments (a spinning disk does about 53 random reads a second).
2. Compression level chosen from measured disk speed: the highest level that still keeps the disk busy.
3. Files grouped by type so similar data compresses together; incompressible files stored as is.
4. An index that lists files without reading the archive; restores read in disk order.
5. Resume after unplugging, checksums and optional parity against bad sectors on old disks.

## Gate (decides whether to build it, and whether an own codec is worth it)
Measured end to end on real disks (USB hard disk, SATA SSD, NVMe), including a full verified restore, against zstd + age, 7-Zip, restic and Kopia:
- Archiver: at least 20% faster backup or restore, or 15% smaller, than the best existing tool at equal safety,
  on at least one disk type, and no slower than a plain copy on the others.
- Own codec: at least 10% better total time than zstd inside the same archiver. Otherwise zstd stays.
Below the gate: use the best existing tool and stop.

## Benchmarks
`python bench/bench.py DIR [DIR ...]` (needs `zstandard` and `cryptography`). In memory, no disk timing.
Results: [bench/RESULTS.md](bench/RESULTS.md), [bench/STREAMS.md](bench/STREAMS.md) (against 7-Zip, RAR, zstd, zpaq).

## Licence
MIT, see [LICENSE](LICENSE).
