use std::{
    fs::{create_dir_all, File},
    io::Write,
    path::PathBuf,
};

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use hyperdu_core as core;

fn build_tree(root: &std::path::Path, dirs: usize, files_per_dir: usize, file_size: usize) {
    for d in 0..dirs {
        let dir = root.join(format!("d{d}"));
        let _ = create_dir_all(&dir);
        for f in 0..files_per_dir {
            let mut fh = File::create(dir.join(format!("f{f}.bin"))).unwrap();
            fh.write_all(&vec![0u8; file_size]).unwrap();
        }
    }
}

fn bench_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("scan_vs_parallel");
    group.sample_size(10);
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();
    // Build N roots of similar size
    let make_roots = |n: usize| -> Vec<PathBuf> {
        (0..n)
            .map(|i| {
                let r = base.join(format!("root{i}"));
                create_dir_all(&r).unwrap();
                build_tree(&r, 16, 16, 1024);
                r
            })
            .collect()
    };
    let mut opt = core::Options::default();
    opt.compute_physical = false;
    opt.progress_every = 0;
    opt.threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    // Single-root baseline
    let roots1 = make_roots(1);
    group.throughput(Throughput::Elements(1));
    group.bench_function(BenchmarkId::new("single_root", 1), |b| {
        b.iter_batched(
            || roots1[0].clone(),
            |r| core::scan_directory(&r, &opt).unwrap(),
            BatchSize::SmallInput,
        )
    });

    // Parallel roots with rayon-par if enabled
    #[cfg(feature = "rayon-par")]
    {
        let roots4 = make_roots(4);
        group.bench_function(BenchmarkId::new("rayon_parallel_roots", 4), |b| {
            b.iter_batched(
                || roots4.clone(),
                |rs| core::parallel_scan(rs, &opt).unwrap(),
                BatchSize::SmallInput,
            )
        });
        group.bench_function(BenchmarkId::new("auto_parallel", 4), |b| {
            b.iter_batched(
                || roots4.clone(),
                |rs| core::auto_parallel_scan(rs, &opt).unwrap(),
                BatchSize::SmallInput,
            )
        });
        // 8 roots
        let roots8 = make_roots(8);
        group.bench_function(BenchmarkId::new("rayon_parallel_roots", 8), |b| {
            b.iter_batched(
                || roots8.clone(),
                |rs| core::parallel_scan(rs, &opt).unwrap(),
                BatchSize::SmallInput,
            )
        });
        group.bench_function(BenchmarkId::new("auto_parallel", 8), |b| {
            b.iter_batched(
                || roots8.clone(),
                |rs| core::auto_parallel_scan(rs, &opt).unwrap(),
                BatchSize::SmallInput,
            )
        });
    }
    // Internal rayon scheduler if enabled
    #[cfg(feature = "rayon-inner")]
    {
        group.bench_function(BenchmarkId::new("rayon_inner_single", 1), |b| {
            b.iter_batched(
                || roots[0].clone(),
                |r| core::scan_directory_rayon(&r, &opt).unwrap(),
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

criterion_group!(benches, bench_scan);
criterion_main!(benches);
#![allow(clippy::field_reassign_with_default)]
