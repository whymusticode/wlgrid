//! Micro-benchmarks for the wlgrid startup hot-path.
//!
//! These exist to catch regressions in the pure-logic functions that run
//! before the first frame is drawn — font discovery, desktop entry scanning
//! (with a warm and a cold icon cache), placeholder icon generation.
//!
//! Wayland-dependent paths (rendering, event loop) are not benched here; for
//! those we'd need a headless compositor, which is deferred to a later pass.
//!
//! Run with: `cargo bench`
//! Run with verbose logging: `WLGRID_DEBUG=1 cargo bench`

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use wlgrid::{load_entries, make_placeholder_icon, Fonts, IconCache, DEFAULT_ICON_SIZE};

fn bench_make_placeholder_icon(c: &mut Criterion) {
    c.bench_function("make_placeholder_icon", |b| {
        b.iter(|| black_box(make_placeholder_icon(DEFAULT_ICON_SIZE)))
    });
}

fn bench_load_fonts(c: &mut Criterion) {
    c.bench_function("load_fonts", |b| b.iter(|| black_box(Fonts::load())));
}

fn bench_load_entries(c: &mut Criterion) {
    let Some(fonts) = Fonts::load() else {
        eprintln!("no fonts found; skipping load_entries benches");
        return;
    };
    let mut group = c.benchmark_group("load_entries");
    group.sample_size(10);
    // Warm: every icon already in the in-memory cache — this is a normal
    // startup with ~/.cache/wlgrid/icons.bin present.
    let mut warm = IconCache::new(DEFAULT_ICON_SIZE);
    load_entries(DEFAULT_ICON_SIZE, &fonts, &mut warm, &[]);
    group.bench_function("warm_cache", |b| {
        b.iter(|| black_box(load_entries(DEFAULT_ICON_SIZE, &fonts, &mut warm, &[])))
    });
    // Cold: resolve and decode every icon file, as on a first launch.
    group.bench_function("cold_cache", |b| {
        b.iter(|| black_box(load_entries(DEFAULT_ICON_SIZE, &fonts, &mut IconCache::new(DEFAULT_ICON_SIZE), &[])))
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_make_placeholder_icon,
    bench_load_fonts,
    bench_load_entries,
);
criterion_main!(benches);
