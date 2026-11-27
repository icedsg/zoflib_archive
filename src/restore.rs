//! Reading: find the index, list, verify, extract.
use crate::{
    container::{self, Keys, Kind, FRAME_LEN},
    group, mail, FileRec, Footer, Form, GroupRec, Index, UnitRec,
};
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Default)]
pub struct Scan {
    pub index: Index,
    pub index_at: Vec<u64>,
    /// first byte after the last index segment (where an interrupted run continues)
    pub end: u64,
    pub complete: bool,
    /// problems met while scanning
    pub damage: Vec<String>,
}

fn add(all: &mut Index, d: Index) {
    all.groups.extend(d.groups);
    all.units.extend(d.units);
    all.files.extend(d.files);
    all.dirs.extend(d.dirs);
}

/// Walks every segment; steps over damaged ones by looking for the next frame.
pub fn scan(f: &mut File, keys: &Keys) -> Result<Scan> {
    let size = f.metadata()?.len();
    let mut s = Scan { end: container::HEADER_LEN as u64, ..Default::default() };
    let mut at = container::HEADER_LEN as u64;
    while at + (FRAME_LEN as u64) < size {
        let fr = match container::read_frame(f, at) {
            Ok(x) if at + FRAME_LEN as u64 + x.len <= size => x,
            _ => {
                if s.damage.len() < 1000 {
                    s.damage.push(format!("no readable segment at {at}"));
                }
                match next_magic(f, at + 1, size)? {
                    Some(n) => {
                        at = n;
                        continue;
                    }
                    None => break,
                }
            }
        };
        let next = at + FRAME_LEN as u64 + fr.len;
        match fr.kind {
            Kind::Index | Kind::Footer | Kind::KeyCheck => match container::read_segment(f, keys, at) {
                Ok((_, p)) => match fr.kind {
                    Kind::Index => {
                        add(&mut s.index, postcard::from_bytes(&p).context("index not readable")?);
                        s.index_at.push(at);
                        s.end = next;
                    }
                    Kind::Footer => s.complete = true,
                    _ => s.end = next,
                },
                Err(e) => {
                    s.damage.push(e.to_string());
                    // a damaged length field must not hide the segments after it
                    match next_magic(f, at + 1, size)? {
                        Some(n) => {
                            at = n;
                            continue;
                        }
                        None => break,
                    }
                }
            },
            Kind::Data => {
                // a damaged length must not make the scan step over good segments: the next one has to start there
                if next + 4 <= size {
                    let mut m = [0u8; 4];
                    f.seek(SeekFrom::Start(next))?;
                    f.read_exact(&mut m)?;
                    if &m != container::SEG_MAGIC {
                        s.damage.push(format!("data segment at {at} does not end where a segment starts"));
                        match next_magic(f, at + 1, size)? {
                            Some(n) => {
                                at = n;
                                continue;
                            }
                            None => break,
                        }
                    }
                }
            }
        }
        at = next;
    }
    Ok(s)
}

fn next_magic(f: &mut File, mut at: u64, size: u64) -> Result<Option<u64>> {
    let mut b = vec![0u8; 1 << 20];
    while at + 4 <= size {
        f.seek(SeekFrom::Start(at))?;
        let n = (size - at).min(b.len() as u64) as usize;
        f.read_exact(&mut b[..n])?;
        if let Some(i) = b[..n].windows(4).position(|w| w == container::SEG_MAGIC) {
            return Ok(Some(at + i as u64));
        }
        at += n as u64 - 3;
        if n < b.len() {
            break;
        }
    }
    Ok(None)
}

pub struct Archive {
    f: File,
    keys: Keys,
    pub complete: bool,
    pub damage: Vec<String>,
    pub files: Vec<FileRec>,
    /// earlier records of paths that were added again later (older versions)
    pub older: Vec<FileRec>,
    pub dirs: Vec<String>,
    groups: HashMap<u64, GroupRec>,
    units: HashMap<u64, UnitRec>,
}

