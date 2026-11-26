//! On-disk container: header, key derivation, authenticated segments. See docs/FORMAT.md.
use anyhow::{bail, ensure, Context, Result};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
};

pub const MAGIC: &[u8; 8] = b"ZOFARCH1";
pub const HEADER_LEN: usize = 64;
pub const SEG_MAGIC: &[u8; 4] = b"ZSG1";
pub const FRAME_LEN: usize = 41;
const MAX_SEGMENT: u64 = 1 << 30;
const NO_PASS: u16 = 1;
const NO_PASS_CONTEXT: &str = "zoflib_archive v1 archive without passphrase";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    KeyCheck = 0,
    Data = 1,
    Index = 2,
    Footer = 3,
}
impl Kind {
    fn from(b: u8) -> Option<Kind> {
        [Kind::KeyCheck, Kind::Data, Kind::Index, Kind::Footer].into_iter().find(|k| *k as u8 == b)
    }
}

#[derive(Clone)]
pub struct Keys {
    cipher: XChaCha20Poly1305,
    header_hash: [u8; 32],
}

pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("random source: {e}"))?;
    Ok(b)
}

/// Header: magic 8, version u16, flags u16 (bit 0: no passphrase), argon2 memory KiB u32, passes u32, lanes u32, salt 16, zero padding.
pub fn new_header(no_pass: bool) -> Result<[u8; HEADER_LEN]> {
    let mut h = [0u8; HEADER_LEN];
    h[..8].copy_from_slice(MAGIC);
    h[8..10].copy_from_slice(&1u16.to_le_bytes());
    h[10..12].copy_from_slice(&(no_pass as u16).to_le_bytes());
    h[12..16].copy_from_slice(&(128u32 * 1024).to_le_bytes());
    h[16..20].copy_from_slice(&3u32.to_le_bytes());
    h[20..24].copy_from_slice(&1u32.to_le_bytes());
    h[24..40].copy_from_slice(&random::<16>()?);
    Ok(h)
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

pub fn derive(h: &[u8; HEADER_LEN], pass: &str) -> Result<Keys> {
    ensure!(&h[..8] == MAGIC, "not a zoflib archive");
    ensure!(u16::from_le_bytes([h[8], h[9]]) == 1, "archive version not supported");
    let flags = u16::from_le_bytes([h[10], h[11]]);
    ensure!(flags <= NO_PASS, "archive uses features this version does not know");
    if flags & NO_PASS != 0 {
        // integrity only: anyone can read it, but damage is still found
        ensure!(pass.is_empty(), "this archive has no passphrase; give an empty one");
        let key = blake3::derive_key(NO_PASS_CONTEXT, &h[24..40]);
        let cipher = XChaCha20Poly1305::new_from_slice(&key).unwrap();
        return Ok(Keys { cipher, header_hash: *blake3::hash(h).as_bytes() });
    }
    ensure!(!pass.is_empty(), "this archive needs its passphrase");
    let (m, t, p) = (le32(&h[12..]), le32(&h[16..]), le32(&h[20..]));
    ensure!((8192..=1 << 20).contains(&m) && (1..=16).contains(&t) && (1..=8).contains(&p), "header damaged: key settings out of range");
    let params = argon2::Params::new(m, t, p, Some(32)).map_err(|e| anyhow::anyhow!("argon2: {e}"))?;
    let mut key = zeroize::Zeroizing::new([0u8; 32]);
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(pass.as_bytes(), &h[24..40], &mut key[..])
        .map_err(|e| anyhow::anyhow!("argon2: {e}"))?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key[..]).unwrap();
    Ok(Keys { cipher, header_hash: *blake3::hash(h).as_bytes() })
}

fn aad(keys: &Keys, kind: Kind, id: u64) -> Vec<u8> {
    let mut a = keys.header_hash.to_vec();
    a.push(kind as u8);
    a.extend(id.to_le_bytes());
    a
}

