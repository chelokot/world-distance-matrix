use dm_wire::compact::{Axis, PARENTS, PARENT_WINDOW};
use rayon::prelude::*;

use crate::geo::{hilbert_index, Coord};
use crate::matrix::Endpoint;
use crate::weight::{dist_dm, time_ms, Weight, UNREACHABLE};

pub fn spatial_order(points: &[Coord]) -> Vec<u32> {
    let mut order: Vec<u32> = (0..points.len() as u32).collect();
    order.sort_by_key(|&index| (hilbert_index(points[index as usize]), index));
    order
}

fn unit_vector(coord: Coord) -> [f64; 3] {
    let (lat, lon) = (coord.lat_degrees().to_radians(), coord.lon_degrees().to_radians());
    [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()]
}

pub fn axis(order: Vec<u32>, points: &[Endpoint]) -> Axis {
    let units: Vec<[f64; 3]> = points.iter().map(|point| unit_vector(point.coord)).collect();
    let parents = (0..points.len())
        .into_par_iter()
        .map(|index| {
            let mut nearest: Vec<(f64, u32)> = Vec::with_capacity(PARENTS + 1);
            for earlier in (index.saturating_sub(PARENT_WINDOW - 1)..index).filter(|&earlier| points[earlier].snap.is_some()) {
                let closeness: f64 = units[index].iter().zip(&units[earlier]).map(|(a, b)| a * b).sum();
                let position = nearest.partition_point(|&(other, _)| other >= closeness);
                if position < PARENTS {
                    nearest.insert(position, (closeness, earlier as u32));
                    nearest.truncate(PARENTS);
                }
            }
            nearest.into_iter().map(|(_, earlier)| earlier).collect()
        })
        .collect();
    Axis { order, parents }
}

pub fn route_cell(weight: Weight) -> Option<[i64; 2]> {
    (weight < UNREACHABLE).then(|| [dist_dm(weight) as i64, time_ms(weight) as i64])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snap::{Placement, Snap};
    use crate::weight::pack;

    #[test]
    fn parents_are_the_nearest_earlier_routable_points() {
        let endpoint = |lon: f64, routable: bool| Endpoint {
            coord: Coord::from_degrees(0.0, lon),
            snap: routable.then_some(Snap { placement: Placement { chain: 0, fraction: 0.0 }, distance_m: 0.0 }),
        };
        let points: Vec<Endpoint> =
            [0.0, 5.0, 1.0, 4.2, 4.1].iter().zip([true, true, true, false, true]).map(|(&lon, routable)| endpoint(lon, routable)).collect();
        let parents = axis((0..5).collect(), &points).parents;
        assert_eq!(parents[0], Vec::<u32>::new());
        assert_eq!(parents[2], vec![0, 1]);
        assert_eq!(parents[4], vec![1, 2, 0]);
        let many: Vec<Endpoint> = (0..20).map(|k| endpoint(k as f64, true)).collect();
        assert_eq!(axis((0..20).collect(), &many).parents[19], (11..19).rev().collect::<Vec<u32>>());
        let far = PARENT_WINDOW + 5;
        let ring: Vec<Endpoint> = (0..far).map(|k| endpoint(if k == far - 1 { 0.0 } else { 0.001 * (k + 1) as f64 }, true)).collect();
        assert!(axis((0..far as u32).collect(), &ring).parents[far - 1].iter().all(|&parent| far - 1 - (parent as usize) < PARENT_WINDOW));
    }

    #[test]
    fn route_cells_keep_native_units() {
        assert_eq!(route_cell(pack(1_234_567, 98_765)), Some([98_765, 1_234_567]));
        assert_eq!(route_cell(UNREACHABLE), None);
    }
}
