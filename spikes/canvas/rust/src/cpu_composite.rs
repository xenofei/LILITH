//! CPU reference compositor (IMG-036): the same formulas as `shaders/composite.comp`.
//!
//! Colour model (IMG-035): 8-bit layer texels are gamma-encoded sRGB, read as `k / 255` and blended
//! as they are (no linearisation). Layer texels are straight alpha; everything after the first
//! blend is premultiplied `f32`, like the GPU registers and the GPU group intermediate (RGBA32F).
//! The final projection is premultiplied RGBA8 (`round(clamp(x) * 255)`).
//!
//! A tile's stack is a flat list, bottom to top:
//! - `KIND_PIXEL`: a 128x128 straight-alpha RGBA8 tile (`pixels` empty = transparent tile);
//! - `KIND_GROUP_BEGIN` / `KIND_GROUP_END`: an isolated group. The children are composited onto
//!   transparent black; at the end the result is blended onto the backdrop with the group's
//!   `blend`, `opacity` and `visible` (carried by the BEGIN entry);
//! - `KIND_CURVES`: a curves adjustment applied to everything below it in the same group;
//!   `pixels` is a 256-entry RGBA8 lookup table (R, G and B used), linearly interpolated.
//!
//! Blend modes (separable, W3C compositing formulas in premultiplied form):
//! normal, multiply, screen, overlay.

#[cxx_qt::bridge]
pub mod ffi {
    /// One entry of a tile's layer stack (bottom to top). See the module docs.
    struct LayerTile<'a> {
        kind: u8,
        blend: u8,
        opacity: f32,
        visible: bool,
        pixels: &'a [u8],
    }

    extern "Rust" {
        /// Composites one 128x128 tile; `out` is premultiplied RGBA8.
        unsafe fn cpu_composite_tile<'a>(layers: &[LayerTile<'a>], out: &mut [u8; 65536]);
        /// 2x2 box filter (edge texels clamped, rounded to nearest) of RGBA8 `src` (w x h) into
        /// `dst` (dw x dh).
        fn cpu_downsample(src: &[u8], w: u32, h: u32, dst: &mut [u8], dw: u32, dh: u32);
        /// Largest per-channel absolute difference between two RGBA8 buffers (255 if the lengths
        /// differ).
        fn cpu_max_abs_diff(a: &[u8], b: &[u8]) -> u32;
    }
}

pub use ffi::LayerTile;

pub const TILE: usize = 128;
pub const TILE_TEXELS: usize = TILE * TILE;
pub const TILE_BYTES: usize = TILE_TEXELS * 4;
pub const LUT_BYTES: usize = 256 * 4;

pub const KIND_PIXEL: u8 = 0;
pub const KIND_GROUP_BEGIN: u8 = 1;
pub const KIND_GROUP_END: u8 = 2;
pub const KIND_CURVES: u8 = 3;

pub const BLEND_NORMAL: u8 = 0;
pub const BLEND_MULTIPLY: u8 = 1;
pub const BLEND_SCREEN: u8 = 2;
pub const BLEND_OVERLAY: u8 = 3;

/// Premultiplied RGBA.
type Px = [f32; 4];

/// Blends premultiplied source `s` (opacity already applied) over premultiplied backdrop `b`.
#[inline]
pub fn blend_px(b: Px, s: Px, mode: u8) -> Px {
    let (sa, ba) = (s[3], b[3]);
    let mut o = [0.0f32; 4];
    for c in 0..3 {
        let (cs, cb) = (s[c], b[c]);
        o[c] = match mode {
            BLEND_MULTIPLY => cs * (1.0 - ba) + cb * (1.0 - sa) + cs * cb,
            BLEND_SCREEN => cs + cb - cs * cb,
            BLEND_OVERLAY => {
                // Overlay = hard light with the roles swapped; the test "Cb <= 0.5" is
                // "2 cb <= ba" in premultiplied form.
                let m = if 2.0 * cb <= ba {
                    2.0 * cs * cb
                } else {
                    sa * ba - 2.0 * (ba - cb) * (sa - cs)
                };
                cs * (1.0 - ba) + cb * (1.0 - sa) + m
            }
            _ => cs + cb * (1.0 - sa),
        };
    }
    o[3] = sa + ba * (1.0 - sa);
    o
}

