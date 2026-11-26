# Streams experiment, 2026-11-25

Undo weak coding inside files (deflate, PNG, JPEG, Outlook mail scrambling and block order), keep the results in separate
streams, then compress those. Rebuilding from the streams gave every one of 17,959 files back byte for byte (BLAKE3).

Sizes in MB (seconds) for real files from an old USB disk, six CPU threads, other work running. 7-Zip 23.01 `-mx9
-md512m -mfb273`, RAR 7.00 `-m5 -md1g -s`, zstd 1.5.5 `--ultra -22 --long=31`, zpaq 7.15 `-m5`.

| Set | original | 7z | rar | zstd22 | zpaq5 | ours+zstd3 | ours+7z | ours+zpaq5 |
|---|---|---|---|---|---|---|---|---|
| jpeg | 250.0 | 235.5 (68 s) | 236.1 (23 s) | 235.5 (179 s) | 227.5 (515 s) | 179.8 (0 s) | 179.8 (28 s) | 179.8 (545 s) |
| png | 150.0 | 139.3 (19 s) | 139.8 (16 s) | 139.3 (96 s) | 136.9 (512 s) | 137.5 (5 s) | 106.2 (562 s) | 88.1 (743 s) |
| text | 150.0 | 10.1 (85 s) | 11.6 (15 s) | 10.7 (90 s) | 7.9 (236 s) | 14.2 (0 s) | 9.9 (48 s) | 7.6 (228 s) |
| binary | 400.0 | 194.1 (77 s) | 198.7 (35 s) | 199.4 (216 s) | 190.1 (593 s) | 210.6 (1 s) | 190.8 (89 s) | 187.5 (564 s) |
| zipped | 400.0 | 231.2 (124 s) | 233.5 (34 s) | 232.1 (197 s) | 228.1 (666 s) | 184.1 (5 s) | 139.6 (602 s) | 131.0 (1389 s) |
| mail | 1674.9 | 442.5 (518 s) | 402.2 (136 s) | 397.3 (892 s) | 498.2 (2223 s) | 356.8 (34 s) | 296.1 (875 s) | 289.0 (2311 s) |
| total | 3024.9 | 1252.8 | 1222.0 | 1214.3 | 1288.7 | 1083.1 | 922.4 | 883.0 |

- Times for "ours" leave out the transform (one run each, 16 threads, read while the disk was busy): jpeg 15 s, png 40 s,
  text 52 s, binary 39 s, zipped 63 s, mail 29 s.
- Not compared: paq8px, precomp, WinZip Zipx, other recompressing tools. Only the stream rebuild was verified; the back
  ends are stock tools and their output was not extracted again.
- Audio: no gain (99 MB of MP3 stays 99 MB). Video: see below.
- Code: [experiments/streams](../experiments/streams) (whole-file in memory, not an archiver).

## MPEG-4 Part 2 video (DivX/Xvid AVI), estimate, 2026-11-26

An estimator parses every picture of the video stream and codes the same symbols (macroblock types, motion vectors,
DC, coefficients) with adaptive context models instead of the fixed codes. Six 4-minute extracts from different films:

| Clip | video KB | estimate KB | change |
|---|---|---|---|
| 0 | 22,859 | 19,406 | -15.1% |
| 1 | 52,833 | 44,410 | -15.9% |
| 2 | 29,675 | 25,951 | -12.6% |
| 3 | 23,853 | 19,503 | -18.2% |
| 4 | 22,068 | 18,529 | -16.0% |
| 5 | 31,188 | 26,586 | -14.8% |

- About 186 million coefficient symbols; none was written in a non-standard way, so an exact rebuild looks possible.
- An estimate only: no encoder or decoder was written, and nothing was rebuilt. About 0.6 MB/s per thread.
- H.264/H.265 (most MP4/MKV) was not tried; it already uses arithmetic or adaptive coding, and gains are smaller.
