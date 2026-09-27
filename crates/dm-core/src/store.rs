use std::fs::File;
use std::io::{BufWriter, Write};
use std::marker::PhantomData;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use bytemuck::Pod;
use memmap2::{Mmap, MmapOptions};
use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub profile: String,
    pub source: String,
    pub built_at_unix: u64,
    pub node_count: u32,
    pub ch_arc_count: u64,
    pub chain_count: u32,
    pub major_chain_count: u32,
    pub geometry_point_count: u64,
    pub arrays: Vec<ArrayEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArrayEntry {
    pub name: String,
    pub element_size: usize,
    pub len: u64,
}

pub struct ArrayWriter {
    dir: PathBuf,
    entries: Vec<ArrayEntry>,
}

impl ArrayWriter {
    pub fn create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir: dir.to_path_buf(), entries: Vec::new() })
    }

    pub fn write<T: Pod>(&mut self, name: &str, data: &[T]) -> Result<()> {
        let path = self.dir.join(format!("{name}.bin"));
        let mut out = BufWriter::with_capacity(1 << 22, File::create(&path).with_context(|| format!("creating {}", path.display()))?);
        out.write_all(bytemuck::cast_slice(data))?;
        out.into_inner()?.sync_all()?;
        self.entries.push(ArrayEntry { name: name.to_string(), element_size: size_of::<T>(), len: data.len() as u64 });
        Ok(())
    }

    pub fn finish(self, mut manifest: Manifest) -> Result<()> {
        manifest.arrays = self.entries;
        let tmp = self.dir.join("manifest.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&manifest)?)?;
        std::fs::rename(tmp, self.dir.join("manifest.json"))?;
        Ok(())
    }
}

pub enum Array<T: Pod> {
    Mapped { map: Mmap, len: usize, element: PhantomData<T> },
    Owned(Vec<T>),
}

impl<T: Pod> From<Vec<T>> for Array<T> {
    fn from(values: Vec<T>) -> Self {
        Array::Owned(values)
    }
}

impl<T: Pod> Deref for Array<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        match self {
            Array::Mapped { map, len, .. } => bytemuck::cast_slice(&map[..len * size_of::<T>()]),
            Array::Owned(values) => values,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Residency {
    OnDemand,
    Prefault,
    Locked,
}

pub struct ArrayReader {
    dir: PathBuf,
    manifest: Manifest,
    residency: Residency,
}

impl ArrayReader {
    pub fn open(dir: &Path, residency: Residency) -> Result<Self> {
        let path = dir.join("manifest.json");
        let manifest: Manifest = serde_json::from_slice(&std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?)
            .with_context(|| format!("parsing {}", path.display()))?;
        if manifest.format_version != FORMAT_VERSION {
            bail!("data format version {} is not supported (expected {FORMAT_VERSION})", manifest.format_version);
        }
        Ok(Self { dir: dir.to_path_buf(), manifest, residency })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn read<T: Pod>(&self, name: &str) -> Result<Array<T>> {
        let entry = self.manifest.arrays.iter().find(|entry| entry.name == name).with_context(|| format!("array {name} missing from manifest"))?;
        ensure!(entry.element_size == size_of::<T>(), "array {name} has element size {} but {} was expected", entry.element_size, size_of::<T>());
        let len = entry.len as usize;
        if len == 0 {
            return Ok(Array::Owned(Vec::new()));
        }
        let path = self.dir.join(format!("{name}.bin"));
        let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let expected_bytes = (len * size_of::<T>()) as u64;
        ensure!(file.metadata()?.len() == expected_bytes, "{} has unexpected size", path.display());
        let mut options = MmapOptions::new();
        if self.residency != Residency::OnDemand {
            options.populate();
        }
        let map = unsafe { options.map(&file) }.with_context(|| format!("mapping {}", path.display()))?;
        if self.residency == Residency::Locked {
            map.lock().with_context(|| format!("locking {} in memory (is the memlock limit high enough?)", path.display()))?;
        }
        Ok(Array::Mapped { map, len, element: PhantomData })
    }
}