/// Straight-alpha RGBA8 texel with layer opacity -> premultiplied source.
#[inline]
pub fn source_px(t: &[u8], opacity: f32) -> Px {
    let a = (t[3] as f32 / 255.0) * opacity;
    [
        (t[0] as f32 / 255.0) * a,
        (t[1] as f32 / 255.0) * a,
        (t[2] as f32 / 255.0) * a,
        a,
    ]
}

/// The curve of channel `c` at `v` (0..1): linear interpolation between LUT entries.
#[inline]
pub fn curve(lut: &[u8], c: usize, v: f32) -> f32 {
    let x = v.clamp(0.0, 1.0) * 255.0;
    let i0 = x.floor().min(254.0);
    let f = x - i0;
    let i0 = i0 as usize;
    let a = lut[i0 * 4 + c] as f32 / 255.0;
    let b = lut[(i0 + 1) * 4 + c] as f32 / 255.0;
    a + (b - a) * f
}

/// Applies a curves adjustment (with its opacity) to a premultiplied pixel.
#[inline]
pub fn apply_curves(b: Px, lut: &[u8], opacity: f32) -> Px {
    if b[3] <= 0.0 {
        return b;
    }
    let mut o = b;
    for c in 0..3 {
        let straight = (b[c] / b[3]).min(1.0);
        let adj = curve(lut, c, straight) * b[3];
        o[c] = b[c] + (adj - b[c]) * opacity;
    }
    o
}

/// `round(clamp(v) * 255)`, ties away from zero. (Measured on the test machine's 16 GB GPU: the GPU results come
/// out a few hundredths of an LSB lower and its UNORM8 stores round exact ties down, so GPU and CPU
/// differ by 1 on ~2.4 % of channels, never by 2; the 1/255 golden tolerance covers it. See
/// docs/spikes/03-canvas-compositing.md.)
#[inline]
pub fn quantize(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

struct GroupFrame {
    buf: Vec<Px>,
    blend: u8,
    opacity: f32,
    visible: bool,
}

/// Composites one tile's stack into premultiplied RGBA8.
pub fn composite_tile(layers: &[LayerTile<'_>], out: &mut [u8; TILE_BYTES]) {
    let mut stack: Vec<GroupFrame> = vec![GroupFrame {
        buf: vec![[0.0; 4]; TILE_TEXELS],
        blend: BLEND_NORMAL,
        opacity: 1.0,
        visible: true,
    }];
    for l in layers {
        match l.kind {
            KIND_PIXEL => {
                if !l.visible || l.pixels.is_empty() {
                    continue;
                }
                assert_eq!(l.pixels.len(), TILE_BYTES, "pixel layer tile must be 128x128 RGBA8");
                let top = stack.last_mut().unwrap();
                for (i, px) in top.buf.iter_mut().enumerate() {
                    let s = source_px(&l.pixels[i * 4..i * 4 + 4], l.opacity);
                    *px = blend_px(*px, s, l.blend);
                }
            }
            KIND_GROUP_BEGIN => stack.push(GroupFrame {
                buf: vec![[0.0; 4]; TILE_TEXELS],
                blend: l.blend,
                opacity: l.opacity,
                visible: l.visible,
            }),
            KIND_GROUP_END => {
                assert!(stack.len() > 1, "GROUP_END without GROUP_BEGIN");
                let g = stack.pop().unwrap();
                if !g.visible {
                    continue;
                }
                let top = stack.last_mut().unwrap();
                for (px, gp) in top.buf.iter_mut().zip(g.buf.iter()) {
                    let s = [gp[0] * g.opacity, gp[1] * g.opacity, gp[2] * g.opacity, gp[3] * g.opacity];
                    *px = blend_px(*px, s, g.blend);
                }
            }
            KIND_CURVES => {
                if !l.visible {
                    continue;
                }
                assert_eq!(l.pixels.len(), LUT_BYTES, "curves LUT must be 256 RGBA8 entries");
                let top = stack.last_mut().unwrap();
                for px in top.buf.iter_mut() {
                    *px = apply_curves(*px, l.pixels, l.opacity);
                }
            }
            k => panic!("unknown layer kind {k}"),
        }
    }
    assert_eq!(stack.len(), 1, "unbalanced GROUP_BEGIN/GROUP_END");
    for (i, px) in stack[0].buf.iter().enumerate() {
        for c in 0..4 {
            out[i * 4 + c] = quantize(px[c]);
        }
    }
}

/// Bridge entry point (unsafe only because cxx requires it for explicit lifetimes).
///
/// # Safety
/// Always safe to call: the slices come from cxx with their lengths.
pub unsafe fn cpu_composite_tile<'a>(layers: &[LayerTile<'a>], out: &mut [u8; 65536]) {
    composite_tile(layers, out);
}

