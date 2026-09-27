use bytemuck::{Pod, Zeroable};

pub type Weight = u64;

const DIST_BITS: u32 = 29;

pub const UNREACHABLE: Weight = 1 << 62;

pub const fn pack(time_ms: u32, dist_dm: u32) -> Weight {
    debug_assert!(dist_dm < 1 << DIST_BITS);
    ((time_ms as u64) << DIST_BITS) | dist_dm as u64
}

pub const fn time_ms(weight: Weight) -> u64 {
    weight >> DIST_BITS
}

pub const fn dist_dm(weight: Weight) -> u32 {
    (weight & ((1 << DIST_BITS) - 1)) as u32
}

pub const NOT_TRAVERSABLE: u32 = u32::MAX;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct ChainCost {
    pub dist_dm: u32,
    pub forward_time_ms: u32,
    pub backward_time_ms: u32,
}

impl ChainCost {
    pub fn forward(&self) -> Option<Weight> {
        (self.forward_time_ms != NOT_TRAVERSABLE).then(|| pack(self.forward_time_ms, self.dist_dm))
    }

    pub fn backward(&self) -> Option<Weight> {
        (self.backward_time_ms != NOT_TRAVERSABLE).then(|| pack(self.backward_time_ms, self.dist_dm))
    }
}

pub fn scale(weight: Weight, fraction: f64) -> Weight {
    pack((time_ms(weight) as f64 * fraction).round() as u32, (dist_dm(weight) as f64 * fraction).round() as u32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteValue {
    pub distance_m: u32,
    pub duration_s: u32,
}

pub const UNREACHABLE_VALUE: u32 = dm_wire::NO_ROUTE;

impl RouteValue {
    pub const UNREACHABLE: RouteValue = RouteValue { distance_m: UNREACHABLE_VALUE, duration_s: UNREACHABLE_VALUE };

    pub fn from_weight(weight: Weight) -> Self {
        if weight >= UNREACHABLE {
            return Self::UNREACHABLE;
        }
        Self { distance_m: (dist_dm(weight) + 5) / 10, duration_s: ((time_ms(weight) + 500) / 1000) as u32 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing_is_lexicographic() {
        assert!(pack(10, 500) < pack(11, 0));
        assert!(pack(10, 499) < pack(10, 500));
        assert_eq!(pack(3, 4) + pack(5, 6), pack(8, 10));
        assert_eq!(time_ms(pack(7, 9)), 7);
        assert_eq!(dist_dm(pack(7, 9)), 9);
    }

    #[test]
    fn unreachable_sums_do_not_overflow() {
        assert!(UNREACHABLE.checked_add(UNREACHABLE).is_some());
        assert!(UNREACHABLE + pack(u32::MAX, (1 << DIST_BITS) - 1) >= UNREACHABLE);
    }

    #[test]
    fn intercontinental_routes_stay_reachable() {
        let week_and_11_500_km = pack(7 * 86_400_000, 115_000_000);
        let cape_town_to_magadan = week_and_11_500_km + week_and_11_500_km;
        assert!(cape_town_to_magadan < UNREACHABLE);
        assert_eq!(RouteValue::from_weight(cape_town_to_magadan), RouteValue { distance_m: 23_000_000, duration_s: 14 * 86_400 });
    }

    #[test]
    fn route_values_round_to_nearest() {
        assert_eq!(RouteValue::from_weight(pack(1499, 14)), RouteValue { distance_m: 1, duration_s: 1 });
        assert_eq!(RouteValue::from_weight(pack(1500, 15)), RouteValue { distance_m: 2, duration_s: 2 });
        assert_eq!(RouteValue::from_weight(UNREACHABLE), RouteValue::UNREACHABLE);
    }
}
