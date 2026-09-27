use bytemuck::{Pod, Zeroable};

pub type Weight = u64;

pub const UNREACHABLE: Weight = 1 << 62;

pub const fn pack(time_ms: u32, dist_dm: u32) -> Weight {
    ((time_ms as u64) << 32) | dist_dm as u64
}

pub const fn time_ms(weight: Weight) -> u32 {
    (weight >> 32) as u32
}

pub const fn dist_dm(weight: Weight) -> u32 {
    weight as u32
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

pub const UNREACHABLE_VALUE: u32 = u32::MAX;

impl RouteValue {
    pub const UNREACHABLE: RouteValue = RouteValue { distance_m: UNREACHABLE_VALUE, duration_s: UNREACHABLE_VALUE };

    pub fn from_weight(weight: Weight) -> Self {
        if weight >= UNREACHABLE {
            return Self::UNREACHABLE;
        }
        Self { distance_m: (dist_dm(weight) + 5) / 10, duration_s: (time_ms(weight) + 500) / 1000 }
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
        assert!(UNREACHABLE + pack(u32::MAX >> 1, u32::MAX >> 1) >= UNREACHABLE);
    }

    #[test]
    fn route_values_round_to_nearest() {
        assert_eq!(RouteValue::from_weight(pack(1499, 14)), RouteValue { distance_m: 1, duration_s: 1 });
        assert_eq!(RouteValue::from_weight(pack(1500, 15)), RouteValue { distance_m: 2, duration_s: 2 });
        assert_eq!(RouteValue::from_weight(UNREACHABLE), RouteValue::UNREACHABLE);
    }
}
