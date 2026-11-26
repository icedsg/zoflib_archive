//! Outlook mail files (PST/OST, Unicode, byte-substitution mode): split into logical-order data units, the rest, and a layout.
use crate::pst;
use crate::streams::pv;
use anyhow::{ensure, Context, Result};

pub struct Split {
    pub data: Vec<Vec<u8>>,
    pub rest: Vec<u8>,
    pub layout: Vec<u8>,
}

pub fn split(d: &[u8]) -> Option<Split> {
    let g = pst::plan(d)?;
    let (mut data, mut layout, mut rest) = (vec![], vec![], vec![]);
    let mut cut = vec![];
    // groups are limited so that one unit stays small
    let groups: Vec<&[pst::Blk]> = g.iter().flat_map(|x| x.chunks(4096)).collect();
    pv(&mut layout, groups.len() as u64);
    let mut last = 0i64;
    for gr in groups {
        pv(&mut layout, gr.len() as u64);
        let mut u = vec![];
        for b in gr {
            let dl = b.ib as i64 / 64 - last;
            last = b.ib as i64 / 64;
            pv(&mut layout, ((dl << 1) ^ (dl >> 63)) as u64);
            pv(&mut layout, (b.cb as u64) << 1 | b.predicted as u64);
            u.extend(d[b.ib..b.ib + b.cb].iter().map(|x| pst::DEC[*x as usize]));
            cut.push((b.ib, b.cb, b.predicted));
        }
        data.push(u);
    }
    cut.sort();
    let mut p = 0;
    for (ib, cb, pr) in cut {
        rest.extend_from_slice(&d[p..ib]);
        p = ib + cb;
        if pr {
            let e = ib + pst::total(cb);
            rest.extend_from_slice(&d[p..e - 16]);
            p = e - 8;
        }
    }
    rest.extend_from_slice(&d[p..]);
    Some(Split { data, rest, layout })
}

struct L<'a>(&'a [u8], usize);
impl L<'_> {
    fn v(&mut self) -> Result<usize> {
        let (mut v, mut s) = (0u64, 0);
        loop {
            let c = *self.0.get(self.1).context("mail layout cut short")?;
            self.1 += 1;
            ensure!(s < 63, "bad number");
            v |= ((c & 127) as u64) << s;
            if c < 128 {
                return Ok(v as usize);
            }
            s += 7;
        }
    }
}

pub fn join(data: &[Vec<u8>], rest: &[u8], layout: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut l = L(layout, 0);
    let mut d = vec![0u8; size];
    let enc = pst::enc_table();
    let mut last = 0i64;
    let mut bl = vec![];
    ensure!(l.v()? == data.len(), "mail layout does not match its data");
    for u in data {
        let n = l.v()?;
        let mut q = 0;
        for _ in 0..n {
            let z = l.v()? as u64;
            last += ((z >> 1) as i64) ^ -((z & 1) as i64);
            let x = l.v()?;
            let (ib, cb) = (usize::try_from(last)? * 64, x >> 1);
            ensure!(cb <= 8176 && ib + pst::total(cb) <= size && q + cb <= u.len(), "mail layout out of range");
            for j in 0..cb {
                d[ib + j] = enc[u[q + j] as usize];
            }
            q += cb;
            bl.push((ib, cb, x & 1 == 1));
        }
    }
    bl.sort();
    let (mut p, mut rp) = (0, 0);
    let mut cp = |d: &mut Vec<u8>, x: usize, y: usize| -> Result<()> {
        ensure!(x <= y && y <= d.len() && rp + y - x <= rest.len(), "mail layout out of range");
        d[x..y].copy_from_slice(&rest[rp..rp + y - x]);
        rp += y - x;
        Ok(())
    };
    for &(ib, cb, pr) in &bl {
        cp(&mut d, p, ib)?;
        p = ib + cb;
        if pr {
            let e = ib + pst::total(cb);
            cp(&mut d, p, e - 16)?;
            p = e - 8;
        }
    }
    cp(&mut d, p, size)?;
    for (ib, cb, pr) in bl {
        if pr {
            let e = ib + pst::total(cb);
            let c = pst::crc(&d[ib..ib + cb]);
            let bid = u64::from_le_bytes(d[e - 8..e].try_into().unwrap());
            d[e - 16..e - 14].copy_from_slice(&(cb as u16).to_le_bytes());
            d[e - 14..e - 12].copy_from_slice(&pst::sig(ib as u64, bid).to_le_bytes());
            d[e - 12..e - 8].copy_from_slice(&c.to_le_bytes());
        }
    }
    Ok(d)
}