fn by_footer(f: &mut File, keys: &Keys) -> Result<Scan> {
    let size = f.metadata()?.len();
    ensure!(size > 100, "too short");
    f.seek(SeekFrom::Start(size - 16))?;
    let mut t = [0u8; 16];
    f.read_exact(&mut t)?;
    ensure!(&t[8..] == b"ZOFAEND1", "no end mark");
    let (fr, p) = container::read_segment(f, keys, u64::from_le_bytes(t[..8].try_into().unwrap()))?;
    ensure!(fr.kind == Kind::Footer, "no footer");
    let foot: Footer = postcard::from_bytes(&p)?;
    let mut s = Scan { complete: true, ..Default::default() };
    for (i, at) in foot.index_at.iter().enumerate() {
        let (fr, p) = container::read_segment(f, keys, *at)?;
        ensure!(fr.kind == Kind::Index && fr.id == i as u64, "index out of order");
        add(&mut s.index, postcard::from_bytes(&p)?);
    }
    Ok(s)
}

#[derive(Default, Debug)]
pub struct Outcome {
    pub files: u64,
    pub bytes: u64,
    pub failed: Vec<(String, String)>,
}

impl Archive {
    pub fn open(p: &Path, pass: &str) -> Result<Archive> {
        let mut f = File::open(p)?;
        let keys = container::open(&mut f, pass)?;
        let s = match by_footer(&mut f, &keys) {
            Ok(s) => s,
            Err(_) => scan(&mut f, &keys)?,
        };
        // a later record of the same path replaces an earlier one
        let mut files = BTreeMap::new();
        let mut older = vec![];
        for r in s.index.files {
            if let Some(o) = files.insert(r.path.clone(), r) {
                older.push(o);
            }
        }
        Ok(Archive {
            f,
            keys,
            complete: s.complete && s.damage.is_empty(),
            damage: s.damage,
            files: files.into_values().collect(),
            older,
            dirs: s.index.dirs,
            groups: s.index.groups.into_iter().map(|g| (g.id, g)).collect(),
            units: s.index.units.into_iter().map(|u| (u.id, u)).collect(),
        })
    }

    /// Makes older versions part of `files`, each under `path~YYYYMMDD-HHMMSS` (the time it was added), so a restore
    /// writes them next to the latest one.
    pub fn include_older(&mut self) {
        let mut taken: HashSet<String> = self.files.iter().map(|f| f.path.clone()).collect();
        for mut f in std::mem::take(&mut self.older) {
            let base = format!("{}~{}", f.path, stamp(f.added));
            let mut name = base.clone();
            let mut n = 1;
            while !taken.insert(name.clone()) {
                n += 1;
                name = format!("{base}-{n}");
            }
            f.path = name;
            self.files.push(f);
        }
    }

    fn load(&mut self, g: u64, want: &[bool]) -> Result<Vec<Result<Vec<u8>>>> {
        let at = self.groups.get(&g).context("group missing from the index")?.at;
        let (fr, p) = container::read_segment(&mut self.f, &self.keys, at)?;
        ensure!(fr.kind == Kind::Data && fr.id == g, "wrong segment for group {g}");
        group::decode(&p, Some(want))
    }

