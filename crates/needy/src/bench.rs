//! `needy bench`: small latency and throughput probes. They run anywhere,
//! including as an actor's driving program, and print one JSON line.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{Result, bail};
use serde_json::json;

fn stats(mut samples: Vec<f64>) -> serde_json::Value {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pick = |q: f64| {
        let i = ((samples.len() as f64 - 1.0) * q).round() as usize;
        (samples[i] * 1000.0).round() / 1000.0
    };
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    json!({
        "n": samples.len(),
        "p50_ms": pick(0.5),
        "p90_ms": pick(0.9),
        "p99_ms": pick(0.99),
        "min_ms": pick(0.0),
        "mean_ms": (mean * 1000.0).round() / 1000.0,
    })
}

/// Spawns `argv` `count` times and reports wall time from spawn to exit.
pub fn spawn(label: &str, count: usize, argv: &[String]) -> Result<()> {
    if argv.is_empty() {
        bail!("no command");
    }
    let mut samples = Vec::with_capacity(count);
    let mut failures = 0;
    for _ in 0..count {
        let start = Instant::now();
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        if !status.success() {
            failures += 1;
        }
    }
    let mut out = stats(samples);
    out["bench"] = json!("spawn");
    out["label"] = json!(label);
    out["argv"] = json!(argv.join(" "));
    out["failures"] = json!(failures);
    println!("{out}");
    Ok(())
}

/// Reads `argv`'s stdout to the end and reports bytes per second.
pub fn pipe(label: &str, argv: &[String]) -> Result<()> {
    let start = Instant::now();
    let mut child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = stdout.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
    }
    child.wait()?;
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{}",
        json!({"bench": "pipe", "label": label, "argv": argv.join(" "), "bytes": total,
               "seconds": (secs * 1000.0).round() / 1000.0,
               "mib_per_s": ((total as f64 / secs / 1048576.0) * 10.0).round() / 10.0})
    );
    Ok(())
}

/// Sequential write and read of one file, then small-file create, stat and read.
pub fn fs(label: &str, dir: &Path, mib: usize, files: usize) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let big = dir.join("needy-bench.bin");
    let chunk = vec![7u8; 1 << 20];
    let start = Instant::now();
    {
        let mut f = std::fs::File::create(&big)?;
        for _ in 0..mib {
            f.write_all(&chunk)?;
        }
        f.sync_all()?;
    }
    let write_secs = start.elapsed().as_secs_f64();
    let start = Instant::now();
    {
        let mut f = std::fs::File::open(&big)?;
        let mut buf = vec![0u8; 1 << 20];
        while f.read(&mut buf)? > 0 {}
    }
    let read_secs = start.elapsed().as_secs_f64();
    std::fs::remove_file(&big)?;

    let small = dir.join("needy-bench-small");
    std::fs::create_dir_all(&small)?;
    let start = Instant::now();
    for i in 0..files {
        std::fs::write(small.join(format!("f{i}.txt")), b"hello needy\n")?;
    }
    let create_secs = start.elapsed().as_secs_f64();
    let start = Instant::now();
    for i in 0..files {
        std::fs::metadata(small.join(format!("f{i}.txt")))?;
    }
    let stat_secs = start.elapsed().as_secs_f64();
    let start = Instant::now();
    for i in 0..files {
        std::fs::read(small.join(format!("f{i}.txt")))?;
    }
    let read_small_secs = start.elapsed().as_secs_f64();
    let start = Instant::now();
    let listed = std::fs::read_dir(&small)?.count();
    let list_secs = start.elapsed().as_secs_f64();
    std::fs::remove_dir_all(&small)?;
    let rate = |n: usize, s: f64| (n as f64 / s).round();
    println!(
        "{}",
        json!({"bench": "fs", "label": label, "dir": dir.display().to_string(),
               "write_mib_s": ((mib as f64 / write_secs) * 10.0).round() / 10.0,
               "read_mib_s": ((mib as f64 / read_secs) * 10.0).round() / 10.0,
               "create_per_s": rate(files, create_secs),
               "stat_per_s": rate(files, stat_secs),
               "read_small_per_s": rate(files, read_small_secs),
               "readdir_entries": listed,
               "readdir_ms": (list_secs * 1000.0 * 1000.0).round() / 1000.0})
    );
    Ok(())
}
