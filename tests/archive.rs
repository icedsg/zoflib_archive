use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use zoflib_archive::{
    group::Level,
    pack::{pack, Options},
    restore::{safe, Archive},
};

struct Rng(u64);
impl Rng {
    fn n(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.n() >> 24) as u8).collect()
    }
}
fn text(r: &mut Rng, n: usize) -> Vec<u8> {
    let w = ["alpha", "beta", "gamma", "delta", "line", "value", "\n", "0", "="];
    let mut o = vec![];
    while o.len() < n {
        o.extend(w[(r.n() % 9) as usize].bytes());
        o.push(b' ');
    }
    o
}
fn gz(d: &[u8], level: u32) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::new(level));
    e.write_all(d).unwrap();
    e.finish().unwrap()
}
fn png(px: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::new(6));
    z.write_all(px).unwrap();
    let z = z.finish().unwrap();
    let mut o = b"\x89PNG\r\n\x1a\n".to_vec();
    for c in z.chunks(8192) {
        o.extend((c.len() as u32).to_be_bytes());
        let mut h = crc32fast::Hasher::new();
        h.update(b"IDAT");
        h.update(c);
        o.extend(b"IDAT");
        o.extend(c);
        o.extend(h.finalize().to_be_bytes());
    }
    o
}
fn make(dir: &Path, seed: u64) {
    let mut r = Rng(seed);
    fs::create_dir_all(dir.join("a/b")).unwrap();
    fs::create_dir_all(dir.join("empty")).unwrap();
    let t = text(&mut r, 400_000);
    let put = |n: &str, d: &[u8]| fs::write(dir.join(n), d).unwrap();
    put("a/t.txt", &t);
    put("a/copy.txt", &t);
    put("a/b/zero.bin", b"");
    put("a/b/one.bin", b"x");
    put("a/rand.mp4", &r.bytes(3_000_000));
    put("a/x.gz", &gz(&t, 9));
    put("a/y.zip", &[gz(&t[..100_000], 1), r.bytes(5000), gz(&t, 6)].concat());
    put("p.png", &png(&text(&mut r, 200_000)));
    let g = gz(&t, 6);
    put("cut.gz", &g[..g.len() / 2]);
    put("fakejpeg.jpg", &[&[0xff, 0xd8, 0xff, 0xe0, 0, 4, 1, 2, 0xff, 0xda, 0, 2][..], &r.bytes(9000), &[0xff, 0xd9]].concat());
    put("sneaky.dat", b"ZSG1\x01 looks like a segment ZSG1 ZOFAEND1");
    put("uni-\u{0bb5}\u{1f600}.txt", "\u{0bb5}\u{0ba3}".as_bytes());
    for i in 0..300 {
        let n = (r.n() % 9000) as usize;
        put(&format!("a/b/f{i}.js"), &text(&mut r, n));
    }
    put("prog.exe", &[b"MZ".to_vec(), r.bytes(50_000), t].concat());
}
fn tree(d: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut m = BTreeMap::new();
    for e in walkdir::WalkDir::new(d) {
        let e = e.unwrap();
        let n = e.path().strip_prefix(d).unwrap().to_str().unwrap().replace('\\', "/");
        m.insert(n, if e.file_type().is_file() { fs::read(e.path()).unwrap() } else { b"<dir>".to_vec() });
    }
    m
}
fn opts(level: Level) -> Options {
    Options { level, group: 1 << 20, checkpoint: 1 << 20, ..Default::default() }
}
fn setup(seed: u64) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let (s, a) = (t.path().join("src"), t.path().join("a.zofa"));
    make(&s, seed);
    (t, s, a)
}
fn extract(a: &Path, o: &Path) -> zoflib_archive::restore::Outcome {
    Archive::open(a, "pw").unwrap().restore(&|_| true, Some(o)).unwrap()
}

#[test]
fn round_trip_every_level() {
    for (i, l) in [Level::Fast, Level::Balanced, Level::Max].into_iter().enumerate() {
        let (t, s, a) = setup(7 + i as u64);
        let r = pack(&s, &a, "pw", opts(l)).unwrap();
        assert!(r.complete && r.skipped.is_empty(), "{:?}", r.skipped);
        assert!(r.duplicate_bytes >= 400_000);
        let o = t.path().join("out");
        let x = extract(&a, &o);
        assert!(x.failed.is_empty(), "{:?}", x.failed);
        assert_eq!(tree(&s), tree(&o));
        let (m1, m2) = (fs::metadata(s.join("p.png")).unwrap(), fs::metadata(o.join("p.png")).unwrap());
        assert_eq!(m1.modified().unwrap(), m2.modified().unwrap());
        assert!(Archive::open(&a, "pw").unwrap().restore(&|_| true, None).unwrap().failed.is_empty());
        // one file only
        let o2 = t.path().join("one");
        let y = Archive::open(&a, "pw").unwrap().restore(&|n| n == "a/x.gz", Some(&o2)).unwrap();
        assert_eq!(y.files, 1);
        assert_eq!(fs::read(o2.join("a/x.gz")).unwrap(), fs::read(s.join("a/x.gz")).unwrap());
    }
}

