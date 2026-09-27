use anyhow::{ensure, Result};
use dm_core::geo::Coord;
use dm_core::weight::UNREACHABLE_VALUE;
use serde::Deserialize;

use crate::verify::Matrix;

#[derive(Deserialize)]
pub struct OsrmTable {
    code: String,
    durations: Vec<Vec<Option<f64>>>,
    distances: Vec<Vec<Option<f64>>>,
}

pub fn osrm_table(base_url: &str, coords: &[Coord]) -> Result<OsrmTable> {
    let locations: Vec<String> = coords.iter().map(|c| format!("{:.7},{:.7}", c.lon_degrees(), c.lat_degrees())).collect();
    let url = format!("{base_url}/table/v1/driving/{}?annotations=duration,distance", locations.join(";"));
    let table: OsrmTable = reqwest::blocking::get(url)?.json()?;
    ensure!(table.code == "Ok", "OSRM answered {}", table.code);
    Ok(table)
}

pub fn worst(coords: &[Coord], ours: &Matrix, reference: &OsrmTable, count: usize) {
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (i, origin) in coords.iter().enumerate() {
        let unreachable_row = (0..ours.size).filter(|&j| j != i && ours.at(i, j).duration_s == UNREACHABLE_VALUE).count();
        if unreachable_row > ours.size / 2 {
            println!("point {i} ({:.6},{:.6}) unreachable from {unreachable_row} others in ours", origin.lat_degrees(), origin.lon_degrees());
        }
        for j in (0..ours.size).filter(|&j| j != i) {
            if let (Some(osrm), mine) = (reference.durations[i][j], ours.at(i, j).duration_s) {
                if mine != UNREACHABLE_VALUE && osrm > 60.0 {
                    pairs.push((mine as f64 - osrm, i, j));
                }
            }
        }
    }
    pairs.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()));
    for &(delta, i, j) in pairs.iter().take(count) {
        println!(
            "{:.6},{:.6} -> {:.6},{:.6}: ours {} s / {} m, osrm {:.0} s / {:.0} m (delta {delta:.0} s)",
            coords[i].lat_degrees(),
            coords[i].lon_degrees(),
            coords[j].lat_degrees(),
            coords[j].lon_degrees(),
            ours.at(i, j).duration_s,
            ours.at(i, j).distance_m,
            reference.durations[i][j].unwrap_or(-1.0),
            reference.distances[i][j].unwrap_or(-1.0)
        );
    }
}

#[derive(Default)]
pub struct Comparison {
    duration_ratios: Vec<f64>,
    distance_ratios: Vec<f64>,
    duration_abs_errors: Vec<f64>,
    only_ours: usize,
    only_osrm: usize,
    both_unreachable: usize,
}

fn quantile(values: &mut [f64], q: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * q).round() as usize]
}

impl Comparison {
    pub fn add(&mut self, ours: &Matrix, reference: &OsrmTable) {
        for i in 0..ours.size {
            for j in (0..ours.size).filter(|&j| j != i) {
                let mine = ours.at(i, j);
                match (mine.duration_s != UNREACHABLE_VALUE, reference.durations[i][j], reference.distances[i][j]) {
                    (true, Some(duration), Some(distance)) if duration > 60.0 && distance > 500.0 => {
                        self.duration_ratios.push(mine.duration_s as f64 / duration);
                        self.distance_ratios.push(mine.distance_m as f64 / distance);
                        self.duration_abs_errors.push((mine.duration_s as f64 - duration).abs());
                    }
                    (true, Some(_), Some(_)) => {}
                    (true, _, _) => self.only_ours += 1,
                    (false, Some(_), _) => self.only_osrm += 1,
                    (false, None, _) => self.both_unreachable += 1,
                }
            }
        }
    }

    pub fn print(mut self) {
        let pairs = self.duration_ratios.len();
        let within = |ratios: &[f64], tolerance: f64| 100.0 * ratios.iter().filter(|r| (*r - 1.0).abs() <= tolerance).count() as f64 / ratios.len() as f64;
        println!("compared pairs: {pairs} (pairs under 60 s or 500 m excluded)");
        println!("reachability: only ours {}, only OSRM {}, neither {}", self.only_ours, self.only_osrm, self.both_unreachable);
        for (name, ratios) in [("duration", &mut self.duration_ratios), ("distance", &mut self.distance_ratios)] {
            let (w2, w5, w10) = (within(ratios, 0.02), within(ratios, 0.05), within(ratios, 0.10));
            println!(
                "{name:>9} ours/osrm: p1 {:.3} p10 {:.3} median {:.3} p90 {:.3} p99 {:.3} | within 2% {w2:.1}%, 5% {w5:.1}%, 10% {w10:.1}%",
                quantile(ratios, 0.01),
                quantile(ratios, 0.10),
                quantile(ratios, 0.5),
                quantile(ratios, 0.90),
                quantile(ratios, 0.99)
            );
        }
        println!(
            "duration absolute error: median {:.0} s, p90 {:.0} s, p99 {:.0} s",
            quantile(&mut self.duration_abs_errors, 0.5),
            quantile(&mut self.duration_abs_errors, 0.9),
            quantile(&mut self.duration_abs_errors, 0.99)
        );
    }
}
