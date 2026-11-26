//! Undoes weak coding inside files (deflate, PNG, JPEG) into separate streams, and rebuilds the exact bytes.
use anyhow::{bail, ensure, Context, Result};
use lepton_jpeg::*;
use preflate_rs::{preflate_whole_deflate_stream, recreate_whole_deflate_stream, PreflateConfig};
use std::{
    io::Cursor,
    panic::{catch_unwind, AssertUnwindSafe},
};

pub fn pv(o: &mut Vec<u8>, mut v: u64) {
    while v > 127 {
        o.push(v as u8 | 128);
        v >>= 7;
    }
    o.push(v as u8);
}
fn be32(d: &[u8]) -> u32 {
    u32::from_be_bytes(d[..4].try_into().unwrap())
}
fn idat_crc(body: &[u8]) -> [u8; 4] {
    let mut h = crc32fast::Hasher::new();
    h.update(b"IDAT");
    h.update(body);
    h.finalize().to_be_bytes()
}

// streams: 0 ops, 1 main, 2 plain, 3 corr, 4 jpg, 5 png
#[derive(Default)]
pub struct Out {
    pub b: [Vec<u8>; 6],
    pub st: [u64; 5],
}

/// Most expanded bytes one unit may hold before further streams are left as they are.
const BUDGET: usize = 384 << 20;
impl Out {
    fn room(&self) -> bool {
        self.b[2].len() + self.b[5].len() < BUDGET
    }
}

