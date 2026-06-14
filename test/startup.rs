//! Micro-benchmarks for the wlgrid startup hot-path.
//!
//! These exist to catch regressions in the pure-logic functions that run
//! before the first frame is drawn — cache validation, desktop entry scanning,
//! checksum computation, placeholder icon generation.
//!
//! Wayland-dependent paths (rendering, event loop) are not benched here; for
//! those we'd need a headless compositor, which is deferred to a later pass.
//!
//! Run with: `cargo bench`
//! Run with verbose logging: `WLGRID_DEBUG=1 cargo bench`

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use wlgrid::{
    compute_checksum, load_cache, load_desktop_entries, make_placeholder_icon,
    DEFAULT_ICON_SIZE,
};

fn bench_compute_checksum(c: &mut Criterion) {
    c.bench_function("compute_checksum", |b| {
        b.iter(|| black_box(compute_checksum()))
    });
}

fn bench_make_placeholder_icon(c: &mut Criterion) {
    c.bench_function("make_placeholder_icon", |b| {
        b.iter(|| black_box(make_placeholder_icon(DEFAULT_ICON_SIZE)))
    });
}

fn bench_load_cache(c: &mut Criterion) {
    // Measures the fast path: reading + deserializing + checksum-verifying the
    // cache file. If the cache is cold (missing/invalid) this will return None
    // quickly; if warm it measures the deserialization + checksum cost.
    c.bench_function("load_cache", |b| {
        b.iter(|| black_box(load_cache()))
    });
}

fn bench_load_desktop_entries(c: &mut Criterion) {
    // This is the heavyweight one — scans every application dir, parses
    // .desktop files, resolves and decodes icon files. It's what "slow
    // startup" feels like when the cache is cold. Keep sample size low so
    // the whole bench suite doesn't take forever.
    let mut group = c.benchmark_group("load_desktop_entries");
    group.sample_size(10);
    group.bench_function("cold_scan", |b| {
        b.iter(|| black_box(load_desktop_entries(DEFAULT_ICON_SIZE, None)))
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_compute_checksum,
    bench_make_placeholder_icon,
    bench_load_cache,
    bench_load_desktop_entries,
);
criterion_main!(benches);
