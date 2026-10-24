//! Measures the native mirror engine against robocopy. Build-only tooling: it isn't part
//! of Replica.
//!
//!     cargo run --release -p replica-core --example mirror_bench [-- <small files> <large MB>]
//!
//! It builds a tree in a temp folder (by default 50,000 small files in 500 folders and three
//! 200 MB files), then times three runs of each engine into its own target: the first copy, a
//! run with no change, and a run after a small change (20 files changed, 10 added, 10 deleted).
//! Everything is in one temp folder, removed at the end.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use replica_core::cancel::CancelToken;
use replica_core::job::Source;
use replica_core::mirror::{Compare, MirrorRequest, mirror};

fn main() {
    let mut args = std::env::args().skip(1);
    let small: usize = args.next().map_or(50_000, |a| a.parse().unwrap());
    let large_mb: usize = args.next().map_or(200, |a| a.parse().unwrap());
    let temp = tempfile::tempdir().unwrap();
    let src = temp.path().join("src");
    println!(
        "Building {small} small files and 3 x {large_mb} MB in {}",
        temp.path().display()
    );
    let t = Instant::now();
    build_tree(&src, small, large_mb);
    println!("  built in {:.1?}\n", t.elapsed());

    let mut rows = Vec::new();
    let native_target = temp.path().join("native");
    let robo_target = temp.path().join("robocopy");
    let robo_mt_target = temp.path().join("robocopy-mt");
    for (label, step) in [("first copy", 0), ("no change", 1), ("small change", 2)] {
        if step == 2 {
            small_change(&src);
        }
        let native = time(|| {
            let out = mirror(
                &MirrorRequest {
                    sources: &[Source {
                        path: src.clone(),
                        exclude: Vec::new(),
                    }],
                    target: &native_target,
                    compare: Compare::SizeAndTime,
                    delete_limit_percent: 100,
                    case_insensitive: true,
                    exclude_paths: &[],
                },
                &mut |_| {},
                &CancelToken::new(),
            )
            .unwrap();
            assert!(!out.partial(), "{:?}", out.failed);
            format!(
                "copied {}, deleted {}, unchanged {}",
                out.copied, out.deleted, out.unchanged
            )
        });
        let robo = time(|| robocopy(&src, &robo_target.join("src"), None));
        let robo_mt = time(|| robocopy(&src, &robo_mt_target.join("src"), Some(8)));
        rows.push((label, native, robo, robo_mt));
    }

    println!("| Run | Native | robocopy /MIR | robocopy /MIR /MT:8 |");
    println!("|---|---|---|---|");
    for (label, native, robo, robo_mt) in &rows {
        println!(
            "| {label} | {:.2?} ({}) | {:.2?} ({}) | {:.2?} ({}) |",
            native.0, native.1, robo.0, robo.1, robo_mt.0, robo_mt.1
        );
    }
}

fn time(f: impl FnOnce() -> String) -> (Duration, String) {
    let start = Instant::now();
    let what = f();
    (start.elapsed(), what)
}

#[cfg(windows)]
fn robocopy(src: &Path, dst: &Path, threads: Option<u32>) -> String {
    let mut cmd = std::process::Command::new("robocopy");
    cmd.arg(src).arg(dst).args([
        "/MIR",
        "/COPY:DAT",
        "/DCOPY:DAT",
        "/R:0",
        "/W:0",
        "/NP",
        "/NFL",
        "/NDL",
        "/NJH",
        "/NJS",
        "/XJ",
    ]);
    if let Some(n) = threads {
        cmd.arg(format!("/MT:{n}"));
    }
    let status = cmd.status().unwrap();
    let code = status.code().unwrap_or(-1);
    assert!(code < 8, "robocopy failed with exit code {code}");
    format!("exit {code}")
}

#[cfg(not(windows))]
fn robocopy(_: &Path, _: &Path, _: Option<u32>) -> String {
    "n/a".into()
}

fn build_tree(src: &Path, small: usize, large_mb: usize) {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    for i in 0..small {
        let dir = src.join(format!("d{:03}", i % 500));
        if i < 500 {
            fs::create_dir_all(&dir).unwrap();
        }
        let size = 1024 + (next(&mut seed) % 15_360) as usize;
        fs::write(dir.join(format!("f{i:05}.dat")), noise(&mut seed, size)).unwrap();
    }
    fs::create_dir_all(src.join("large")).unwrap();
    for i in 0..3 {
        let mut file = std::io::BufWriter::new(
            fs::File::create(src.join(format!("large/big{i}.bin"))).unwrap(),
        );
        for _ in 0..large_mb {
            file.write_all(&noise(&mut seed, 1 << 20)).unwrap();
        }
    }
}

fn small_change(src: &Path) {
    let mut seed = 99u64;
    for i in 0..20 {
        fs::write(
            src.join(format!("d{:03}/f{i:05}.dat", i % 500)),
            noise(&mut seed, 4096),
        )
        .unwrap();
    }
    for i in 0..10 {
        fs::write(src.join(format!("d000/new{i}.dat")), noise(&mut seed, 2048)).unwrap();
    }
    for i in 100..110 {
        let p: PathBuf = src.join(format!("d{:03}/f{i:05}.dat", i % 500));
        fs::remove_file(p).unwrap();
    }
}

fn next(x: &mut u64) -> u64 {
    *x ^= *x << 13;
    *x ^= *x >> 7;
    *x ^= *x << 17;
    *x
}

fn noise(seed: &mut u64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        out.extend_from_slice(&next(seed).to_le_bytes());
    }
    out.truncate(len);
    out
}
