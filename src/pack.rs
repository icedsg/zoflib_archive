//! Packing: read files in order, fill one open group per category, compress groups on worker threads, write from one thread.
use crate::{
    container::{self, Keys, Kind},
    group::{self, Cat, Level},
    mail, postcard_vec,
    restore::{self, Scan},
    FileRec, Footer, Form, GroupRec, Hash, Index, UnitRec,
};
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::mpsc,
    time::UNIX_EPOCH,
};

#[derive(Clone, Copy)]
pub struct Options {
    pub level: Level,
    /// bytes of input per group
    pub group: usize,
    /// groups being compressed at once
    pub in_flight: usize,
    pub resume: bool,
    /// index written after this many stored bytes; what an interrupted run can lose
    pub checkpoint: u64,
    /// stop early after this many checkpoints, as if interrupted (tests)
    pub stop_after: Option<usize>,
}
impl Default for Options {
    fn default() -> Self {
        Options { level: Level::Balanced, group: 64 << 20, in_flight: 4, resume: false, checkpoint: 256 << 20, stop_after: None }
    }
}
#[derive(Default, Debug)]
pub struct Report {
    pub files: u64,
    pub bytes: u64,
    pub resumed_files: u64,
    pub duplicate_bytes: u64,
    pub archive_bytes: u64,
    /// files and links left out, with the reason
    pub skipped: Vec<(String, String)>,
    pub complete: bool,
}

const PART: usize = 64 << 20;
const MAIL_MAX: u64 = 6 << 30;

type Done = (u64, Result<Vec<u8>>);

struct Packer {
    f: File,
    at: u64,
    keys: Keys,
    o: Options,
    open: BTreeMap<Cat, (u64, Vec<Vec<u8>>, usize)>,
    next_group: u64,
    next_unit: u64,
    next_index: u64,
    by_hash: HashMap<(Hash, u64), (u64, u64)>, // -> unit, group
    new_units: HashMap<u64, Vec<UnitRec>>,     // by group
    waiting: Vec<(FileRec, HashSet<u64>)>,
    written: HashSet<u64>,
    delta: Index,
    since: u64,
    index_at: Vec<u64>,
    checkpoints: usize,
    busy: usize,
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
    pool: rayon::ThreadPool,
    rep: Report,
}