/// Complete segment: frame (magic, kind, id, length, nonce) and ciphertext. Nonces are random, so ids may repeat after a resume.
pub fn seal(keys: &Keys, kind: Kind, id: u64, plain: &[u8]) -> Result<Vec<u8>> {
    let nonce = random::<24>()?;
    let a = aad(keys, kind, id);
    let ct = keys
        .cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad: &a })
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;
    ensure!((ct.len() as u64) < MAX_SEGMENT, "segment too large");
    let mut o = Vec::with_capacity(FRAME_LEN + ct.len());
    o.extend(SEG_MAGIC);
    o.push(kind as u8);
    o.extend(id.to_le_bytes());
    o.extend((ct.len() as u32).to_le_bytes());
    o.extend(nonce);
    o.extend(ct);
    Ok(o)
}

pub struct Frame {
    pub kind: Kind,
    pub id: u64,
    pub len: u64,
    nonce: [u8; 24],
}

pub fn read_frame(f: &mut File, at: u64) -> Result<Frame> {
    f.seek(SeekFrom::Start(at))?;
    let mut b = [0u8; FRAME_LEN];
    f.read_exact(&mut b).context("segment frame cut short")?;
    ensure!(&b[..4] == SEG_MAGIC, "no segment at {at}");
    let kind = Kind::from(b[4]).context("unknown segment kind")?;
    let len = le32(&b[13..]) as u64;
    ensure!((16..MAX_SEGMENT).contains(&len), "bad segment length");
    Ok(Frame { kind, id: u64::from_le_bytes(b[5..13].try_into().unwrap()), len, nonce: b[17..41].try_into().unwrap() })
}

pub fn read_segment(f: &mut File, keys: &Keys, at: u64) -> Result<(Frame, Vec<u8>)> {
    let fr = read_frame(f, at)?;
    let size = f.metadata()?.len();
    ensure!(at + FRAME_LEN as u64 + fr.len <= size, "segment at {at} cut short");
    let mut ct = vec![0u8; fr.len as usize];
    f.read_exact(&mut ct)?;
    let a = aad(keys, fr.kind, fr.id);
    match keys.cipher.decrypt(XNonce::from_slice(&fr.nonce), Payload { msg: &ct, aad: &a }) {
        Ok(p) => Ok((fr, p)),
        Err(_) => bail!("segment at {at} failed authentication (damaged or altered)"),
    }
}

pub fn open(f: &mut File, pass: &str) -> Result<Keys> {
    f.seek(SeekFrom::Start(0))?;
    let mut h = [0u8; HEADER_LEN];
    f.read_exact(&mut h).context("file too short to be an archive")?;
    let keys = derive(&h, pass)?;
    match read_segment(f, &keys, HEADER_LEN as u64) {
        Ok((fr, _)) if fr.kind == Kind::KeyCheck => Ok(keys),
        _ => bail!("wrong passphrase, or the start of the archive is damaged"),
    }
}

/// An empty passphrase makes an archive without one: checked for damage, but readable by anyone.
pub fn create(f: &mut File, pass: &str) -> Result<(Keys, u64)> {
    let h = new_header(pass.is_empty())?;
    let keys = derive(&h, pass)?;
    f.write_all(&h)?;
    let s = seal(&keys, Kind::KeyCheck, 0, b"zoflib")?;
    f.write_all(&s)?;
    Ok((keys, (HEADER_LEN + s.len()) as u64))
}

/// Whether the archive at `p` was made with a passphrase.
pub fn has_passphrase(p: &std::path::Path) -> Result<bool> {
    let mut h = [0u8; HEADER_LEN];
    File::open(p)?.read_exact(&mut h).context("file too short to be an archive")?;
    ensure!(&h[..8] == MAGIC, "not a zoflib archive");
    Ok(u16::from_le_bytes([h[10], h[11]]) & NO_PASS == 0)
}
