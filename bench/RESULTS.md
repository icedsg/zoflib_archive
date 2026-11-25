# First benchmark, 2026-11-25
16-thread Windows PC, in memory, one run while other work was running, up to 256 MB per sample (files up to 8 MB).
"mixed documents" is mostly already-compressed files; the second sample is small (13 MB), so treat it as a hint only.
Disk figures: 46-84 MB/s read, 71 MB/s write, 53 random 4K reads/s, measured on another machine.

| Codec | Project files: ratio, compress MB/s | Archives (13 MB): ratio, compress | Mixed documents: ratio, compress |
|---|---|---|---|
| zstd -5, 1 thread | 2.35, 515 | 1.29, 1099 | 1.09, 1235 |
| zstd 1, 8 threads | 3.09, 1411 | 1.34, 802 | 1.10, 1313 |
| zstd 3, 8 threads | 3.29, 625 | 1.49, 259 | 1.12, 579 |
| zstd 9, 8 threads | 3.51, 152 | 1.75, 31 | 1.13, 128 |
| zstd 16, 8 threads | 3.66, 23 | 1.82, 4 | 1.14, 29 |
| xz 6 (first 32 MB only, not comparable) | 4.74, 3 | 1.97, 2 | 1.15, 2 |

zstd decompression: 318-2,124 MB/s. AES-256-GCM: 788 MB/s on one thread. ChaCha20-Poly1305: 420 MB/s.

To keep a 71 MB/s disk busy, compression must take in 71 x ratio MB/s: about 234 MB/s for project files at ratio 3.3.
zstd 3 on 8 threads passes that (625); zstd 9 does not (152 against 249).
