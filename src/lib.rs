//! Encrypted, compressed archives that adapt to the data and the disk.
pub mod container;
pub mod group;
mod mail;
pub mod pack;
mod pst;
pub mod restore;
pub mod streams;

use serde::{Deserialize, Serialize};

pub type Hash = [u8; 32];

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UnitRec {
    pub id: u64,
    pub hash: Hash,
    pub len: u64,
    pub group: u64,
    pub slot: u32,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GroupRec {
    pub id: u64,
    pub at: u64,
    pub len: u64,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// units joined in order
    Plain,
    /// data units, then the rest unit, then the layout unit
    Mail,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileRec {
    pub path: String,
    pub size: u64,
    /// seconds and nanoseconds since 1970
    pub mtime: (i64, u32),
    pub hash: Hash,
    pub form: Form,
    pub units: Vec<u64>,
    /// when this record was added, seconds since 1970 (the start of that pack or add run)
    pub added: i64,
}
/// One index segment. An archive's index is all of them together; each covers only groups already written before it.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Index {
    pub groups: Vec<GroupRec>,
    pub units: Vec<UnitRec>,
    pub files: Vec<FileRec>,
    pub dirs: Vec<String>,
}
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Footer {
    pub index_at: Vec<u64>,
    pub files: u64,
    pub bytes: u64,
}

pub fn postcard_vec<T: Serialize>(t: &T) -> anyhow::Result<Vec<u8>> {
    Ok(postcard::to_stdvec(t)?)
}