impl Packer {
    fn write_segment(&mut self, s: &[u8]) -> Result<u64> {
        let at = self.at;
        self.f.write_all(s).context("writing the archive")?;
        self.at += s.len() as u64;
        Ok(at)
    }
    fn take_done(&mut self, d: Done) -> Result<()> {
        self.busy -= 1;
        let s = d.1?;
        let at = self.write_segment(&s)?;
        self.since += s.len() as u64;
        self.delta.groups.push(GroupRec { id: d.0, at, len: s.len() as u64 });
        self.delta.units.extend(self.new_units.remove(&d.0).unwrap_or_default());
        self.written.insert(d.0);
        if self.since >= self.o.checkpoint {
            self.checkpoint()?;
        }
        Ok(())
    }
    fn checkpoint(&mut self) -> Result<()> {
        let w = &self.written;
        let (ready, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting).into_iter().partition(|(_, g)| g.is_subset(w));
        self.waiting = rest;
        self.delta.files.extend(ready.into_iter().map(|x| x.0));
        let d = std::mem::take(&mut self.delta);
        self.since = 0;
        // data first, then the index that points at it
        self.f.sync_data()?;
        let s = container::seal(&self.keys, Kind::Index, self.next_index, &postcard_vec(&d)?)?;
        self.next_index += 1;
        let at = self.write_segment(&s)?;
        self.f.sync_data()?;
        self.index_at.push(at);
        self.checkpoints += 1;
        Ok(())
    }
    fn stopped(&self) -> bool {
        self.o.stop_after.map(|n| self.checkpoints >= n).unwrap_or(false)
    }
    fn flush(&mut self, cat: Cat) -> Result<()> {
        let Some((id, units, _)) = self.open.remove(&cat) else { return Ok(()) };
        while self.busy >= self.o.in_flight {
            let d = self.rx.recv()?;
            self.take_done(d)?;
        }
        while let Ok(d) = self.rx.try_recv() {
            self.take_done(d)?;
        }
        self.busy += 1;
        let (tx, keys, level) = (self.tx.clone(), self.keys.clone(), self.o.level);
        self.pool.spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                group::encode(&units, cat, level).and_then(|g| container::seal(&keys, Kind::Data, id, &g))
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("compressing a group failed")));
            let _ = tx.send((id, r));
        });
        Ok(())
    }
    fn unit(&mut self, cat: Cat, d: Vec<u8>, groups: &mut HashSet<u64>) -> Result<u64> {
        let key = (*blake3::hash(&d).as_bytes(), d.len() as u64);
        if let Some((u, g)) = self.by_hash.get(&key) {
            groups.insert(*g);
            self.rep.duplicate_bytes += key.1;
            return Ok(*u);
        }
        if let Some((_, _, n)) = self.open.get(&cat) {
            if n + d.len() > self.o.group {
                self.flush(cat)?;
            }
        }
        if !self.open.contains_key(&cat) {
            self.open.insert(cat, (self.next_group, vec![], 0));
            self.next_group += 1;
        }
        let e = self.open.get_mut(&cat).unwrap();
        let (gid, slot) = (e.0, e.1.len() as u32);
        e.2 += d.len();
        e.1.push(d);
        let id = self.next_unit;
        self.next_unit += 1;
        self.by_hash.insert(key, (id, gid));
        self.new_units.entry(gid).or_default().push(UnitRec { id, hash: key.0, len: key.1, group: gid, slot });
        groups.insert(gid);
        Ok(id)
    }
    fn file(&mut self, p: &Path, name: &str) -> Result<()> {
        let mut f = File::open(p)?;
        let m = f.metadata()?;
        let (size, mtime) = (m.len(), mtime(&m)?);
        let cat = group::category(name);
        let mut h = blake3::Hasher::new();
        let mut units = vec![];
        let mut groups = HashSet::new();
        let mut form = Form::Plain;
        if cat == Cat::Mail && size <= MAIL_MAX {
            let mut d = Vec::with_capacity(size as usize);
            f.read_to_end(&mut d)?;
            ensure!(d.len() as u64 == size, "changed while being read");
            h.update(&d);
            // the split is only used when it rebuilds this exact file
            let s = mail::split(&d).filter(|s| mail::join(&s.data, &s.rest, &s.layout, d.len()).map(|x| x == d).unwrap_or(false));
            match s {
                Some(s) => {
                    drop(d);
                    form = Form::Mail;
                    for u in s.data {
                        units.push(self.unit(Cat::Mail, u, &mut groups)?);
                    }
                    // the rest, then the layout, are always the last two units
                    units.push(self.unit(Cat::Other, s.rest, &mut groups)?);
                    units.push(self.unit(Cat::Other, s.layout, &mut groups)?);
                }
                None => {
                    for c in d.chunks(PART) {
                        units.push(self.unit(Cat::Other, c.to_vec(), &mut groups)?);
                    }
                }
            }
        } else {
            let mut left = size;
            while left > 0 {
                let mut d = vec![0u8; left.min(PART as u64) as usize];
                f.read_exact(&mut d).context("changed while being read")?;
                left -= d.len() as u64;
                h.update(&d);
                units.push(self.unit(cat, d, &mut groups)?);
            }
        }
        let m2 = fs::metadata(p)?;
        ensure!(m2.len() == size && self::mtime(&m2)? == mtime, "changed while being read");
        self.rep.files += 1;
        self.rep.bytes += size;
        let rec = FileRec { path: name.into(), size, mtime, hash: *h.finalize().as_bytes(), form, units };
        self.waiting.push((rec, groups));
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        let cats: Vec<Cat> = self.open.keys().copied().collect();
        for c in cats {
            self.flush(c)?;
        }
        while self.busy > 0 {
            let d = self.rx.recv()?;
            self.take_done(d)?;
        }
        self.checkpoint()?;
        ensure!(self.waiting.is_empty(), "internal: files without data");
        Ok(())
    }
}

pub fn mtime(m: &fs::Metadata) -> Result<(i64, u32)> {
    let t = m.modified()?;
    Ok(match t.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
        Err(e) => {
            let d = e.duration();
            if d.subsec_nanos() == 0 { (-(d.as_secs() as i64), 0) } else { (-(d.as_secs() as i64) - 1, 1_000_000_000 - d.subsec_nanos()) }
        }
    })
}

