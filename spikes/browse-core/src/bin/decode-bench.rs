//! S0.2 (APP-038; P4b-R12, P4b-R36): the `image` crate's decode, Lanczos3 resize and PNG
//! encode, in process.
//!
//! `decode-bench gen --work <dir>`: the four generated inputs.
//! `decode-bench timing --work <dir> --out <json>`: one thread, 160/256/512 px.
//! `decode-bench safety --work <dir> --out <json>`: the 60,000² header and broken files (own process).
//! `decode-bench concurrent --work <dir> --out <json>`: four 4096² decodes at once (own process).
//! `decode-bench cold --work <dir> --out <json> [--count 10000]`: the cold thumbnail cache.

use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

use browse_core::stats::summary;
use browse_core::win::{MIB, working_set};
use browse_core::{Rng, num_arg, path_arg, subcommand, write_json};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageFormat, ImageReader, Limits, RgbImage};
use serde_json::{Value, json};

/// P4b-R36's per-decode allocation limit for tiers up to 512 px, and P4b-R12's side limit.
const ALLOC_LIMIT: u64 = 256 << 20;
const SIDE_LIMIT: u32 = 16_384;

/// Smooth gradients, a few rings and deterministic noise, so the files don't compress away.
fn picture(w: u32, h: u32, seed: u64) -> RgbImage {
    let mut rng = Rng::new(seed);
    let (cx, cy) = (
        w as f64 * (0.3 + 0.4 * rng.unit()),
        h as f64 * (0.3 + 0.4 * rng.unit()),
    );
    let phase = rng.unit() * std::f64::consts::TAU;
    let mut img = RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
        let d = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt() / w as f64;
        let ring = ((d * 40.0 + phase).sin() * 0.5 + 0.5) * 80.0;
        let n = (rng.below(33) as f64) - 16.0;
        let c = |base: f64| (base + n).clamp(0.0, 255.0) as u8;
        *p = image::Rgb([
            c(fx * 170.0 + ring),
            c(fy * 170.0 + ring * 0.5),
            c((1.0 - fx) * 120.0 + ring),
        ]);
    }
    img
}

fn png_bytes(img: &DynamicImage) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)
        .expect("png encode");
    out.into_inner()
}

fn jpeg_bytes(img: &RgbImage, quality: u8) -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(img)
        .expect("jpeg encode");
    out
}

fn webp_bytes(img: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .encode(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgb8,
        )
        .expect("webp encode");
    out
}

/// The limits a render uses (P4b-R12, P4b-R36).
fn limits(alloc: u64) -> Limits {
    let mut l = Limits::default();
    l.max_image_width = Some(SIDE_LIMIT);
    l.max_image_height = Some(SIDE_LIMIT);
    l.max_alloc = Some(alloc);
    l
}

fn decode(bytes: &[u8], lim: Limits) -> Result<DynamicImage, String> {
    let mut r = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    r.limits(lim);
    r.decode().map_err(|e| e.to_string())
}

/// One render: read, decode, resize (Lanczos3, long side to `px`), PNG-encode. Returns the PNG
/// and the stage times in ms (read, decode, resize, encode).
fn render(path: &Path, px: u32) -> Result<(Vec<u8>, [f64; 4]), String> {
    let t0 = Instant::now();
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let t1 = Instant::now();
    let img = decode(&bytes, limits(ALLOC_LIMIT))?;
    let t2 = Instant::now();
    let thumb = img.resize(px, px, FilterType::Lanczos3);
    let t3 = Instant::now();
    let png = png_bytes(&thumb);
    let t4 = Instant::now();
    let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1000.0;
    Ok((png, [ms(t0, t1), ms(t1, t2), ms(t2, t3), ms(t3, t4)]))
}

const INPUTS: [(&str, &str); 4] = [
    ("png_1024", "in-1024.png"),
    ("jpeg_2048", "in-2048.jpg"),
    ("webp_1024", "in-1024.webp"),
    ("png_4096", "in-4096.png"),
];

fn gen_inputs(work: &Path) {
    std::fs::create_dir_all(work).expect("work folder");
    let p1024 = picture(1024, 1024, 1);
    std::fs::write(
        work.join("in-1024.png"),
        png_bytes(&DynamicImage::ImageRgb8(p1024.clone())),
    )
    .unwrap();
    std::fs::write(work.join("in-1024.webp"), webp_bytes(&p1024)).unwrap();
    std::fs::write(
        work.join("in-2048.jpg"),
        jpeg_bytes(&picture(2048, 2048, 2), 90),
    )
    .unwrap();
    std::fs::write(
        work.join("in-4096.png"),
        png_bytes(&DynamicImage::ImageRgb8(picture(4096, 4096, 3))),
    )
    .unwrap();
    for (_, f) in INPUTS {
        let len = std::fs::metadata(work.join(f))
            .map(|m| m.len())
            .unwrap_or(0);
        println!("generated {f}: {:.1} MiB", len as f64 / MIB);
    }
}