pub fn cpu_downsample(src: &[u8], w: u32, h: u32, dst: &mut [u8], dw: u32, dh: u32) {
    let (w, h, dw, dh) = (w as usize, h as usize, dw as usize, dh as usize);
    assert!(src.len() >= w * h * 4 && dst.len() >= dw * dh * 4);
    for y in 0..dh {
        let (y0, y1) = ((2 * y).min(h - 1), (2 * y + 1).min(h - 1));
        for x in 0..dw {
            let (x0, x1) = ((2 * x).min(w - 1), (2 * x + 1).min(w - 1));
            for c in 0..4 {
                let sum = src[(y0 * w + x0) * 4 + c] as u32
                    + src[(y0 * w + x1) * 4 + c] as u32
                    + src[(y1 * w + x0) * 4 + c] as u32
                    + src[(y1 * w + x1) * 4 + c] as u32;
                dst[(y * dw + x) * 4 + c] = ((sum + 2) / 4) as u8;
            }
        }
    }
}

pub fn cpu_max_abs_diff(a: &[u8], b: &[u8]) -> u32 {
    if a.len() != b.len() {
        return 255;
    }
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs()).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
        [r, g, b, a].repeat(TILE_TEXELS)
    }
    fn pixel<'a>(px: &'a [u8], blend: u8, opacity: f32) -> LayerTile<'a> {
        LayerTile { kind: KIND_PIXEL, blend, opacity, visible: true, pixels: px }
    }
    fn group<'a>(kind: u8, blend: u8, opacity: f32) -> LayerTile<'a> {
        LayerTile { kind, blend, opacity, visible: true, pixels: &[] }
    }
    fn run(layers: &[LayerTile<'_>]) -> Box<[u8; TILE_BYTES]> {
        let mut out = Box::new([0u8; TILE_BYTES]);
        composite_tile(layers, &mut out);
        out
    }
    fn within1(a: u8, b: u8) -> bool {
        (a as i32 - b as i32).abs() <= 1
    }

    #[test]
    fn normal_over_white_50pct_is_mid_grey() {
        let white = solid(255, 255, 255, 255);
        let black = solid(0, 0, 0, 255);
        let out = run(&[pixel(&white, BLEND_NORMAL, 1.0), pixel(&black, BLEND_NORMAL, 0.5)]);
        for px in out.chunks_exact(4) {
            for c in 0..3 {
                assert!(within1(px[c], 128), "channel {c} = {}", px[c]);
            }
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn blend_modes_match_textbook_values_on_opaque_backdrop() {
        let back = solid(102, 153, 204, 255); // 0.4, 0.6, 0.8
        let top = solid(51, 204, 128, 255); // 0.2, 0.8, ~0.502
        let mul = run(&[pixel(&back, 0, 1.0), pixel(&top, BLEND_MULTIPLY, 1.0)]);
        let scr = run(&[pixel(&back, 0, 1.0), pixel(&top, BLEND_SCREEN, 1.0)]);
        let ovl = run(&[pixel(&back, 0, 1.0), pixel(&top, BLEND_OVERLAY, 1.0)]);
        let (b, t) = ([0.4f32, 0.6, 0.8], [0.2f32, 0.8, 128.0 / 255.0]);
        for c in 0..3 {
            let m = b[c] * t[c];
            let s = b[c] + t[c] - b[c] * t[c];
            let o = if b[c] <= 0.5 { 2.0 * b[c] * t[c] } else { 1.0 - 2.0 * (1.0 - b[c]) * (1.0 - t[c]) };
            assert!(within1(mul[c], quantize(m)), "multiply c{c}");
            assert!(within1(scr[c], quantize(s)), "screen c{c}");
            assert!(within1(ovl[c], quantize(o)), "overlay c{c}");
        }
    }

    #[test]
    fn hidden_layers_and_empty_tiles_are_skipped() {
        let white = solid(255, 255, 255, 255);
        let red = solid(255, 0, 0, 255);
        let mut hidden = pixel(&red, BLEND_NORMAL, 1.0);
        hidden.visible = false;
        let out = run(&[pixel(&white, 0, 1.0), hidden, pixel(&[], BLEND_MULTIPLY, 1.0)]);
        assert!(out.iter().all(|v| *v == 255));
    }

    #[test]
    fn normal_group_at_full_opacity_equals_flat_stack() {
        let a = solid(200, 30, 90, 255);
        let b = solid(10, 220, 40, 128);
        let c = solid(90, 90, 250, 200);
        let flat = run(&[pixel(&a, 0, 1.0), pixel(&b, 0, 0.8), pixel(&c, 0, 0.6)]);
        let grouped = run(&[
            pixel(&a, 0, 1.0),
            group(KIND_GROUP_BEGIN, BLEND_NORMAL, 1.0),
            pixel(&b, 0, 0.8),
            pixel(&c, 0, 0.6),
            group(KIND_GROUP_END, 0, 1.0),
        ]);
        assert!(flat.iter().zip(grouped.iter()).all(|(x, y)| within1(*x, *y)));
    }

    #[test]
    fn isolated_multiply_group_differs_from_flat() {
        // Isolation: a multiply layer inside a group multiplies only with the group content.
        let back = solid(255, 0, 0, 255);
        let mid = solid(0, 0, 255, 255);
        let mul = solid(128, 128, 128, 255);
        let iso = run(&[
            pixel(&back, 0, 1.0),
            group(KIND_GROUP_BEGIN, BLEND_NORMAL, 0.5),
            pixel(&mid, 0, 1.0),
            pixel(&mul, BLEND_MULTIPLY, 1.0),
            group(KIND_GROUP_END, 0, 1.0),
        ]);
        // group = (0,0,~0.5) opaque; at 50% over red -> (0.5, 0, ~0.25)
        assert!(within1(iso[0], 128) && within1(iso[1], 0) && within1(iso[2], 64), "{:?}", &iso[0..4]);
    }

    #[test]
    fn identity_curve_is_a_no_op_and_invert_curve_inverts() {
        let px = solid(30, 128, 250, 255);
        let ident: Vec<u8> = (0..256).flat_map(|i| [i as u8; 4]).collect();
        let invert: Vec<u8> = (0..256).flat_map(|i| [255 - i as u8; 4]).collect();
        let curves = |lut: &'static [u8]| LayerTile { kind: KIND_CURVES, blend: 0, opacity: 1.0, visible: true, pixels: lut };
        let ident: &'static [u8] = Box::leak(ident.into_boxed_slice());
        let invert: &'static [u8] = Box::leak(invert.into_boxed_slice());
        let a = run(&[pixel(&px, 0, 1.0), curves(ident)]);
        let b = run(&[pixel(&px, 0, 1.0), curves(invert)]);
        assert_eq!(&a[0..4], &[30, 128, 250, 255]);
        assert!(within1(b[0], 225) && within1(b[1], 127) && within1(b[2], 5), "{:?}", &b[0..4]);
    }

    #[test]
    fn downsample_is_a_rounded_box_filter() {
        let src = [0u8, 0, 0, 0, 255, 255, 255, 255, 1, 2, 3, 4, 2, 2, 2, 2]; // 2x2
        let mut dst = [0u8; 4];
        cpu_downsample(&src, 2, 2, &mut dst, 1, 1);
        assert_eq!(dst, [65, 65, 65, 65]); // (0+255+1+2+2)/4 = 65 for c0; similar for others
    }
}