fn feat() -> EnabledFeatures {
    let mut f = EnabledFeatures::compat_lepton_vector_write();
    f.max_partitions = 1;
    f.progressive = true;
    f
}
fn unlepton(v: &[u8]) -> Vec<u8> {
    let mut o = vec![];
    let f = EnabledFeatures::compat_lepton_vector_read();
    let _ = decode_lepton(&mut Cursor::new(v), &mut o, &f, &DEFAULT_THREAD_POOL);
    o
}
fn lepton(d: &[u8]) -> Option<Vec<u8>> {
    catch_unwind(AssertUnwindSafe(|| {
        let mut v = vec![];
        encode_lepton(&mut Cursor::new(d), &mut Cursor::new(&mut v), &feat(), &DEFAULT_THREAD_POOL).ok()?;
        if v.len() < d.len() && unlepton(&v) == d {
            Some(v)
        } else {
            None
        }
    }))
    .ok()
    .flatten()
}
fn jpeg_end(d: &[u8]) -> Option<usize> {
    let mut i = 2;
    loop {
        if *d.get(i)? != 0xff {
            return None;
        }
        let m = *d.get(i + 1)?;
        if m == 0xff {
            i += 1;
            continue;
        }
        if m == 0xd9 {
            return Some(i + 2);
        }
        if m < 0xc0 || m == 0xd8 {
            return None;
        }
        let l = u16::from_be_bytes(d.get(i + 2..i + 4)?.try_into().unwrap()) as usize;
        if l < 2 {
            return None;
        }
        i += 2 + l;
        if m == 0xda {
            loop {
                if *d.get(i)? == 0xff {
                    let n = *d.get(i + 1)?;
                    if n != 0 && !(0xd0..=0xd7).contains(&n) && n != 0xff {
                        break;
                    }
                }
                i += 1;
            }
        }
    }
}
fn deflate_len(d: &[u8]) -> Option<usize> {
    let mut x = flate2::Decompress::new(false);
    let mut b = vec![0u8; 65536];
    loop {
        let (i0, o0) = (x.total_in(), x.total_out());
        match x.decompress(&d[i0 as usize..], &mut b, flate2::FlushDecompress::None) {
            Ok(flate2::Status::StreamEnd) => {
                return if x.total_out() >= 64 { Some(x.total_in() as usize) } else { None };
            }
            Err(_) => return None,
            _ => {
                if (x.total_in() == i0 && x.total_out() == o0) || x.total_out() > 200 << 20 {
                    return None;
                }
            }
        }
    }
}
fn inflate(d: &[u8]) -> Option<(usize, Vec<u8>, Vec<u8>)> {
    let n = deflate_len(d)?;
    if n < 48 {
        return None;
    }
    catch_unwind(AssertUnwindSafe(|| {
        let cfg = PreflateConfig { max_chain_length: 4096, plain_text_limit: 256 << 20, verify_compression: false };
        let (r, p) = preflate_whole_deflate_stream(&d[..n], &cfg).ok()?;
        let p = p.text().to_vec();
        if r.compressed_size != n || r.corrections.len() * 4 > n {
            return None;
        }
        if recreate_whole_deflate_stream(&p, &r.corrections).ok()? != d[..n] {
            return None;
        }
        Some((n, p, r.corrections))
    }))
    .ok()
    .flatten()
}
/// i = position of the first IDAT length field
fn png_run(d: &[u8], i: usize) -> Option<(usize, Vec<usize>, Vec<u8>)> {
    let (mut p, mut lens, mut z) = (i, vec![], vec![]);
    while d.get(p + 4..p + 8) == Some(&b"IDAT"[..]) {
        let l = be32(&d[p..]) as usize;
        let body = d.get(p + 8..p + 8 + l)?;
        let c = d.get(p + 8 + l..p + 12 + l)?;
        if idat_crc(body) != c {
            return None;
        }
        z.extend_from_slice(body);
        lens.push(l);
        p += 12 + l;
    }
    if lens.is_empty() || lens[0] == 0 {
        None
    } else {
        Some((p - i, lens, z))
    }
}
fn lit(d: &[u8], o: &mut Out, depth: u8) {
    if !d.is_empty() {
        o.b[0].push(1);
        pv(&mut o.b[0], d.len() as u64);
        o.b[if depth == 0 { 1 } else { 2 }].extend_from_slice(d);
    }
}
fn deflate_start(w: &[u8]) -> Option<usize> {
    if w.len() < 32 {
        return None;
    }
    let k = if w.starts_with(b"PK\x03\x04") {
        if w[8] != 8 {
            return None;
        }
        30 + w[26] as usize + 256 * w[27] as usize + w[28] as usize + 256 * w[29] as usize
    } else if w[0] == 0x1f && w[1] == 0x8b && w[2] == 8 && w[3] & 0xe0 == 0 {
        let mut k = 10;
        if w[3] & 4 != 0 {
            k += 2 + w[10] as usize + 256 * w[11] as usize;
        }
        for b in [8, 16] {
            if w[3] & b != 0 {
                while k < w.len() && w[k] != 0 {
                    k += 1;
                }
                k += 1;
            }
        }
        if w[3] & 2 != 0 {
            k += 2;
        }
        k
    } else if w[0] & 15 == 8 && (4..=7).contains(&(w[0] >> 4)) && w[1] & 0x20 == 0 && (w[0] as u32 * 256 + w[1] as u32) % 31 == 0 {
        2
    } else {
        return None;
    };
    if k < w.len() {
        Some(k)
    } else {
        None
    }
}
pub fn expand(d: &[u8], o: &mut Out, depth: u8) {
    let (mut i, mut from) = (0usize, 0usize);
    while i + 4 < d.len() {
        let w = &d[i..];
        if w[0] == 0xff && w[1] == 0xd8 && w[2] == 0xff {
            if let Some(n) = jpeg_end(w).filter(|n| *n > 512) {
                if let Some(v) = lepton(&w[..n]) {
                    lit(&d[from..i], o, depth);
                    o.b[0].push(2);
                    pv(&mut o.b[0], v.len() as u64);
                    o.st[0] += n as u64;
                    o.st[1] += v.len() as u64;
                    o.b[4].extend_from_slice(&v);
                    i += n;
                    from = i;
                    continue;
                }
            }
        }
        if i >= from + 4 && w.starts_with(b"IDAT") {
            let s = i - 4;
            if let Some((n, lens, z)) = png_run(d, s).filter(|r| r.2.len() > 6 && o.room()) {
                if let Some((cn, p, c)) = inflate(&z[2..]) {
                    let tail = &z[2 + cn..];
                    if tail.len() < 64 {
                        lit(&d[from..s], o, depth);
                        let q = &mut o.b[0];
                        q.push(4);
                        pv(q, lens.len() as u64);
                        for (k, l) in lens.iter().enumerate() {
                            pv(q, if k > 0 && *l == lens[0] { 0 } else { *l as u64 + 1 });
                        }
                        q.extend_from_slice(&z[..2]);
                        pv(q, tail.len() as u64);
                        q.extend_from_slice(tail);
                        pv(q, c.len() as u64);
                        pv(q, p.len() as u64);
                        o.b[3].extend_from_slice(&c);
                        o.b[5].extend_from_slice(&p);
                        o.st[4] += n as u64;
                        i = s + n;
                        from = i;
                        continue;
                    }
                }
            }
        }
        if let Some(k) = deflate_start(w).filter(|_| o.room()) {
            if let Some((n, p, c)) = inflate(&w[k..]) {
                lit(&d[from..i + k], o, depth);
                o.b[0].push(3);
                pv(&mut o.b[0], c.len() as u64);
                o.b[3].extend_from_slice(&c);
                o.st[2] += n as u64;
                o.st[3] += p.len() as u64;
                if depth < 2 {
                    expand(&p, o, depth + 1);
                } else {
                    lit(&p, o, depth);
                    o.b[0].push(0);
                }
                i += k + n;
                from = i;
                continue;
            }
        }
        i += 1;
    }
    lit(&d[from..], o, depth);
    o.b[0].push(0);
}