fn timing(work: &Path, out: &Path) {
    let mut cases = Vec::new();
    let mut bar_p95 = None;
    for (name, file) in INPUTS {
        let path = work.join(file);
        let pxs: &[u32] = if name == "png_1024" {
            &[160, 256, 512]
        } else {
            &[160, 512]
        };
        for &px in pxs {
            let runs = if name == "png_1024" && px == 256 {
                100
            } else {
                30
            };
            for _ in 0..3 {
                render(&path, px).expect("warm-up render");
            }
            let mut total = Vec::new();
            let mut stages: [Vec<f64>; 4] = Default::default();
            for _ in 0..runs {
                let t = Instant::now();
                let (_, s) = render(&path, px).expect("render");
                total.push(t.elapsed().as_secs_f64() * 1000.0);
                for (k, v) in s.iter().enumerate() {
                    stages[k].push(*v);
                }
            }
            let sum = summary(&total);
            if name == "png_1024" && px == 256 {
                bar_p95 = sum["p95"].as_f64();
            }
            println!(
                "{name} -> {px} px: p50 {} p95 {} ms",
                sum["p50"], sum["p95"]
            );
            cases.push(json!({
                "input": name, "file_mib": std::fs::metadata(&path).map(|m| m.len() as f64 / MIB).unwrap_or(0.0),
                "px": px, "runs": runs, "total_ms": sum,
                "read_ms": summary(&stages[0]), "decode_ms": summary(&stages[1]),
                "resize_ms": summary(&stages[2]), "encode_ms": summary(&stages[3]),
                "total_ms_raw": total,
            }));
        }
    }
    let pass = bar_p95.is_some_and(|p| p < 60.0);
    write_json(
        out,
        &json!({"threads": 1, "alloc_limit_mib": ALLOC_LIMIT >> 20, "cases": cases, "png_1024_to_256_p95_ms": bar_p95, "pass": pass}),
    );
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend(data);
    out.extend(&body);
    out.extend(crc32(&body).to_be_bytes());
}

/// A valid PNG whose IHDR claims `w` × `h` RGBA 8-bit, with a short valid IDAT (a stored
/// deflate block of five zero bytes) and IEND.
fn header_bomb(w: u32, h: u32) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 6, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    let zlib = [
        0x78, 0x01, 0x01, 0x05, 0x00, 0xFA, 0xFF, 0, 0, 0, 0, 0, 0x00, 0x05, 0x00, 0x01,
    ];
    chunk(&mut png, b"IDAT", &zlib);
    chunk(&mut png, b"IEND", &[]);
    png
}

/// Decodes `bytes` under `catch_unwind`: `{outcome: "error" | "ok" | "panic", message}`.
fn guarded(bytes: &[u8], lim: Limits) -> Value {
    match catch_unwind(AssertUnwindSafe(|| decode(bytes, lim))) {
        Ok(Ok(img)) => json!({"outcome": "ok", "width": img.width(), "height": img.height()}),
        Ok(Err(e)) => json!({"outcome": "error", "message": e}),
        Err(p) => {
            json!({"outcome": "panic", "message": p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))})
        }
    }
}

fn safety(work: &Path, out: &Path) {
    // The header first, while the process is still small, so the peak belongs to it.
    let bomb = header_bomb(60_000, 60_000);
    let (base, _) = working_set();
    let t = Instant::now();
    let bomb_result = guarded(&bomb, limits(ALLOC_LIMIT));
    let bomb_ms = t.elapsed().as_secs_f64() * 1000.0;
    let (_, peak) = working_set();
    let peak_above = (peak.saturating_sub(base)) as f64 / MIB;
    // Recorded: the same header with only the allocation limit, and with the crate's defaults.
    let mut alloc_only = Limits::default();
    alloc_only.max_alloc = Some(ALLOC_LIMIT);
    let bomb_alloc_only = guarded(&bomb, alloc_only);
    let bomb_defaults = guarded(&bomb, Limits::default());
    let (_, peak_after_all) = working_set();

    let jpeg = std::fs::read(work.join("in-2048.jpg")).expect("in-2048.jpg (run gen first)");
    let half = guarded(&jpeg[..jpeg.len() / 2], limits(ALLOC_LIMIT));
    let head = guarded(&jpeg[..200], limits(ALLOC_LIMIT));
    let empty = guarded(&[], limits(ALLOC_LIMIT));

    let bomb_pass = bomb_result["outcome"] == "error" && peak_above < 64.0;
    let broken_pass = [&half, &empty].iter().all(|r| r["outcome"] == "error");
    write_json(
        out,
        &json!({
            "bomb": {"claims": "60000 x 60000 RGBA8", "bytes": bomb.len(), "result": bomb_result, "ms": bomb_ms,
                     "baseline_ws_mib": base as f64 / MIB, "peak_above_baseline_mib": peak_above, "pass": bomb_pass,
                     "alloc_limit_only": bomb_alloc_only, "crate_default_limits": bomb_defaults,
                     "peak_above_baseline_after_all_three_mib": peak_after_all.saturating_sub(base) as f64 / MIB},
            "broken": {"jpeg_cut_to_half": half, "jpeg_cut_to_200_bytes": head, "empty": empty, "pass": broken_pass},
        }),
    );
    println!(
        "safety: bomb {} (peak +{peak_above:.1} MiB), half JPEG {}, 200-byte JPEG {}, empty {}",
        bomb_result["outcome"], half["outcome"], head["outcome"], empty["outcome"]
    );
}

