// Writes report.json and astctool.log next to the produced archive.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::pipeline::Report;

pub struct Written {
    pub report_json: PathBuf,
    pub log: PathBuf,
}

pub fn write_report(report: &Report, archive: &Path) -> Result<Written> {
    let dir = archive.parent().unwrap_or_else(|| Path::new("."));
    let report_json = dir.join("report.json");
    let log_path = dir.join("astctool.log");

    let json = serde_json::to_string_pretty(report).context("serialize report")?;
    std::fs::write(&report_json, json)
        .with_context(|| format!("write {}", report_json.display()))?;

    let mut text = String::new();
    text.push_str("astctool\n");
    text.push_str(&format!("input:  {}\n", report.input));
    text.push_str(&format!("output: {}\n", report.output));
    text.push_str(&format!(
        "format: {}  quality: {}  jobs: {}x{}\n",
        report.format, report.quality, report.jobs, report.threads_per_process
    ));
    text.push_str(&format!(
        "textures: {} ok, {} failed, {} unsupported\n",
        report.succeeded,
        report.failed_count,
        report.unsupported.len()
    ));
    text.push_str(&format!(
        "archive: {} bytes compressed, {} bytes tar, {} ktx\n",
        report.archive.compressed_bytes,
        report.archive.decompressed_bytes,
        report.archive.ktx_files
    ));
    text.push_str(&format!("elapsed: {:.1}s\n", report.elapsed_ms as f64 / 1000.0));
    text.push_str("\n--- log ---\n");
    for line in &report.log {
        text.push_str(line);
        text.push('\n');
    }
    if !report.failures.is_empty() {
        text.push_str("\n--- failures ---\n");
        for f in &report.failures {
            text.push_str(&format!("[{}] {}: {}\n", f.stage, f.path, f.error));
        }
    }
    if !report.unsupported.is_empty() {
        text.push_str("\n--- unsupported (skipped) ---\n");
        for p in &report.unsupported {
            text.push_str(p);
            text.push('\n');
        }
    }

    std::fs::write(&log_path, text)
        .with_context(|| format!("write {}", log_path.display()))?;

    Ok(Written {
        report_json,
        log: log_path,
    })
}