    /// Runs over the chosen files. `out` = None only verifies. Plain files are written in archive order, so a spinning
    /// disk reads mostly forward; mail files follow, one at a time, because they are rebuilt in memory.
    pub fn restore(&mut self, pick: &dyn Fn(&str) -> bool, out: Option<&Path>) -> Result<Outcome> {
        let files: Vec<FileRec> = self.files.iter().filter(|f| pick(&f.path)).cloned().collect();
        let mut oc = Outcome::default();
        let mut bad: HashMap<usize, String> = HashMap::new();
        if let Some(o) = out {
            fs::create_dir_all(o)?;
            for d in &self.dirs {
                if pick(d) {
                    fs::create_dir_all(safe(o, d)?)?;
                }
            }
        }
        // unit -> places it goes
        let mut dest: HashMap<u64, Vec<(usize, u64)>> = HashMap::new();
        let mut hashers: HashMap<usize, Order> = HashMap::new();
        let mut need: HashMap<u64, HashSet<u32>> = HashMap::new();
        for (i, f) in files.iter().enumerate() {
            if f.form == Form::Mail {
                continue;
            }
            let mut off = 0;
            for u in &f.units {
                match self.units.get(u) {
                    Some(r) => {
                        dest.entry(*u).or_default().push((i, off));
                        need.entry(r.group).or_default().insert(r.slot);
                        off += r.len;
                    }
                    None => {
                        bad.insert(i, "data missing from the index".into());
                    }
                }
            }
            if off != f.size {
                bad.entry(i).or_insert("size does not match".into());
            }
            hashers.insert(i, Order::default());
            if f.units.is_empty() {
                if let Some(o) = out {
                    if let Err(e) = write_at(o, &f.path, 0, &[], true) {
                        bad.insert(i, format!("{e:#}"));
                    }
                }
            }
        }
        let slots: HashMap<(u64, u32), u64> = self.units.values().map(|u| ((u.group, u.slot), u.id)).collect();
        let mut order: Vec<u64> = need.keys().copied().collect();
        order.sort_by_key(|g| self.groups.get(g).map(|x| x.at).unwrap_or(0));
        let mut started: HashSet<usize> = HashSet::new();
        for g in order {
            let n = need[&g].iter().max().unwrap() + 1;
            let want: Vec<bool> = (0..n).map(|k| need[&g].contains(&k)).collect();
            let r = self.load(g, &want);
            let mut r = match r {
                Ok(v) => v.into_iter().map(Some).collect(),
                Err(e) => {
                    let m = format!("{e:#}");
                    let mut v: Vec<Option<Result<Vec<u8>>>> = vec![];
                    for _ in 0..n {
                        v.push(Some(Err(anyhow::anyhow!(m.clone()))));
                    }
                    v
                }
            };
            for k in &need[&g] {
                let id = slots[&(g, *k)];
                let u = match r.get_mut(*k as usize).and_then(|x| x.take()) {
                    Some(Ok(d)) if d.len() as u64 == self.units[&id].len => Ok(d),
                    Some(Err(e)) => Err(format!("{e:#}")),
                    _ => Err("unit has the wrong length".to_string()),
                };
                for (i, off) in dest.get(&id).cloned().unwrap_or_default() {
                    match &u {
                        Ok(d) => {
                            if let Some(o) = out {
                                if let Err(e) = write_at(o, &files[i].path, off, d, started.insert(i)) {
                                    bad.entry(i).or_insert(format!("{e:#}"));
                                }
                            }
                            hashers.get_mut(&i).unwrap().add(off, *blake3::hash(d).as_bytes(), d);
                        }
                        Err(m) => {
                            bad.entry(i).or_insert(m.clone());
                        }
                    }
                }
            }
        }
        for (i, f) in files.iter().enumerate() {
            if f.form == Form::Plain && !bad.contains_key(&i) {
                let ok = match hashers[&i].finish() {
                    Some(h) => h == f.hash,
                    // pieces came out of order: check every piece by its unit hash, and the file as written
                    None => {
                        let ph = hashers[&i].pieces.clone();
                        let mut off = 0;
                        f.units.iter().all(|u| {
                            let r = &self.units[u];
                            let k = ph.get(&off) == Some(&r.hash);
                            off += r.len;
                            k
                        }) && match out {
                            Some(o) => hash_file(&safe(o, &f.path)?)? == f.hash,
                            None => true,
                        }
                    }
                };
                if !ok {
                    bad.insert(i, "contents do not match the recorded hash".into());
                }
            }
            if f.form == Form::Mail {
                if let Err(e) = self.mail(f, out) {
                    bad.insert(i, format!("{e:#}"));
                }
            }
            match bad.get(&i) {
                None => {
                    if let Some(o) = out {
                        let (e, d) = (std::time::UNIX_EPOCH, std::time::Duration::new(f.mtime.0.unsigned_abs(), 0));
                        let t = if f.mtime.0 >= 0 { e.checked_add(d) } else { e.checked_sub(d) }
                            .and_then(|t| t.checked_add(std::time::Duration::from_nanos(f.mtime.1 as u64)));
                        if let Some(t) = t {
                            std::fs::File::options().write(true).open(safe(o, &f.path)?)?.set_times(std::fs::FileTimes::new().set_modified(t))?;
                        }
                    }
                    oc.files += 1;
                    oc.bytes += f.size;
                }
                Some(m) => {
                    // never leave a wrong file looking like a good one
                    if let Some(o) = out {
                        if let Ok(p) = safe(o, &f.path) {
                            let _ = fs::remove_file(p);
                        }
                    }
                    oc.failed.push((f.path.clone(), m.clone()));
                }
            }
        }
        Ok(oc)
    }

