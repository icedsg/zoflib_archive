use std::collections::{HashMap, HashSet};
pub const DEC: [u8; 256] = include!("tbl.txt");
pub fn enc_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    for i in 0..256 {
        t[DEC[i] as usize] = i as u8;
    }
    t
}
fn g16(d: &[u8], o: usize) -> Option<usize> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()) as usize)
}
fn g64(d: &[u8], o: usize) -> Option<u64> {
    d.get(o..o + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
}
/// standard polynomial, start 0, no final xor
pub fn crc(d: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new_with_initial(0xffff_ffff);
    h.update(d);
    !h.finalize()
}
pub fn sig(ib: u64, bid: u64) -> u16 {
    let x = (ib as u32) ^ (bid as u32);
    ((x >> 16) as u16) ^ (x as u16)
}
pub fn total(cb: usize) -> usize {
    (cb + 16 + 63) / 64 * 64
}
pub struct Blk {
    pub ib: usize,
    pub cb: usize,
    pub predicted: bool,
}
/// Groups of data blocks in logical order; each group is one contiguous stream.
pub fn plan(d: &[u8]) -> Option<Vec<Vec<Blk>>> {
    if d.len() < 1024 || &d[..4] != b"!BDN" || g16(d, 10)? != 23 || d[513] != 1 {
        return None;
    }
    let mut pages = vec![g64(d, 240)? as usize];
    let mut seen = HashSet::new();
    let mut map: HashMap<u64, (usize, usize)> = HashMap::new();
    while let Some(p) = pages.pop() {
        if !seen.insert(p) {
            continue;
        }
        let pg = match p.checked_add(512).and_then(|e| d.get(p..e)) {
            Some(x) => x,
            None => continue,
        };
        if pg[496] != 0x80 {
            continue;
        }
        let (n, sz, lvl) = (pg[488] as usize, pg[490] as usize, pg[491]);
        for i in 0..n {
            let o = i * sz;
            if o + 24 > 488 {
                break;
            }
            if lvl > 0 {
                pages.push(g64(pg, o + 16)? as usize);
            } else {
                map.insert(g64(pg, o)? & !1, (g64(pg, o + 8)? as usize, g16(pg, o + 16)?));
            }
        }
    }
    let mut all: Vec<(usize, usize, u64)> = map.iter().map(|(b, (i, c))| (*i, *c, *b)).collect();
    all.sort();
    let mut end = 0usize;
    let mut ok = HashSet::new();
    for (ib, cb, bid) in &all {
        if *ib < 1024 || *ib % 64 != 0 || *ib < end || *cb == 0 || *cb > 8176 {
            continue;
        }
        if ib + total(*cb) > d.len() {
            continue;
        }
        end = ib + total(*cb);
        ok.insert(*bid);
    }
    let children = |bid: u64| -> Vec<u64> {
        if !ok.contains(&bid) {
            return vec![];
        }
        let (ib, cb) = map[&bid];
        let b = &d[ib..ib + cb];
        if b.len() < 8 || b[0] != 1 {
            return vec![];
        }
        let n = g16(b, 2).unwrap();
        (0..n).filter_map(|i| g64(b, 8 + i * 8).map(|x| x & !1)).collect()
    };
    let mk = |bid: u64| {
        let (ib, cb) = map[&bid];
        let t = &d[ib + total(cb) - 16..ib + total(cb)];
        let predicted = g16(t, 0) == Some(cb)
            && g16(t, 2) == Some(sig(ib as u64, g64(t, 8).unwrap()) as usize)
            && u32::from_le_bytes(t[4..8].try_into().unwrap()) == crc(&d[ib..ib + cb]);
        Blk { ib, cb, predicted }
    };
    let mut used = HashSet::new();
    let mut groups: Vec<Vec<Blk>> = vec![];
    let ints: Vec<u64> = all.iter().map(|x| x.2).filter(|b| b & 2 != 0 && ok.contains(b)).collect();
    for lvl in [2u8, 1] {
        for &b in &ints {
            let (ib, _) = map[&b];
            if d[ib] != 1 || d[ib + 1] != lvl {
                continue;
            }
            let leaves: Vec<u64> = if lvl == 1 {
                children(b)
            } else {
                children(b).into_iter().flat_map(|x| children(x)).collect()
            };
            let mut g = vec![];
            for x in leaves {
                if x & 2 == 0 && ok.contains(&x) && used.insert(x) {
                    g.push(mk(x));
                }
            }
            if !g.is_empty() {
                groups.push(g);
            }
        }
    }
    let rest: Vec<u64> = all
        .iter()
        .map(|x| x.2)
        .filter(|b| b & 2 == 0 && ok.contains(b) && !used.contains(b))
        .collect();
    for c in rest.chunks(2048) {
        groups.push(c.iter().map(|b| mk(*b)).collect());
    }
    Some(groups)
}
