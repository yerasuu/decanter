//! PS4 texture layout for block-compressed formats: blocks are grouped in
//! 8x8 tiles laid out row by row, each tile in Morton (Z) order. Checked
//! against Bloodborne's own BC7 and BC1 menu textures, which decode clean
//! with it.

fn morton(x: usize, y: usize) -> usize {
    (0..8).fold(0, |code, bit| {
        code | ((x >> bit) & 1) << (2 * bit) | ((y >> bit) & 1) << (2 * bit + 1)
    })
}

/// Where linear block `(bx, by)` sits in the tiled data, in blocks.
fn tiled_index(bx: usize, by: usize, blocks_wide: usize) -> usize {
    let tiles_wide = blocks_wide.div_ceil(8).max(1);
    ((by / 8) * tiles_wide + bx / 8) * 64 + morton(bx % 8, by % 8)
}

/// Linear (row-major) blocks to the PS4 layout. `width` and `height` are
/// in pixels; `block` is 16 for BC7, 8 for BC1.
pub fn tile(linear: &[u8], width: usize, height: usize, block: usize) -> Vec<u8> {
    let (wide, high) = (width / 4, height / 4);
    let mut out = vec![0; linear.len()];
    for by in 0..high {
        for bx in 0..wide {
            let from = (by * wide + bx) * block;
            let to = tiled_index(bx, by, wide) * block;
            out[to..to + block].copy_from_slice(&linear[from..from + block]);
        }
    }
    out
}

pub fn detile(tiled: &[u8], width: usize, height: usize, block: usize) -> Vec<u8> {
    let (wide, high) = (width / 4, height / 4);
    let mut out = vec![0; tiled.len()];
    for by in 0..high {
        for bx in 0..wide {
            let from = tiled_index(bx, by, wide) * block;
            let to = (by * wide + bx) * block;
            out[to..to + block].copy_from_slice(&tiled[from..from + block]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiling_round_trips_and_moves_blocks() {
        let (width, height, block) = (64, 32, 16);
        let linear: Vec<u8> = (0..width / 4 * height / 4 * block)
            .map(|byte| (byte / block) as u8)
            .collect();
        let tiled = tile(&linear, width, height, block);
        assert_ne!(tiled, linear);
        assert_eq!(detile(&tiled, width, height, block), linear);
        // Block (1, 0) is the second in Morton order, block (0, 1) the third.
        assert_eq!((tiled[16], tiled[32]), (1, 16));
    }
}
