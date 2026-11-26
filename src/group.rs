//! A group: many units whose streams are compressed together and can be unpacked without any other group.
use crate::streams::{expand, unexpand, Out, Rd};
use anyhow::{bail, ensure, Context, Result};
use liblzma::stream::{Check, Filters, LzmaOptions, Stream};
use rayon::prelude::*;
use std::io::{Read, Write};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Fast,
    Balanced,
    Max,
}
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Cat {
    Text,
    Program,
    Mail,
    Packed,
    Media,
    Other,
}
pub const CATS: [Cat; 6] = [Cat::Text, Cat::Program, Cat::Mail, Cat::Packed, Cat::Media, Cat::Other];

pub fn category(name: &str) -> Cat {
    let e = name.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).unwrap_or_default();
    let has = |l: &str| l.split(' ').any(|x| x == e);
    if has("txt log csv tsv xml json html htm js mjs ts tsx jsx css scss less java py c cc cpp h hpp cs go rs sql md sh bat ps1 ini cfg conf php jsp properties map yml yaml svg xsd xsl srt tex rb pl lua kt swift vb asp aspx") {
        Cat::Text
    } else if has("exe dll so lib obj pdb sys ocx o a dylib scr cpl ax node") {
        Cat::Program
    } else if has("pst ost") {
        Cat::Mail
    } else if has("zip apk jar war ear aar ipa docx xlsx pptx odt ods odp epub pdf gz tgz png jpg jpeg jpe thm swf xpi crx nupkg whl") {
        Cat::Packed
    } else if has("mp4 mkv avi mov wmv flv mpg mpeg m4v 3gp vob webm ts f4v rmvb divx mp3 m4a aac wma ogg opus flac amr heic webp gif rar 7z bz2 xz zst cab") {
        Cat::Media
    } else {
        Cat::Other
    }
}

// stream codecs
const STORE: u8 = 0;
const ZSTD: u8 = 1;
const LZMA: u8 = 2;
const LZMA_X86: u8 = 3;
const LZMA_BLOCK: usize = 32 << 20;
const ZSTD_BLOCK: usize = 128 << 20;

fn choose(stream: usize, cat: Cat, level: Level) -> (u8, i32) {
    use Level::*;
    let strong = (if cat == Cat::Program && stream == 1 { LZMA_X86 } else { LZMA }, 6);
    match (stream, level) {
        (4, _) => (ZSTD, 1),                         // re-coded photos: only repeats are left
        (1, _) if cat == Cat::Media => (ZSTD, 3),    // video and audio barely shrink
        (_, Max) => (strong.0, 9),
        (_, Fast) => (ZSTD, 3),
        (5, Balanced) => strong,                     // image pixels
        (1, Balanced) if matches!(cat, Cat::Text | Cat::Program) => strong,
        (_, Balanced) => (ZSTD, 9),
    }
}

fn filters(x86: bool, preset: u32, dict: usize) -> Result<Filters> {
    let mut o = LzmaOptions::new_preset(preset)?;
    o.dict_size((dict.max(4096).next_power_of_two()).min(LZMA_BLOCK) as u32);
    if preset >= 9 {
        o.nice_len(273);
    }
    let mut f = Filters::new();
    if x86 {
        f.x86();
    }
    f.lzma2(&o);
    Ok(f)
}

fn compress(d: &[u8], codec: u8, lv: i32) -> Result<Vec<u8>> {
    Ok(match codec {
        STORE => d.to_vec(),
        ZSTD => {
            let mut c = zstd::bulk::Compressor::new(lv)?;
            if d.len() > 8 << 20 {
                c.long_distance_matching(true)?;
                c.window_log(27)?;
            }
            c.compress(d)?
        }
        LZMA | LZMA_X86 => {
            let s = Stream::new_stream_encoder(&filters(codec == LZMA_X86, lv as u32, d.len())?, Check::None)?;
            let mut e = liblzma::write::XzEncoder::new_stream(Vec::new(), s);
            e.write_all(d)?;
            e.finish()?
        }
        _ => bail!("codec"),
    })
}
fn decompress(d: &[u8], codec: u8, n: usize) -> Result<Vec<u8>> {
    let mut o = Vec::with_capacity(n);
    match codec {
        STORE => o.extend_from_slice(d),
        ZSTD => {
            let mut r = zstd::stream::read::Decoder::with_buffer(d)?;
            r.window_log_max(27)?;
            r.take(n as u64 + 1).read_to_end(&mut o)?;
        }
        LZMA | LZMA_X86 => {
            let s = Stream::new_stream_decoder(256 << 20, 0)?;
            liblzma::read::XzDecoder::new_stream(d, s).take(n as u64 + 1).read_to_end(&mut o)?;
        }
        c => bail!("unknown codec {c}"),
    }
    ensure!(o.len() == n, "stream length is wrong");
    Ok(o)
}

