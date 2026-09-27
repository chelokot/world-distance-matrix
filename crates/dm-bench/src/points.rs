use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use dm_core::geo::Coord;
use osmpbf::{BlobDecode, BlobReader};
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rayon::prelude::*;

#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub min: Coord,
    pub max: Coord,
}

impl Region {
    pub fn parse(bbox: &str) -> Result<Self> {
        let v: Vec<f64> = bbox.split(',').map(|p| p.trim().parse()).collect::<Result<_, _>>().context("bbox must be min_lon,min_lat,max_lon,max_lat")?;
        anyhow::ensure!(v.len() == 4, "bbox must have four numbers");
        Ok(Self { min: Coord::from_degrees(v[1], v[0]), max: Coord::from_degrees(v[3], v[2]) })
    }

    pub fn contains(&self, c: Coord) -> bool {
        (self.min.lat..=self.max.lat).contains(&c.lat) && (self.min.lon..=self.max.lon).contains(&c.lon)
    }
}

fn blocks(path: &Path) -> Result<impl ParallelIterator<Item = osmpbf::Blob>> {
    let reader = BlobReader::from_path(path).with_context(|| format!("opening {}", path.display()))?;
    Ok(reader.par_bridge().map(|blob| blob.expect("readable blob")))
}

pub fn extract(input: &Path, region: Option<Region>, count: usize, seed: u64) -> Result<Vec<Coord>> {
    let building_first_nodes: Vec<i64> = blocks(input)?
        .flat_map_iter(|blob| {
            let mut ids = Vec::new();
            if let Ok(BlobDecode::OsmData(block)) = blob.decode() {
                for group in block.groups() {
                    for way in group.ways() {
                        if way.tags().any(|(k, _)| k == "building") {
                            if let Some(first) = way.refs().next() {
                                ids.push(first);
                            }
                        }
                    }
                }
            }
            ids
        })
        .collect();
    let mut wanted = building_first_nodes;
    wanted.par_sort_unstable();
    wanted.dedup();
    let mut points: Vec<(i64, Coord)> = blocks(input)?
        .flat_map_iter(|blob| {
            let mut found = Vec::new();
            if let Ok(BlobDecode::OsmData(block)) = blob.decode() {
                for group in block.groups() {
                    for node in group.dense_nodes() {
                        let coord = Coord { lat: node.decimicro_lat(), lon: node.decimicro_lon() };
                        let addressed = || node.tags().any(|(k, _)| k == "addr:housenumber");
                        if wanted.binary_search(&node.id()).is_ok() || addressed() {
                            found.push((node.id(), coord));
                        }
                    }
                }
            }
            found
        })
        .filter(|(_, c)| region.is_none_or(|r| r.contains(*c)))
        .collect();
    points.par_sort_unstable_by_key(|(id, _)| *id);
    points.dedup_by_key(|(id, _)| *id);
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    points.shuffle(&mut rng);
    points.truncate(count);
    Ok(points.into_iter().map(|(_, c)| c).collect())
}

pub fn write_csv(path: &Path, points: &[Coord]) -> Result<()> {
    let mut out = BufWriter::new(std::fs::File::create(path)?);
    for p in points {
        writeln!(out, "{:.7},{:.7}", p.lat_degrees(), p.lon_degrees())?;
    }
    out.flush()?;
    Ok(())
}

pub fn read_csv(path: &Path) -> Result<Vec<Coord>> {
    let reader = BufReader::new(std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?);
    reader
        .lines()
        .map(|line| {
            let line = line?;
            let (lat, lon) = line.split_once(',').context("expected lat,lon")?;
            Ok(Coord::from_degrees(lat.trim().parse()?, lon.trim().parse()?))
        })
        .collect()
}
