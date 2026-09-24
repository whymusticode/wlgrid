//! CPU rasteriser drawing straight into a `wl_shm` ARGB8888 buffer (premultiplied
//! 0xAARRGGBB words). Only three primitives are needed: anti-aliased rounded
//! rects, scaled icons, and tinted glyph masks. Callers use screen (full-surface
//! buffer px) coordinates; the canvas covers the region at `origin` and clips.

use std::collections::HashMap;

/// Icons pre-scaled to a draw size and premultiplied, keyed by (entry index, size).
pub type IconCache = HashMap<(usize, i32), Vec<u32>>;

pub struct Canvas<'a> {
    px: &'a mut [u32],
    w: i32,
    h: i32,
    origin: (i32, i32),
    icons: &'a mut IconCache,
}

/// Premultiply a straight-alpha colour scaled by `cov` into an ARGB word.
fn premul(c: [f32; 4], cov: f32) -> u32 {
    let a = (c[3] * cov).clamp(0.0, 1.0);
    let q = |v: f32| (v.clamp(0.0, 1.0) * a * 255.0 + 0.5) as u32;
    ((a * 255.0 + 0.5) as u32) << 24 | q(c[0]) << 16 | q(c[1]) << 8 | q(c[2])
}

/// Premultiplied `src` over premultiplied `dst`. Two channels are scaled per
/// multiply (0x00RR00BB / 0x00AA00GG lanes) with the usual exact /255 trick.
#[inline]
fn over(dst: u32, src: u32) -> u32 {
    let inv = 255 - (src >> 24);
    if inv == 0 || dst == 0 {
        return src;
    }
    let scale = |lanes: u32| {
        let t = lanes * inv + 0x0080_0080;
        ((t + ((t >> 8) & 0x00FF_00FF)) >> 8) & 0x00FF_00FF
    };
    src + (scale(dst & 0x00FF_00FF) | scale((dst >> 8) & 0x00FF_00FF) << 8)
}

impl<'a> Canvas<'a> {
    /// Wrap `px` (w×h words) covering the screen region whose top-left is `origin`.
    pub fn new(px: &'a mut [u32], w: i32, h: i32, origin: (i32, i32), icons: &'a mut IconCache) -> Self {
        px.fill(0);
        Canvas { px, w, h, origin, icons }
    }

    /// Screen rect → clipped local (x0, y0, x1, y1), or None if off-canvas.
    fn clip(&self, x: i32, y: i32, w: i32, h: i32) -> Option<(i32, i32, i32, i32)> {
        let (x, y) = (x - self.origin.0, y - self.origin.1);
        let (x0, y0, x1, y1) = (x.max(0), y.max(0), (x + w).min(self.w), (y + h).min(self.h));
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    /// Rounded rect with an optional inner border, anti-aliased with the same
    /// signed-distance maths as the old GL shader. Colours are straight alpha.
    #[allow(clippy::too_many_arguments)]
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, radius: f32, fill: [f32; 4], border: [f32; 4], border_w: f32) {
        let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) else { return };
        let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
        let r = radius.clamp(0.0, hw.min(hh));
        let bw = border_w.max(0.0);
        let (cx, cy) = ((x - self.origin.0) as f32 + hw, (y - self.origin.1) as f32 + hh);
        let solid = premul(fill, 1.0);

        // Colour by signed distance to the edge, sampled every 1/STEPS px over
        // the only range that isn't plain fill (< lo) or outside (> 0.5).
        const STEPS: f32 = 32.0;
        let lo = -(bw + 0.5);
        let lut: Vec<u32> = (0..=((0.5 - lo) * STEPS) as usize).map(|i| {
            let dist = lo + i as f32 / STEPS;
            let outer = (0.5 - dist).clamp(0.0, 1.0);
            let inner = (0.5 - (dist + bw)).clamp(0.0, 1.0);
            let col: [f32; 4] = std::array::from_fn(|i| border[i] + (fill[i] - border[i]) * inner);
            premul(col, outer)
        }).collect();
        // Per-column offset past the straight part of the edge (the SDF's q).
        let qxs: Vec<f32> = (x0..x1).map(|px| (px as f32 + 0.5 - cx).abs() - (hw - r)).collect();