fn concurrent(work: &Path, out: &Path) {
    let (base, _) = working_set();
    let path = work.join("in-4096.png");
    let barrier = Arc::new(Barrier::new(4));
    let start = Instant::now();
    let results: Vec<Value> = (0..4)
        .map(|_| {
            let (path, barrier) = (path.clone(), barrier.clone());
            thread::spawn(move || {
                barrier.wait();
                let t = Instant::now();
                let r = catch_unwind(AssertUnwindSafe(|| render(&path, 512)));
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                match r {
                    Ok(Ok((png, s))) => json!({"outcome": "ok", "ms": ms, "png_bytes": png.len(), "decode_ms": s[1]}),
                    Ok(Err(e)) => json!({"outcome": "error", "message": e, "ms": ms}),
                    Err(_) => json!({"outcome": "panic", "ms": ms}),
                }
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().expect("join"))
        .collect();
    let wall = start.elapsed().as_secs_f64() * 1000.0;
    let (_, peak) = working_set();
    let above = peak.saturating_sub(base) as f64 / MIB;
    let all_ok = results.iter().all(|r| r["outcome"] == "ok");
    let pass = all_ok && above < 1536.0;
    write_json(
        out,
        &json!({"threads": 4, "alloc_limit_mib": ALLOC_LIMIT >> 20, "results": results, "wall_ms": wall,
        "baseline_ws_mib": base as f64 / MIB, "peak_above_baseline_mib": above, "all_decoded": all_ok, "pass": pass}),
    );
    println!("concurrent: all ok {all_ok}, peak +{above:.1} MiB, wall {wall:.0} ms");
}

fn cold(work: &Path, out: &Path) {
    let count: usize = num_arg("--count", 10_000);
    let inputs = work.join("cold-in");
    let thumbs = work.join("cold-thumbs");
    let _ = std::fs::remove_dir_all(&thumbs);
    std::fs::create_dir_all(&inputs).unwrap();
    std::fs::create_dir_all(&thumbs).unwrap();
    let cores = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    // Generation, untimed, on every core.
    let next = AtomicUsize::new(0);
    let gen_start = Instant::now();
    thread::scope(|s| {
        for _ in 0..cores {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= count {
                        break;
                    }
                    let p = inputs.join(format!("{i:05}.jpg"));
                    if !p.exists() {
                        std::fs::write(&p, jpeg_bytes(&picture(512, 512, 1000 + i as u64), 85))
                            .unwrap();
                    }
                }
            });
        }
    });
    let gen_s = gen_start.elapsed().as_secs_f64();
    let total_bytes: u64 = (0..count)
        .filter_map(|i| std::fs::metadata(inputs.join(format!("{i:05}.jpg"))).ok())
        .map(|m| m.len())
        .sum();
    let workers = 4.min(cores / 2).max(1);
    let next = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    let start = Instant::now();
    let per_item: Vec<f64> = thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut times = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= count {
                            break;
                        }
                        let t = Instant::now();
                        match render(&inputs.join(format!("{i:05}.jpg")), 160) {
                            Ok((png, _)) => {
                                std::fs::write(thumbs.join(format!("{i:05}.png")), png).unwrap()
                            }
                            Err(_) => {
                                failures.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        times.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                    times
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    let total_s = start.elapsed().as_secs_f64();
    write_json(
        out,
        &json!({"count": count, "px": 160, "logical_processors": cores, "workers": workers,
        "inputs_mib": total_bytes as f64 / MIB, "generation_s": gen_s, "total_s": total_s,
        "per_second": count as f64 / total_s, "per_item_ms": summary(&per_item), "failures": failures.load(Ordering::Relaxed),
        "os_file_cache": "not flushed (needs admin); the inputs were just written"}),
    );
    println!("cold: {count} at 160 px with {workers} workers in {total_s:.2} s");
}

fn main() {
    let work = path_arg("--work");
    match subcommand().as_str() {
        "gen" => gen_inputs(&work),
        "timing" => timing(&work, &path_arg("--out")),
        "safety" => safety(&work, &path_arg("--out")),
        "concurrent" => concurrent(&work, &path_arg("--out")),
        "cold" => cold(&work, &path_arg("--out")),
        other => {
            eprintln!(
                "usage: decode-bench gen|timing|safety|concurrent|cold --work <dir> [--out <json>] (got {other:?})"
            );
            std::process::exit(2);
        }
    }
}
