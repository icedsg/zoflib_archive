// Experiment: undo weak coding inside files (deflate, PNG, JPEG, Outlook mail), write separate streams, rebuild and verify.
mod pst;
use lepton_jpeg::*;
use preflate_rs::*;
use rayon::prelude::*;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Cursor,
    panic::{catch_unwind, AssertUnwindSafe},
    path::*,
    time::Instant,
};

fn pv(o: &mut Vec<u8>, mut v: u64) {
    while v > 127 {
        o.push(v as u8 | 128);
        v >>= 7;
    }
    o.push(v as u8);
}
fn gv(b: &[u8], p: &mut usize) -> u64 {
    let (mut v, mut s) = (0, 0);
    loop {
        let c = b[*p];
        *p += 1;
        v |= ((c & 127) as u64) << s;
        if c < 128 {
            return v;
        }
        s += 7;
    }
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
const NAMES: [&str; 6] = ["ops", "main", "plain", "corr", "jpg", "png"];
#[derive(Default)]
struct Out {
    b: [Vec<u8>; 6],
    st: [u64; 5],
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
fn expand(d: &[u8], o: &mut Out, depth: u8) {
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
            if let Some((n, lens, z)) = png_run(d, s).filter(|r| r.2.len() > 6) {
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
        if let Some(k) = deflate_start(w) {
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

struct Rd {
    b: Vec<Vec<u8>>,
    p: [usize; 6],
}
impl Rd {
    fn take(&mut self, k: usize, n: usize) -> Vec<u8> {
        let v = self.b[k][self.p[k]..self.p[k] + n].to_vec();
        self.p[k] += n;
        v
    }
    fn v(&mut self) -> usize {
        gv(&self.b[0], &mut self.p[0]) as usize
    }
}
fn unexpand(r: &mut Rd, depth: u8) -> Vec<u8> {
    let mut o = vec![];
    loop {
        let t = r.take(0, 1)[0];
        match t {
            0 => return o,
            1 => {
                let n = r.v();
                o.extend(r.take(if depth == 0 { 1 } else { 2 }, n));
            }
            2 => {
                let n = r.v();
                o.extend(unlepton(&r.take(4, n)));
            }
            3 => {
                let n = r.v();
                let c = r.take(3, n);
                let p = unexpand(r, depth + 1);
                o.extend(recreate_whole_deflate_stream(&p, &c).unwrap());
            }
            4 => {
                let n = r.v();
                let mut lens: Vec<usize> = vec![];
                for k in 0..n {
                    let l = r.v();
                    lens.push(if k > 0 && l == 0 { lens[0] } else { l - 1 });
                }
                let mut z = r.take(0, 2);
                let t = r.v();
                let tail = r.take(0, t);
                let cl = r.v();
                let pl = r.v();
                let c = r.take(3, cl);
                let p = r.take(5, pl);
                z.extend(recreate_whole_deflate_stream(&p, &c).unwrap());
                z.extend(tail);
                let mut q = 0;
                for l in lens {
                    let body = &z[q..q + l];
                    q += l;
                    o.extend((l as u32).to_be_bytes());
                    o.extend(b"IDAT");
                    o.extend(body);
                    o.extend(idat_crc(body));
                }
            }
            _ => panic!("tag"),
        }
    }
}
fn cat(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "exe" | "dll" | "so" | "lib" | "msi" => "bin.exe",
        "js" | "html" | "htm" | "css" | "java" | "xml" | "txt" | "log" | "py" | "php" | "sql" | "json" | "csv" | "c" | "h" | "md" => "text.txt",
        "pst" | "ost" => "mail.dat",
        _ => "misc.dat",
    }
}
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let a: Vec<String> = std::env::args().collect();
    let (src, dst) = (PathBuf::from(&a[1]), PathBuf::from(&a[2]));
    let nopst = a.get(3).map(|s| s == "nopst").unwrap_or(false);
    let t = Instant::now();
    let mut files: Vec<PathBuf> = fs::read_dir(&src).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort_by_key(|p| (cat(p), p.extension().map(|x| x.to_owned()), p.clone()));
    let _ = fs::remove_dir_all(&dst);
    fs::create_dir_all(&dst).unwrap();
    let mut by: BTreeMap<&str, Vec<PathBuf>> = BTreeMap::new();
    for f in files {
        by.entry(cat(&f)).or_default().push(f);
    }
    let (mut tin, mut tout) = (0u64, 0u64);
    let mut allok = true;
    for (c, fl) in by {
        let mut units: Vec<Vec<u8>> = vec![];
        let (mut meta, mut originals, mut rest_all) = (vec![], vec![], Vec::<u8>::new());
        for f in &fl {
            let d = fs::read(f).unwrap();
            tin += d.len() as u64;
            originals.push((blake3::hash(&d), d.len()));
            let pl = if nopst { None } else { pst::plan(&d) };
            if let Some(g) = pl {
                let (mut bytes, mut np) = (0, 0);
                let mut cut: Vec<(usize, usize, bool)> = vec![];
                meta.push(1u8);
                pv(&mut meta, d.len() as u64);
                pv(&mut meta, g.len() as u64);
                let mut last = 0i64;
                for gr in &g {
                    pv(&mut meta, gr.len() as u64);
                    let mut u = vec![];
                    for b in gr {
                        let dl = b.ib as i64 / 64 - last;
                        last = b.ib as i64 / 64;
                        pv(&mut meta, ((dl << 1) ^ (dl >> 63)) as u64);
                        pv(&mut meta, (b.cb as u64) << 1 | b.predicted as u64);
                        u.extend(d[b.ib..b.ib + b.cb].iter().map(|x| pst::DEC[*x as usize]));
                        bytes += b.cb;
                        np += b.predicted as usize;
                        cut.push((b.ib, b.cb, b.predicted));
                    }
                    units.push(u);
                }
                cut.sort();
                let nb = cut.len();
                let mut p = 0;
                for (ib, cb, pr) in cut {
                    rest_all.extend(&d[p..ib]);
                    p = ib + cb;
                    if pr {
                        let e = ib + pst::total(cb);
                        rest_all.extend(&d[p..e - 16]);
                        p = e - 8;
                    }
                }
                rest_all.extend(&d[p..]);
                println!("  pst {} MB: {} groups, {} blocks, {} predicted trailers, {} MB data", d.len() / 1_000_000, g.len(), nb, np, bytes / 1_000_000);
            } else {
                meta.push(0);
                units.push(d);
            }
        }
        let mut seen: HashMap<blake3::Hash, usize> = HashMap::new();
        let mut refs = vec![];
        let mut dupb = 0;
        for (i, u) in units.iter().enumerate() {
            let h = blake3::hash(u);
            match seen.get(&h) {
                Some(j) if u.len() > 64 => {
                    refs.push(Some(*j));
                    dupb += u.len();
                }
                _ => {
                    seen.insert(h, i);
                    refs.push(None);
                }
            }
        }
        let outs: Vec<Out> = units
            .par_iter()
            .zip(refs.par_iter())
            .map(|(u, r)| {
                let mut o = Out::default();
                if r.is_none() {
                    expand(u, &mut o, 0);
                }
                o
            })
            .collect();
        drop(units);
        let mut b: Vec<Vec<u8>> = vec![vec![]; 6];
        let mut st = [0u64; 5];
        for o in outs {
            for k in 0..6 {
                b[k].extend(&o.b[k]);
            }
            for k in 0..5 {
                st[k] += o.st[k];
            }
        }
        let enc_t = t.elapsed();
        let mut rmeta = vec![];
        for r in &refs {
            pv(&mut rmeta, r.map(|x| x as u64 + 1).unwrap_or(0));
        }
        for k in 0..6 {
            if !b[k].is_empty() {
                tout += b[k].len() as u64;
                fs::write(dst.join(format!("{}_{}", NAMES[k], c)), &b[k]).unwrap();
            }
        }
        fs::write(dst.join(format!("meta_{c}")), [&meta[..], &rmeta[..]].concat()).unwrap();
        if !rest_all.is_empty() {
            tout += rest_all.len() as u64;
            fs::write(dst.join(format!("rest_{c}")), &rest_all).unwrap();
        }
        println!(
            "{c}: {} files, dup units {} MB, jpeg {}->{} KB, deflate {}->{} KB, png {} KB, at {:.0}s",
            fl.len(), dupb / 1_000_000, st[0] / 1000, st[1] / 1000, st[2] / 1000, st[3] / 1000, st[4] / 1000, enc_t.as_secs_f64()
        );
        // rebuild every file from the streams only
        let tv = Instant::now();
        let mut r = Rd { b, p: [0; 6] };
        let mut done: Vec<Vec<u8>> = vec![];
        let keep: HashSet<usize> = refs.iter().flatten().cloned().collect();
        let (mut mp, mut ui, mut rp, mut okc) = (0usize, 0usize, 0usize, 0usize);
        let enc = pst::enc_table();
        let mut next = |r: &mut Rd| {
            let i = ui;
            ui += 1;
            let u = match refs[i] {
                Some(j) => done[j].clone(),
                None => unexpand(r, 0),
            };
            done.push(if keep.contains(&i) { u.clone() } else { vec![] });
            u
        };
        for (h, len) in &originals {
            let k = meta[mp];
            mp += 1;
            let d = if k == 0 {
                next(&mut r)
            } else {
                let l = gv(&meta, &mut mp) as usize;
                let ng = gv(&meta, &mut mp);
                let mut last = 0i64;
                let mut bl = vec![];
                let mut d = vec![0u8; l];
                for _ in 0..ng {
                    let n = gv(&meta, &mut mp);
                    let u = next(&mut r);
                    let mut q = 0;
                    for _ in 0..n {
                        let z = gv(&meta, &mut mp);
                        last += ((z >> 1) as i64) ^ -((z & 1) as i64);
                        let x = gv(&meta, &mut mp) as usize;
                        let (ib, cb) = (last as usize * 64, x >> 1);
                        for j in 0..cb {
                            d[ib + j] = enc[u[q + j] as usize];
                        }
                        q += cb;
                        bl.push((ib, cb, x & 1 == 1));
                    }
                }
                bl.sort();
                let mut p = 0;
                let mut cp = |d: &mut Vec<u8>, x: usize, y: usize| {
                    d[x..y].copy_from_slice(&rest_all[rp..rp + y - x]);
                    rp += y - x;
                };
                let mut fix = vec![];
                for (ib, cb, pr) in bl {
                    cp(&mut d, p, ib);
                    p = ib + cb;
                    if pr {
                        let e = ib + pst::total(cb);
                        cp(&mut d, p, e - 16);
                        p = e - 8;
                        fix.push((ib, cb, e));
                    }
                }
                cp(&mut d, p, l);
                for (ib, cb, e) in fix {
                    let c = pst::crc(&d[ib..ib + cb]);
                    let bid = u64::from_le_bytes(d[e - 8..e].try_into().unwrap());
                    d[e - 16..e - 14].copy_from_slice(&(cb as u16).to_le_bytes());
                    d[e - 14..e - 12].copy_from_slice(&pst::sig(ib as u64, bid).to_le_bytes());
                    d[e - 12..e - 8].copy_from_slice(&c.to_le_bytes());
                }
                d
            };
            if d.len() == *len && blake3::hash(&d) == *h {
                okc += 1;
            }
        }
        allok &= okc == originals.len();
        println!("{c}: verified {okc}/{} files byte-identical in {:.0}s", originals.len(), tv.elapsed().as_secs_f64());
    }
    println!("in {} MB, streams {} MB, total {:.0}s, all verified {}", tin / 1_000_000, tout / 1_000_000, t.elapsed().as_secs_f64(), allok);
}