#[test]
fn wrong_passphrase_fails() {
    let (_t, s, a) = setup(1);
    pack(&s, &a, "pw", opts(Level::Fast)).unwrap();
    for p in ["Pw", "pw ", "", "other"] {
        assert!(Archive::open(&a, p).is_err());
    }
    assert!(zoflib_archive::container::has_passphrase(&a).unwrap());
}

#[test]
fn names_and_text_are_not_visible() {
    let (_t, s, a) = setup(2);
    pack(&s, &a, "pw", opts(Level::Fast)).unwrap();
    let d = fs::read(&a).unwrap();
    for w in [&b"fakejpeg"[..], b"alpha", b"prog.exe"] {
        assert!(!d.windows(w.len()).any(|x| x == w));
    }
}

#[test]
fn existing_archive_is_never_overwritten() {
    let (_t, s, a) = setup(3);
    fs::write(&a, b"keep").unwrap();
    assert!(pack(&s, &a, "pw", opts(Level::Fast)).is_err());
    assert_eq!(fs::read(&a).unwrap(), b"keep");
}

#[test]
fn flipped_bytes_never_return_bad_data() {
    let (t, s, a) = setup(4);
    pack(&s, &a, "pw", opts(Level::Fast)).unwrap();
    let good = fs::read(&a).unwrap();
    let want = tree(&s);
    let mut r = Rng(99);
    let (mut hit, mut partial) = (0, 0);
    for i in 0..40 {
        let mut d = good.clone();
        let at = match i { 0 => 9, 1 => 30, 2 => 70, 3 => d.len() - 4, 4 => d.len() - 12, 5 => d.len() - 60, _ => (r.n() % d.len() as u64) as usize };
        d[at] ^= 1 << (r.n() % 8);
        let b = t.path().join(format!("bad{i}.zofa"));
        fs::write(&b, &d).unwrap();
        let o = t.path().join(format!("o{i}"));
        match Archive::open(&b, "pw") {
            Err(_) => hit += 1,
            Ok(mut z) => {
                let x = z.restore(&|_| true, Some(&o)).unwrap();
                // whatever was written is right
                for (n, c) in tree(&o) {
                    assert_eq!(Some(&c), want.get(&n), "flip at {at}: {n}");
                }
                if !x.failed.is_empty() || !z.complete {
                    partial += 1;
                }
            }
        }
        let _ = fs::remove_dir_all(&o);
    }
    assert!(hit >= 2 && partial >= 20, "{hit} {partial}");
}

#[test]
fn cut_archive_reads_what_is_indexed_and_says_so() {
    let (t, s, a) = setup(5);
    pack(&s, &a, "pw", opts(Level::Fast)).unwrap();
    let d = fs::read(&a).unwrap();
    let want = tree(&s);
    for k in [30, 50, 80, 99] {
        let b = t.path().join("cut.zofa");
        fs::write(&b, &d[..d.len() * k / 100]).unwrap();
        let mut z = Archive::open(&b, "pw").unwrap();
        assert!(!z.complete);
        let o = t.path().join(format!("c{k}"));
        z.restore(&|_| true, Some(&o)).unwrap();
        for (n, c) in tree(&o) {
            assert_eq!(Some(&c), want.get(&n));
        }
    }
}

#[test]
fn resume_after_stop_and_torn_tail() {
    let (t, s, a) = setup(6);
    let r = pack(&s, &a, "pw", Options { stop_after: Some(2), in_flight: 1, group: 100_000, checkpoint: 1, ..opts(Level::Fast) }).unwrap();
    assert!(!r.complete);
    // half a segment at the end, as after pulling the cable
    let mut f = fs::OpenOptions::new().append(true).open(&a).unwrap();
    f.write_all(b"ZSG1\x01garbage").unwrap();
    drop(f);
    assert!(pack(&s, &a, "bad", Options { resume: true, ..opts(Level::Fast) }).is_err());
    fs::write(s.join("a/t.txt"), b"changed since").unwrap();
    let r = pack(&s, &a, "pw", Options { resume: true, ..opts(Level::Fast) }).unwrap();
    assert!(r.complete && r.resumed_files > 0, "{r:?}");
    let o = t.path().join("out");
    let x = extract(&a, &o);
    assert!(x.failed.is_empty(), "{:?}", x.failed);
    assert_eq!(tree(&s), tree(&o));
    assert!(pack(&s, &a, "pw", Options { resume: true, ..opts(Level::Fast) }).is_err());
}

