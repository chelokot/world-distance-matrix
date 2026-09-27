const PROBABILITY_BITS: u32 = 12;
const ONE: u16 = 1 << PROBABILITY_BITS;
const ADAPTATION_SHIFT: u32 = 5;
const TOP: u32 = 1 << 24;

#[derive(Clone, Copy)]
pub struct Bit(u16);

impl Bit {
    pub const UNKNOWN: Bit = Bit(ONE / 2);

    fn bound(self, range: u32) -> u32 {
        (range >> PROBABILITY_BITS) * self.0 as u32
    }

    fn update(&mut self, bit: bool) {
        let probability = self.0 as u32;
        let rise = probability + ((ONE as u32 - probability) >> ADAPTATION_SHIFT);
        let fall = probability - (probability >> ADAPTATION_SHIFT);
        self.0 = select(bit, fall, rise) as u16;
    }
}

fn select(condition: bool, when_true: u32, when_false: u32) -> u32 {
    when_false ^ ((when_true ^ when_false) & 0u32.wrapping_sub(condition as u32))
}

pub trait Coder {
    fn bit(&mut self, model: &mut Bit, bit: bool) -> bool;

    fn raw(&mut self, value: u64, bits: u32) -> u64;

    fn symbol<const LEAVES: usize>(&mut self, tree: &mut [Bit; LEAVES], value: usize) -> usize {
        let mut node = 1;
        for shift in (0..LEAVES.trailing_zeros()).rev() {
            node = 2 * node + self.bit(&mut tree[node], (value >> shift) & 1 == 1) as usize;
        }
        node - LEAVES
    }

    fn magnitude(&mut self, tree: &mut [Bit; 64], value: u64) -> u64 {
        match self.symbol(tree, significant_bits(value) as usize) as u32 {
            0 => 0,
            bits => 1 << (bits - 1) | self.raw(value, bits - 1),
        }
    }
}

pub fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

pub fn unzigzag(value: u64) -> i64 {
    (value >> 1) as i64 ^ -((value & 1) as i64)
}

pub fn significant_bits(value: u64) -> u32 {
    u64::BITS - value.leading_zeros()
}

fn low_bits(value: u64, bits: u32) -> u64 {
    value & ((1u64 << bits) - 1)
}

pub struct Encoder {
    low: u64,
    range: u32,
    cache: u8,
    pending: u32,
    coded: Vec<u8>,
    raw: Vec<u8>,
    raw_bits: u64,
    raw_filled: u32,
}

impl Default for Encoder {
    fn default() -> Self {
        Self { low: 0, range: u32::MAX, cache: 0, pending: 1, coded: Vec::new(), raw: Vec::new(), raw_bits: 0, raw_filled: 0 }
    }
}

impl Encoder {
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || self.low >> 32 != 0 {
            let carry = (self.low >> 32) as u8;
            let mut byte = self.cache;
            for _ in 0..self.pending {
                self.coded.push(byte.wrapping_add(carry));
                byte = 0xFF;
            }
            self.pending = 0;
            self.cache = (self.low >> 24) as u8;
        }
        self.pending += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    fn push_raw(&mut self, value: u64, bits: u32) {
        self.raw_bits |= value << self.raw_filled;
        self.raw_filled += bits;
        while self.raw_filled >= 8 {
            self.raw.push(self.raw_bits as u8);
            self.raw_bits >>= 8;
            self.raw_filled -= 8;
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        if self.raw_filled > 0 {
            self.raw.push(self.raw_bits as u8);
        }
        [(self.coded.len() as u32).to_le_bytes(), (self.raw.len() as u32).to_le_bytes()].concat().into_iter().chain(self.coded).chain(self.raw).collect()
    }
}

impl Coder for Encoder {
    fn bit(&mut self, model: &mut Bit, bit: bool) -> bool {
        let bound = model.bound(self.range);
        if bit {
            self.low += bound as u64;
            self.range -= bound;
        } else {
            self.range = bound;
        }
        model.update(bit);
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
        bit
    }

    fn raw(&mut self, value: u64, bits: u32) -> u64 {
        let value = low_bits(value, bits);
        let low = bits.min(32);
        self.push_raw(low_bits(value, low), low);
        self.push_raw(value >> low, bits - low);
        value
    }
}

pub const SECTION_HEADER_LEN: usize = 8;

pub fn section_len(header: &[u8]) -> usize {
    let [coded, raw] = crate::words(&header[..SECTION_HEADER_LEN])[..] else { unreachable!("two words") };
    SECTION_HEADER_LEN + coded as usize + raw as usize
}

pub struct Decoder<'a> {
    coded: &'a [u8],
    position: usize,
    range: u32,
    code: u32,
    raw: &'a [u8],
    raw_position: usize,
    raw_bits: u64,
    raw_available: u32,
}

impl<'a> Decoder<'a> {
    pub fn new(section: &'a [u8]) -> Self {
        let coded_len = crate::words(&section[..4])[0] as usize;
        let (coded, raw) = section[SECTION_HEADER_LEN..].split_at(coded_len);
        let mut decoder = Self { coded, position: 1, range: u32::MAX, code: 0, raw, raw_position: 0, raw_bits: 0, raw_available: 0 };
        for _ in 0..4 {
            decoder.code = decoder.code << 8 | decoder.next_coded();
        }
        decoder
    }

    fn next_coded(&mut self) -> u32 {
        let byte = self.coded.get(self.position).copied().unwrap_or(0);
        self.position += 1;
        byte as u32
    }

    fn take_raw(&mut self, bits: u32) -> u64 {
        while self.raw_available < bits {
            self.raw_bits |= (self.raw.get(self.raw_position).copied().unwrap_or(0) as u64) << self.raw_available;
            self.raw_position += 1;
            self.raw_available += 8;
        }
        let value = low_bits(self.raw_bits, bits);
        self.raw_bits >>= bits;
        self.raw_available -= bits;
        value
    }
}

impl Coder for Decoder<'_> {
    fn bit(&mut self, model: &mut Bit, _: bool) -> bool {
        let bound = model.bound(self.range);
        let bit = self.code >= bound;
        self.code -= select(bit, bound, 0);
        self.range = select(bit, self.range - bound, bound);
        model.update(bit);
        while self.range < TOP {
            self.range <<= 8;
            self.code = self.code << 8 | self.next_coded();
        }
        bit
    }

    fn raw(&mut self, _: u64, bits: u32) -> u64 {
        let low = bits.min(32);
        let value = self.take_raw(low);
        value | self.take_raw(bits - low) << low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_all(coder: &mut impl Coder, items: &[(bool, usize, u64, u64)]) -> Vec<(bool, usize, u64, u64)> {
        let (mut flag, mut symbols, mut magnitudes) = (Bit::UNKNOWN, [Bit::UNKNOWN; 64], [Bit::UNKNOWN; 64]);
        items
            .iter()
            .map(|&(f, s, m, r)| (coder.bit(&mut flag, f), coder.symbol(&mut symbols, s), coder.magnitude(&mut magnitudes, m), coder.raw(r, 63)))
            .collect()
    }

    #[test]
    fn round_trips_skewed_bits_symbols_and_wide_magnitudes() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let items: Vec<_> = (0..20_000).map(|_| (next() % 100 < 97, (next() % 64) as usize, next() >> (1 + next() % 63), next() >> 1)).collect();
        let mut encoder = Encoder::default();
        assert!(code_all(&mut encoder, &items) == items);
        let section = encoder.finish();
        assert_eq!(section_len(&section), section.len());
        assert!(code_all(&mut Decoder::new(&section), &vec![(false, 0, 0, 0); items.len()]) == items);
    }
}