pub fn pack(src: &Path, dst: &Path, pass: &str, o: Options) -> Result<Report> {
    ensure!(src.is_dir(), "source is not a folder");
    let src = src.canonicalize()?;
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(!o.resume)
        .create(o.resume)
        .truncate(false)
        .open(dst)
        .context("opening the archive (it must not exist yet unless resuming)")?;
    let me = dst.canonicalize()?;
    let mut scan = Scan::default();
    let (keys, at) = if o.resume && f.metadata()?.len() > 0 {
        let keys = container::open(&mut f, pass)?;
        scan = restore::scan(&mut f, &keys)?;
        if scan.complete {
            bail!("this archive is already complete");
        }
        // everything after the last index belongs to files that are packed again
        f.set_len(scan.end)?;
        f.seek(SeekFrom::Start(scan.end))?;
        (keys, scan.end)
    } else {
        container::create(&mut f, pass)?
    };
    let have: HashMap<String, (u64, (i64, u32))> = scan.index.files.iter().map(|x| (x.path.clone(), (x.size, x.mtime))).collect();
    let had_dirs: HashSet<String> = scan.index.dirs.iter().cloned().collect();
    let (tx, rx) = mpsc::channel();
    let mut p = Packer {
        f,
        at,
        keys,
        o,
        open: BTreeMap::new(),
        next_group: scan.index.groups.iter().map(|g| g.id + 1).max().unwrap_or(0),
        next_unit: scan.index.units.iter().map(|u| u.id + 1).max().unwrap_or(0),
        next_index: scan.index_at.len() as u64,
        by_hash: scan.index.units.iter().map(|u| ((u.hash, u.len), (u.id, u.group))).collect(),
        new_units: HashMap::new(),
        waiting: vec![],
        written: scan.index.groups.iter().map(|g| g.id).collect(),
        delta: Index::default(),
        since: 0,
        index_at: scan.index_at.clone(),
        checkpoints: 0,
        busy: 0,
        tx,
        rx,
        pool: rayon::ThreadPoolBuilder::new().build()?,
        rep: Report::default(),
    };
    for e in walkdir::WalkDir::new(&src).follow_links(false).sort_by_file_name() {
        if p.stopped() {
            return Ok(p.rep);
        }
        let e = match e {
            Ok(e) => e,
            Err(x) => {
                let n = x.path().map(|q| q.display().to_string()).unwrap_or_default();
                p.rep.skipped.push((n, x.to_string()));
                continue;
            }
        };
        if e.path() == me {
            continue;
        }
        let rel = e.path().strip_prefix(&src)?;
        let Some(name) = rel.to_str().map(|x| x.replace('\\', "/")) else {
            p.rep.skipped.push((rel.display().to_string(), "name is not valid Unicode".into()));
            continue;
        };
        if name.is_empty() {
            continue;
        }
        if e.path_is_symlink() || !(e.file_type().is_dir() || e.file_type().is_file()) {
            p.rep.skipped.push((name, "link or special file".into()));
        } else if e.file_type().is_dir() {
            if !had_dirs.contains(&name) {
                p.delta.dirs.push(name);
            }
        } else {
            if let Some(h) = have.get(&name) {
                let m = e.metadata()?;
                if *h == (m.len(), mtime(&m)?) {
                    p.rep.resumed_files += 1;
                    p.rep.files += 1;
                    p.rep.bytes += h.0;
                    continue;
                }
                // changed since: packed again below, and the newer record wins on restore
            }
            if let Err(x) = p.file(e.path(), &name) {
                // a write problem ends the run; a problem with one file does not
                if x.chain().any(|c| c.to_string() == "writing the archive") {
                    return Err(x);
                }
                p.rep.skipped.push((name, format!("{x:#}")));
            }
        }
    }
    p.finish()?;
    let foot = Footer { index_at: p.index_at.clone(), files: p.rep.files, bytes: p.rep.bytes };
    let s = container::seal(&p.keys, Kind::Footer, 0, &postcard_vec(&foot)?)?;
    let foot_at = p.write_segment(&s)?;
    let mut tail = foot_at.to_le_bytes().to_vec();
    tail.extend(b"ZOFAEND1");
    p.write_segment(&tail)?;
    p.f.sync_all()?;
    p.rep.archive_bytes = p.at;
    p.rep.complete = true;
    Ok(p.rep)
}