/// Group bytes: unit count u32; per unit 6 stream lengths u32; block count u32; per block: stream u8, codec u8, raw u32,
/// packed u32; then the packed blocks in order.
pub fn encode(units: &[Vec<u8>], cat: Cat, level: Level) -> Result<Vec<u8>> {
    let outs: Vec<Out> = units
        .par_iter()
        .map(|u| {
            let mut o = Out::default();
            expand(u, &mut o, 0);
            o
        })
        .collect();
    let mut head = (outs.len() as u32).to_le_bytes().to_vec();
    for o in &outs {
        for s in &o.b {
            head.extend(u32::try_from(s.len()).context("unit too large")?.to_le_bytes());
        }
    }
    let mut streams: Vec<Vec<u8>> = vec![];
    for k in 0..6 {
        streams.push(outs.iter().flat_map(|o| o.b[k].iter().copied()).collect());
    }
    drop(outs);
    let mut jobs: Vec<(usize, u8, i32, &[u8])> = vec![];
    for (k, s) in streams.iter().enumerate() {
        let (c, lv) = choose(k, cat, level);
        let size = if c == ZSTD { ZSTD_BLOCK } else { LZMA_BLOCK };
        jobs.extend(s.chunks(size).map(|b| (k, c, lv, b)));
    }
    let packed: Vec<(u8, Vec<u8>)> = jobs
        .par_iter()
        .map(|(_, c, lv, b)| {
            let p = compress(b, *c, *lv)?;
            Ok(if p.len() < b.len() { (*c, p) } else { (STORE, b.to_vec()) })
        })
        .collect::<Result<_>>()?;
    head.extend((jobs.len() as u32).to_le_bytes());
    for (j, (c, p)) in jobs.iter().zip(&packed) {
        head.push(j.0 as u8);
        head.push(*c);
        head.extend((j.3.len() as u32).to_le_bytes());
        head.extend((p.len() as u32).to_le_bytes());
    }
    for (_, p) in packed {
        head.extend(p);
    }
    Ok(head)
}

struct Cur<'a>(&'a [u8]);
impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(n <= self.0.len(), "group cut short");
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<usize> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as usize)
    }
}

/// Rebuilds units; `want` picks slots (None = all). Unwanted slots come back empty.
pub fn decode(g: &[u8], want: Option<&[bool]>) -> Result<Vec<Result<Vec<u8>>>> {
    let mut c = Cur(g);
    let n = c.u32()?;
    let mut lens = vec![];
    for _ in 0..n {
        let mut l = [0usize; 6];
        for x in &mut l {
            *x = c.u32()?;
        }
        lens.push(l);
    }
    let nb = c.u32()?;
    let mut blocks = vec![];
    for _ in 0..nb {
        blocks.push((c.u8()? as usize, c.u8()?, c.u32()?, c.u32()?));
    }
    let mut jobs = vec![];
    for (k, codec, raw, packed) in blocks {
        ensure!(k < 6, "bad stream");
        jobs.push((k, codec, raw, c.take(packed)?));
    }
    let parts: Vec<Vec<u8>> = jobs.par_iter().map(|j| decompress(j.3, j.1, j.2)).collect::<Result<_>>()?;
    let mut streams: Vec<Vec<u8>> = vec![vec![]; 6];
    for (j, p) in jobs.iter().zip(parts) {
        if streams[j.0].is_empty() {
            streams[j.0] = p;
        } else {
            streams[j.0].extend(p);
        }
    }
    let mut at = [0usize; 6];
    let mut spans = vec![];
    for l in &lens {
        spans.push(at);
        for k in 0..6 {
            at[k] += l[k];
            ensure!(at[k] <= streams[k].len(), "unit past the end of its stream");
        }
    }
    Ok(lens
        .par_iter()
        .zip(spans.par_iter())
        .enumerate()
        .map(|(i, (l, s))| {
            if want.map(|w| !w.get(i).copied().unwrap_or(false)).unwrap_or(false) {
                return Ok(vec![]);
            }
            let v: Vec<&[u8]> = (0..6).map(|k| &streams[k][s[k]..s[k] + l[k]]).collect();
            unexpand(&mut Rd::new([v[0], v[1], v[2], v[3], v[4], v[5]]), 0)
        })
        .collect())
}