        for py in y0..y1 {
            let row = (py * self.w) as usize;
            let qy = (py as f32 + 0.5 - cy).abs() - (hh - r);
            // This row's span of plain fill (distance <= lo): only the few
            // pixels either side of it need the distance field.
            let (fx0, fx1) = if qy - r <= lo {
                let half = if qy > 0.0 { hw - r } else { hw + lo };
                let a = ((cx - half - 0.5).ceil() as i32).clamp(x0, x1);
                (a, ((cx + half - 0.5).floor() as i32 + 1).clamp(a, x1))
            } else {
                (x1, x1)
            };
            for px in (x0..fx0).chain(fx1..x1) {
                let qx = qxs[(px - x0) as usize];
                // Rounded-rect SDF: Euclidean only in the corners.
                let dist = if qx > 0.0 && qy > 0.0 { (qx * qx + qy * qy).sqrt() } else { qx.max(qy) } - r;
                let src = if dist <= lo {
                    solid
                } else if dist >= 0.5 {
                    continue;
                } else {
                    lut[((dist - lo) * STEPS) as usize]
                };
                if src >> 24 > 0 {
                    let p = &mut self.px[row + px as usize];
                    *p = over(*p, src);
                }
            }
            if solid >> 24 > 0 {
                for p in &mut self.px[row + fx0 as usize..row + fx1 as usize] {
                    *p = over(*p, solid);
                }
            }
        }
    }

    /// Draw a `src`×`src` straight-alpha RGBA icon scaled to `size`. The
    /// scaled, premultiplied copy is cached under (`key`, `size`).
    pub fn icon(&mut self, key: usize, pixels: &[u8], src: i32, x: i32, y: i32, size: i32) {
        let Some((x0, y0, x1, y1)) = self.clip(x, y, size, size) else { return };
        let scaled = self.icons.entry((key, size)).or_insert_with(|| scale_icon(pixels, src, size));
        let (lx, ly) = (x - self.origin.0, y - self.origin.1);
        for py in y0..y1 {
            let srow = ((py - ly) * size) as usize;
            let row = (py * self.w) as usize;
            for px in x0..x1 {
                let s = scaled[srow + (px - lx) as usize];
                if s >> 24 == 0 {
                    continue;
                }
                let p = &mut self.px[row + px as usize];
                *p = over(*p, s);
            }
        }
    }

    /// Draw an 8-bit coverage mask (`w`×`h`) tinted with a straight-alpha colour.
    #[allow(clippy::too_many_arguments)]
    pub fn mask(&mut self, mask: &[u8], w: i32, h: i32, x: i32, y: i32, tint: [f32; 4]) {
        let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) else { return };
        let (lx, ly) = (x - self.origin.0, y - self.origin.1);
        let shades: Vec<u32> = (0..=255).map(|cov| premul(tint, cov as f32 / 255.0)).collect();
        for py in y0..y1 {
            let mrow = ((py - ly) * w) as usize;
            let row = (py * self.w) as usize;
            for px in x0..x1 {
                let cov = mask[mrow + (px - lx) as usize];
                if cov == 0 {
                    continue;
                }
                let p = &mut self.px[row + px as usize];
                *p = over(*p, shades[cov as usize]);
            }
        }
    }
}

/// Linear blend of two premultiplied ARGB words, `t` in 0..=256 towards `b`,
/// two channels per multiply as in `over`.
#[inline]
fn lerp(a: u32, b: u32, t: u32) -> u32 {
    let lanes = |m: u32| ((((a >> m) & 0x00FF_00FF) * (256 - t) + ((b >> m) & 0x00FF_00FF) * t) >> 8) & 0x00FF_00FF;
    lanes(0) | lanes(8) << 8
}

/// Bilinearly resample a straight-alpha RGBA icon to `size`², premultiplied.
/// Separable: each source row is resampled horizontally once, then output
/// rows blend two of those vertically.
fn scale_icon(pixels: &[u8], src: i32, size: i32) -> Vec<u32> {
    // Source index pair + weight (of the second, 0..=256) per output column / row.
    let taps = |i: i32| {
        let f = ((i as f32 + 0.5) * src as f32 / size as f32 - 0.5).max(0.0);
        let i0 = (f as i32).min(src - 1);
        (i0 as usize, (i0 + 1).min(src - 1) as usize, ((f - i0 as f32) * 256.0) as u32)
    };
    let (src, size) = (src as usize, size as usize);
    // Premultiply first so transparent texels don't bleed colour.
    let texels: Vec<u32> = pixels.chunks_exact(4).map(|p| {
        let (a, pm) = (p[3] as u32, |c: u8| (c as u32 * p[3] as u32 + 127) / 255);
        a << 24 | pm(p[0]) << 16 | pm(p[1]) << 8 | pm(p[2])
    }).collect();
    let cols: Vec<_> = (0..size as i32).map(taps).collect();
    let rows: Vec<u32> = texels.chunks_exact(src)
        .flat_map(|row| cols.iter().map(|&(x0, x1, t)| lerp(row[x0], row[x1], t)))
        .collect();
    let mut out = Vec::with_capacity(size * size);
    for (y0, y1, t) in (0..size as i32).map(taps) {
        let (r0, r1) = (&rows[y0 * size..][..size], &rows[y1 * size..][..size]);
        out.extend(r0.iter().zip(r1).map(|(&a, &b)| lerp(a, b, t)));
    }
    out
}
