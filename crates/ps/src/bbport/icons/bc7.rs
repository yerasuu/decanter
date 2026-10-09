//! A BC7 encoder for small UI art, mode 6 only: one RGBA line per 4x4
//! block, 7-bit endpoints with a p-bit each, 4-bit indices.
//!
//! One line per block is plenty for icons drawn on a single ramp (a dark
//! disc, a light rim, transparency); blocks that mix more colors than a
//! line can reach come out slightly tinted, which is why the icon art keeps
//! each block close to one ramp.

const WEIGHTS: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

pub type Pixel = [u8; 4];

fn interpolate(low: u8, high: u8, weight: u32) -> u8 {
    (((64 - weight) * u32::from(low) + weight * u32::from(high) + 32) >> 6) as u8
}

/// 7-bit endpoint for `value` with p-bit `pbit`, and the 8-bit value it expands to.
fn quantize(value: u8, pbit: u8) -> (u8, u8) {
    let seven = ((u16::from(value) + 1).saturating_sub(u16::from(pbit)) >> 1).min(127) as u8;
    (seven, seven << 1 | pbit)
}

struct Candidate {
    error: u64,
    low: [u8; 4],
    high: [u8; 4],
    pbits: [u8; 2],
    indices: [u8; 16],
}

/// 16 pixels, row by row, to one 16-byte block.
pub fn encode_block(pixels: &[Pixel; 16]) -> [u8; 16] {
    let mut lo = [u8::MAX; 4];
    let mut hi = [0u8; 4];
    for pixel in pixels {
        for channel in 0..4 {
            lo[channel] = lo[channel].min(pixel[channel]);
            hi[channel] = hi[channel].max(pixel[channel]);
        }
    }
    let mut best: Option<Candidate> = None;
    for p0 in 0..2 {
        for p1 in 0..2 {
            let (mut low, mut high) = ([0u8; 4], [0u8; 4]);
            let (mut low8, mut high8) = ([0u8; 4], [0u8; 4]);
            for channel in 0..4 {
                (low[channel], low8[channel]) = quantize(lo[channel], p0);
                (high[channel], high8[channel]) = quantize(hi[channel], p1);
            }
            let palette = WEIGHTS.map(|weight| [0, 1, 2, 3].map(|c| interpolate(low8[c], high8[c], weight)));
            let mut indices = [0u8; 16];
            let mut error = 0u64;
            for (slot, pixel) in pixels.iter().enumerate() {
                let (index, distance) = palette
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        let distance: u64 = (0..4).map(|c| u64::from(pixel[c].abs_diff(entry[c])).pow(2)).sum();
                        (index, distance)
                    })
                    .min_by_key(|&(_, distance)| distance)
                    .unwrap_or((0, 0));
                indices[slot] = index as u8;
                error += distance;
            }
            if best.as_ref().is_none_or(|best| error < best.error) {
                best = Some(Candidate {
                    error,
                    low,
                    high,
                    pbits: [p0, p1],
                    indices,
                });
            }
        }
    }
    let Some(mut best) = best else {
        return [0; 16];
    };
    // The first index has an implicit top bit of 0: swap the ends instead.
    if best.indices[0] >= 8 {
        std::mem::swap(&mut best.low, &mut best.high);
        best.pbits.swap(0, 1);
        for index in &mut best.indices {
            *index = 15 - *index;
        }
    }
    let mut bits = 0u128;
    let mut at = 0;
    let mut put = |value: u128, width: u32| {
        bits |= value << at;
        at += width;
    };
    put(1 << 6, 7);
    for channel in 0..4 {
        put(u128::from(best.low[channel]), 7);
        put(u128::from(best.high[channel]), 7);
    }
    put(u128::from(best.pbits[0]), 1);
    put(u128::from(best.pbits[1]), 1);
    put(u128::from(best.indices[0]), 3);
    for &index in &best.indices[1..] {
        put(u128::from(index), 4);
    }
    bits.to_le_bytes()
}

#[cfg(test)]
pub fn decode_mode6(block: &[u8; 16]) -> [Pixel; 16] {
    let bits = u128::from_le_bytes(*block);
    let mut at = 0;
    let mut take = |width: u32| {
        let value = (bits >> at) & ((1 << width) - 1);
        at += width;
        value as u8
    };
    assert_eq!(take(7), 1 << 6, "not mode 6");
    let mut ends = [[0u8; 4]; 2];
    for channel in 0..4 {
        ends[0][channel] = take(7);
        ends[1][channel] = take(7);
    }
    let (p0, p1) = (take(1), take(1));
    let low = ends[0].map(|value| value << 1 | p0);
    let high = ends[1].map(|value| value << 1 | p1);
    let mut out = [[0u8; 4]; 16];
    for (slot, pixel) in out.iter_mut().enumerate() {
        let index = take(if slot == 0 { 3 } else { 4 });
        *pixel = [0, 1, 2, 3].map(|c| interpolate(low[c], high[c], WEIGHTS[usize::from(index)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_two_color_block_comes_back_close() {
        let disc = [44, 44, 48, 255];
        let clear = [0, 0, 0, 0];
        let pixels: [Pixel; 16] = std::array::from_fn(|i| if i % 3 == 0 { clear } else { disc });
        let back = decode_mode6(&encode_block(&pixels));
        for (want, got) in pixels.iter().zip(&back) {
            for channel in 0..4 {
                assert!(want[channel].abs_diff(got[channel]) <= 2, "{want:?} vs {got:?}");
            }
        }
    }

    #[test]
    fn the_anchor_index_always_fits() {
        let pixels: [Pixel; 16] = std::array::from_fn(|i| if i == 0 { [255; 4] } else { [0; 4] });
        let back = decode_mode6(&encode_block(&pixels));
        assert_eq!(back[0], [255; 4]);
        assert_eq!(back[1], [0; 4]);
    }
}