pub struct Rd<'a> {
    b: [&'a [u8]; 6],
    p: [usize; 6],
}
impl<'a> Rd<'a> {
    pub fn new(b: [&'a [u8]; 6]) -> Self {
        Rd { b, p: [0; 6] }
    }
    fn take(&mut self, k: usize, n: usize) -> Result<&'a [u8]> {
        let e = self.p[k].checked_add(n).filter(|e| *e <= self.b[k].len()).context("stream cut short")?;
        let v = &self.b[k][self.p[k]..e];
        self.p[k] = e;
        Ok(v)
    }
    fn v(&mut self) -> Result<usize> {
        let (mut v, mut s) = (0u64, 0);
        loop {
            let c = self.take(0, 1)?[0];
            ensure!(s < 63, "bad number");
            v |= ((c & 127) as u64) << s;
            if c < 128 {
                return Ok(v as usize);
            }
            s += 7;
        }
    }
}
fn recreate(p: &[u8], c: &[u8]) -> Result<Vec<u8>> {
    catch_unwind(AssertUnwindSafe(|| recreate_whole_deflate_stream(p, c).ok())).ok().flatten().context("deflate stream not rebuilt")
}
pub fn unexpand(r: &mut Rd, depth: u8) -> Result<Vec<u8>> {
    ensure!(depth < 8, "nested too deep");
    let mut o = vec![];
    loop {
        match r.take(0, 1)?[0] {
            0 => return Ok(o),
            1 => {
                let n = r.v()?;
                o.extend_from_slice(r.take(if depth == 0 { 1 } else { 2 }, n)?);
            }
            2 => {
                let n = r.v()?;
                let v = r.take(4, n)?;
                o.extend(catch_unwind(AssertUnwindSafe(|| unlepton(v))).ok().context("image not rebuilt")?);
            }
            3 => {
                let n = r.v()?;
                let c = r.take(3, n)?;
                let p = unexpand(r, depth + 1)?;
                o.extend(recreate(&p, c)?);
            }
            4 => {
                let n = r.v()?;
                let mut lens: Vec<usize> = vec![];
                for k in 0..n {
                    let l = r.v()?;
                    lens.push(if k > 0 && l == 0 { lens[0] } else { l.checked_sub(1).context("bad length")? });
                }
                let mut z = r.take(0, 2)?.to_vec();
                let t = r.v()?;
                let tail = r.take(0, t)?;
                let cl = r.v()?;
                let pl = r.v()?;
                let c = r.take(3, cl)?;
                let p = r.take(5, pl)?;
                z.extend(recreate(p, c)?);
                z.extend(tail);
                let mut q = 0usize;
                for l in lens {
                    let body = z.get(q..q.checked_add(l).context("bad length")?).context("image cut short")?;
                    q += l;
                    o.extend((l as u32).to_be_bytes());
                    o.extend(b"IDAT");
                    o.extend(body);
                    o.extend(idat_crc(body));
                }
            }
            t => bail!("unknown operation {t}"),
        }
    }
}