    fn mail(&mut self, f: &FileRec, out: Option<&Path>) -> Result<()> {
        ensure!(f.units.len() >= 2, "mail record incomplete");
        let mut recs = vec![];
        for u in &f.units {
            recs.push(self.units.get(u).context("data missing from the index")?.clone());
        }
        let mut need: BTreeMap<u64, HashSet<u32>> = BTreeMap::new();
        for r in &recs {
            need.entry(r.group).or_default().insert(r.slot);
        }
        let mut order: Vec<u64> = need.keys().copied().collect();
        order.sort_by_key(|g| self.groups.get(g).map(|x| x.at).unwrap_or(0));
        let mut got: HashMap<(u64, u32), Vec<u8>> = HashMap::new();
        for g in order {
            let n = need[&g].iter().max().unwrap() + 1;
            let want: Vec<bool> = (0..n).map(|k| need[&g].contains(&k)).collect();
            for (k, u) in self.load(g, &want)?.into_iter().enumerate() {
                if want.get(k) == Some(&true) {
                    got.insert((g, k as u32), u?);
                }
            }
        }
        let mut parts = vec![];
        for r in &recs {
            parts.push(got.get(&(r.group, r.slot)).context("unit missing")?.clone());
        }
        drop(got);
        let layout = parts.pop().unwrap();
        let rest = parts.pop().unwrap();
        ensure!(f.size <= 8 << 30, "mail file too large");
        let d = mail::join(&parts, &rest, &layout, f.size as usize)?;
        ensure!(blake3::hash(&d).as_bytes() == &f.hash, "contents do not match the recorded hash");
        if let Some(o) = out {
            write_at(o, &f.path, 0, &d, true)?;
        }
        Ok(())
    }
}

/// File hash when pieces arrive in order; otherwise piece hashes for a later check.
#[derive(Default)]
struct Order {
    h: blake3::Hasher,
    next: u64,
    broken: bool,
    pieces: HashMap<u64, [u8; 32]>,
}
impl Order {
    fn add(&mut self, off: u64, hash: [u8; 32], d: &[u8]) {
        if off == self.next && !self.broken {
            self.h.update(d);
            self.next += d.len() as u64;
        } else {
            self.broken = true;
        }
        self.pieces.insert(off, hash);
    }
    fn finish(&self) -> Option<[u8; 32]> {
        (!self.broken).then(|| *self.h.finalize().as_bytes())
    }
}

fn hash_file(p: &Path) -> Result<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    let mut f = File::open(p)?;
    let mut b = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            return Ok(*h.finalize().as_bytes());
        }
        h.update(&b[..n]);
    }
}

/// Joins a stored path to the output folder; refuses anything that could leave it.
pub fn safe(out: &Path, rel: &str) -> Result<PathBuf> {
    let mut p = out.to_path_buf();
    for c in rel.split('/') {
        if c.is_empty() || c == "." || c == ".." || c.contains(['\\', ':', '\0']) {
            bail!("unsafe path in archive");
        }
        p.push(c);
        // do not write through links that are already there
        if let Ok(m) = fs::symlink_metadata(&p) {
            ensure!(!m.file_type().is_symlink(), "a link is in the way");
        }
    }
    Ok(p)
}

fn write_at(out: &Path, rel: &str, off: u64, d: &[u8], first: bool) -> Result<()> {
    let p = safe(out, rel)?;
    if first {
        if let Some(d) = p.parent() {
            fs::create_dir_all(d)?;
        }
    }
    let mut f = OpenOptions::new().write(true).create(first).truncate(first).open(&p).context("writing a file")?;
    f.seek(SeekFrom::Start(off))?;
    f.write_all(d)?;
    Ok(())
}

/// `YYYYMMDD-HHMMSS` in UTC for seconds since 1970.
pub fn stamp(t: i64) -> String {
    let (d, s) = (t.div_euclid(86400), t.rem_euclid(86400));
    // days to civil date (Howard Hinnant's algorithm)
    let z = d + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}{month:02}{day:02}-{:02}{:02}{:02}", s / 3600, s / 60 % 60, s % 60)
}
