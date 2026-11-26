# On a USB hard disk, 2026-11-26

Target: 1 TB 2.5-inch USB hard disk, NTFS, empty. Raw sequential speed, uncached, 8.6 GB: 90 MB/s write, 90 MB/s read.
Sources on a SATA SSD, 16 threads. `zofa` with a passphrase (Argon2id + XChaCha20-Poly1305). zstd 3 = python-zstandard
0.23, all threads, over a tar stream. Copy = `cp -r`. Seconds, sizes in MB.

| Set | in | copy | zstd 3 | zofa fast | zofa balanced |
|---|---|---|---|---|---|
| Android emulator images (83 files) | 14,735 | 14,735 in 166 s | 4,443 in 81 s | 4,232 in 110 s | 4,151 in 143 s |
| GPU libraries (4 DLLs) | 2,601 | 2,601 in 29 s | 998 in 17 s | 788 in 17 s | 673 in 211 s |

- Verify (rebuild every file and check its BLAKE3) took 53 s / 51 s and 5 s / 18 s. The archives had just been written,
  so reads probably came from the file cache: these are CPU times, not disk times.
- One-file extract, archive and output on the same disk: 9,431 MB in 165 s (emulator images), 1,794 MB in 30 s / 41 s
  (largest DLL). Byte-for-byte equal to the source.
- The emulator images hold 6,174 MB of repeated 64 MiB pieces, stored once.
- Packing the images is CPU-bound and slower than zstd 3 for about 5% less space; the stream search over large disk
  images costs time and finds little there. The DLLs get 21% (fast) and 33% (balanced) less than zstd 3.
- Not yet: 7-Zip, restic, Kopia on this disk; an SSD and NVMe as targets; uncached restore timings.