#[test]
fn unsafe_paths_are_refused() {
    for p in ["../x", "a/../../x", "/x", "C:/x", "a\\..\\x", "a:stream", "", "a//b"] {
        assert!(safe(Path::new("out"), p).is_err(), "{p}");
    }
}

#[test]
fn archive_without_passphrase() {
    let (t, s, a) = setup(8);
    assert!(pack(&s, &a, "", opts(Level::Fast)).unwrap().complete);
    assert!(!zoflib_archive::container::has_passphrase(&a).unwrap());
    assert!(Archive::open(&a, "pw").is_err());
    let o = t.path().join("out");
    let x = Archive::open(&a, "").unwrap().restore(&|_| true, Some(&o)).unwrap();
    assert!(x.failed.is_empty());
    assert_eq!(tree(&s), tree(&o));
    // still checked for damage
    let mut d = fs::read(&a).unwrap();
    let n = d.len() / 2;
    d[n] ^= 4;
    fs::write(&a, &d).unwrap();
    let o2 = t.path().join("bad");
    let found = match Archive::open(&a, "") {
        Err(_) => true,
        Ok(mut z) => {
            let x = z.restore(&|_| true, Some(&o2)).unwrap();
            !x.failed.is_empty() || !z.complete
        }
    };
    assert!(found);
}

#[test]
fn add_only_keeps_deleted_files_and_old_versions() {
    let t = tempfile::tempdir().unwrap();
    let (s, a) = (t.path().join("src"), t.path().join("a.zofa"));
    fs::create_dir_all(s.join(".git/objects")).unwrap();
    fs::create_dir_all(s.join("plans")).unwrap();
    fs::write(s.join(".git/objects/x"), b"git data").unwrap();
    fs::write(s.join("plans/p.md"), b"first plan").unwrap();
    fs::write(s.join("mock.png"), png(&[7u8; 5000])).unwrap();
    let o = |l| Options { add: true, exclude: vec![".git".into()], ..opts(l) };
    assert!(pack(&s, &a, "pw", o(Level::Max)).unwrap().complete);
    let before = fs::read(&a).unwrap();
    let foot_at = u64::from_le_bytes(before[before.len() - 16..before.len() - 8].try_into().unwrap()) as usize;
    // change one, delete one, add one
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(s.join("plans/p.md"), b"second plan, longer").unwrap();
    fs::remove_file(s.join("mock.png")).unwrap();
    fs::write(s.join("new.md"), b"new").unwrap();
    let r = pack(&s, &a, "pw", o(Level::Max)).unwrap();
    assert!(r.complete && r.resumed_files == 0 && r.files == 2, "{r:?}");
    // the same again: nothing new
    let r = pack(&s, &a, "pw", o(Level::Fast)).unwrap();
    assert_eq!((r.files, r.resumed_files), (2, 2));
    let after = fs::read(&a).unwrap();
    assert_eq!(&after[..foot_at], &before[..foot_at], "what was written before must not change");
    let mut z = Archive::open(&a, "pw").unwrap();
    assert!(z.complete);
    let names: Vec<&str> = z.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(names, ["mock.png", "new.md", "plans/p.md"]);
    assert_eq!(z.older.len(), 1);
    z.include_older();
    let out = t.path().join("out");
    assert!(z.restore(&|_| true, Some(&out)).unwrap().failed.is_empty());
    let got = tree(&out);
    assert!(!got.keys().any(|k| k.contains(".git")));
    assert_eq!(got["plans/p.md"], b"second plan, longer");
    assert_eq!(got["mock.png"], png(&[7u8; 5000]));
    let old: Vec<_> = got.iter().filter(|(k, _)| k.starts_with("plans/p.md~")).collect();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].1, b"first plan");
    // a wrong passphrase cannot add
    assert!(pack(&s, &a, "other", o(Level::Fast)).is_err());
}

#[test]
fn stamps() {
    use zoflib_archive::restore::stamp;
    assert_eq!(stamp(0), "19700101-000000");
    assert_eq!(stamp(1_795_000_000), "20261118-110640");
    assert_eq!(stamp(951_782_400), "20000229-000000");
}
