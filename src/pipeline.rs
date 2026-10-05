// Orchestrates scan -> encode -> validate -> package -> verify, streaming
// progress and never bailing on individual texture failures.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use tempfile::TempDir;

use crate::{contract, input, kram, ktx, scan, tarzstd};

#[derive(Debug, Clone)]
pub struct PipelineOptions {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub format: String,
    pub quality: i32,
    /// How many kram processes to run at once.
    pub jobs: usize,
    /// Re-encode blocks whose flat or two-level alpha (e.g. PS2 opaque 0x80)
    /// decodes off, at some cost in colour on those blocks.
    pub alpha_exact: bool,
}

impl PipelineOptions {
    pub fn output_path(&self) -> PathBuf {
        self.output
            .clone()
            .unwrap_or_else(|| default_output(&self.input))
    }
}

pub fn default_output(input: &Path) -> PathBuf {
    if input.is_dir() {
        let name = input
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "texturepack".into());
        input
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{name}.tar.zst"))
    } else {
        input.with_extension("tar.zst")
    }
}

#[derive(Debug, Clone)]
pub enum Progress {
    Phase(String),
    Discovered {
        families: usize,
        unsupported: usize,
    },
    Encode {
        done: usize,
        total: usize,
        failed: usize,
        eta: Option<Duration>,
        current: String,
    },
    Validate {
        done: usize,
        total: usize,
        failed: usize,
    },
    Log(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    pub path: String,
    pub stage: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveInfo {
    pub path: String,
    pub compressed_bytes: u64,
    pub decompressed_bytes: u64,
    pub ktx_files: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub input: String,
    pub replacements_root: String,
    pub output: String,
    pub format: String,
    pub quality: i32,
    pub alpha_exact: bool,
    pub jobs: usize,
    pub threads_per_process: usize,
    pub total_textures: usize,
    pub succeeded: usize,
    pub failed_count: usize,
    pub failures: Vec<Failure>,
    pub unsupported: Vec<String>,
    pub archive: ArchiveInfo,
    pub elapsed_ms: u128,
    pub kram_version: String,
    #[serde(skip)]
    pub log: Vec<String>,
}

struct Job {
    out: PathBuf,
    display: String,
    argv: Vec<OsString>,
}

/// Run the whole pipeline. Fatal errors (bad input, no kram, write failure)
/// return Err; texture-level problems are collected in the report.
pub fn run(opts: &PipelineOptions, progress: &mut dyn FnMut(Progress)) -> Result<Report> {
    let started = Instant::now();
    let mut log = Vec::new();
    let note = |log: &mut Vec<String>, progress: &mut dyn FnMut(Progress), msg: String| {
        log::info!("{msg}");
        log.push(msg.clone());
        progress(Progress::Log(msg));
    };

    progress(Progress::Phase("Preparing kram".into()));
    let kram_path = kram::ensure_kram().context("extract embedded kram")?;
    kram::check_available(&kram_path)?;
    let version = kram::kram_version(&kram_path).unwrap_or_default();
    note(&mut log, progress, format!("using {version}"));

    progress(Progress::Phase("Finding textures".into()));
    let resolved = input::resolve(&opts.input)?;
    let root = resolved.replacements.clone();
    let s = scan::discover(&root)?;
    if s.families.is_empty() {
        bail!("no convertible textures found under {}", root.display());
    }
    let unsupported: Vec<String> = s
        .unsupported
        .iter()
        .map(|p| rel_str(p, &root))
        .collect();
    note(
        &mut log,
        progress,
        format!(
            "found {} texture families, {} unsupported files",
            s.families.len(),
            unsupported.len()
        ),
    );
    progress(Progress::Discovered {
        families: s.families.len(),
        unsupported: unsupported.len(),
    });

    let work = TempDir::new().context("create work dir")?;
    let converted = work.path().join(tarzstd::PREFIX);
    std::fs::create_dir_all(&converted).context("create staging dir")?;

    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let total = s.families.len();
    let concurrency = opts.jobs.max(1).min(total);
    let threads_per = (cores / concurrency).max(1).min(16);

    // Build jobs.
    let mut jobs: Vec<Job> = Vec::with_capacity(total);
    for fam in &s.families {
        let raw: Vec<(u32, PathBuf)> = fam
            .sidecars
            .iter()
            .map(|(l, p)| (*l, p.clone()))
            .collect();
        let (sidecars, fell_back) = kram::usable_sidecars(&raw);
        if fell_back {
            note(
                &mut log,
                progress,
                format!(
                    "{}: sidecar chain unusable, generating mips from base",
                    fam.rel_base.display()
                ),
            );
        }
        let out = converted.join(fam.rel_base.with_extension("ktx"));
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let argv = kram::build_argv(
            &fam.base,
            &sidecars,
            &out,
            &opts.format,
            opts.quality,
            threads_per,
            opts.alpha_exact,
        );
        jobs.push(Job {
            out,
            display: fam.rel_base.display().to_string(),
            argv,
        });
    }

    // Encode in parallel, continue on error.
    progress(Progress::Phase("Encoding".into()));
    let mut encode_ok = vec![false; total];
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, std::result::Result<(), String>)>();
    let mut failures: Vec<Failure> = Vec::new();

    std::thread::scope(|scope| {
        for _ in 0..concurrency {
            let tx = tx.clone();
            let next = &next;
            let jobs = &jobs;
            let kram_path = &kram_path;
            scope.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= total {
                    break;
                }
                let res = kram::run_encode(kram_path, &jobs[i].argv);
                let _ = tx.send((i, res));
            });
        }
        drop(tx);

        let enc_start = Instant::now();
        let mut done = 0usize;
        let mut failed = 0usize;
        while let Ok((i, res)) = rx.recv() {
            done += 1;
            match res {
                Ok(()) => encode_ok[i] = true,
                Err(e) => {
                    failed += 1;
                    failures.push(Failure {
                        path: jobs[i].display.clone(),
                        stage: "encode".into(),
                        error: e,
                    });
                }
            }
            let elapsed = enc_start.elapsed().as_secs_f64().max(1e-6);
            let rate = done as f64 / elapsed;
            let remaining = total - done;
            let eta = if rate > 0.0 && remaining > 0 {
                Some(Duration::from_secs_f64(remaining as f64 / rate))
            } else {
                None
            };
            progress(Progress::Encode {
                done,
                total,
                failed,
                eta,
                current: jobs[i].display.clone(),
            });
        }
    });

    // Validate outputs.
    progress(Progress::Phase("Validating".into()));
    let mut ok_outputs: Vec<&Job> = Vec::new();
    for (i, job) in jobs.iter().enumerate() {
        if !encode_ok[i] {
            continue;
        }
        match ktx::validate_file(&job.out) {
            Ok(()) => ok_outputs.push(job),
            Err(e) => {
                failures.push(Failure {
                    path: job.display.clone(),
                    stage: "validate".into(),
                    error: e,
                });
            }
        }
    }
    let ktx_count = ok_outputs.len();
    progress(Progress::Validate {
        done: ok_outputs.len(),
        total,
        failed: total - ok_outputs.len(),
    });
    note(
        &mut log,
        progress,
        format!("{ktx_count}/{total} textures encoded and validated"),
    );

    if ktx_count == 0 {
        bail!("every texture failed; nothing to package");
    }

    // Package.
    progress(Progress::Phase("Packaging".into()));
    let out_path = opts.output_path();
    let stats = tarzstd::write_archive(&out_path, &converted)
        .with_context(|| format!("write archive {}", out_path.display()))?;

    // Verify the archive against the consumer contract.
    progress(Progress::Phase("Verifying archive".into()));
    let summary = contract::validate_tar_zst(&out_path, Some(ktx_count))
        .map_err(|e| anyhow::anyhow!("pack failed consumer contract check: {e}"))?;
    note(
        &mut log,
        progress,
        format!(
            "archive ok: {} entries, {} ktx, {} MB -> {} MB",
            summary.entries,
            summary.ktx_files,
            summary.decompressed_size / 1_000_000,
            stats.compressed_bytes / 1_000_000
        ),
    );

    let failed_count = failures.len();
    let report = Report {
        input: opts.input.display().to_string(),
        replacements_root: root.display().to_string(),
        output: out_path.display().to_string(),
        format: opts.format.clone(),
        quality: opts.quality,
        alpha_exact: opts.alpha_exact,
        jobs: concurrency,
        threads_per_process: threads_per,
        total_textures: total,
        succeeded: ktx_count,
        failed_count,
        failures,
        unsupported,
        archive: ArchiveInfo {
            path: out_path.display().to_string(),
            compressed_bytes: stats.compressed_bytes,
            decompressed_bytes: summary.decompressed_size,
            ktx_files: summary.ktx_files,
        },
        elapsed_ms: started.elapsed().as_millis(),
        kram_version: version,
        log,
    };
    Ok(report)
}

fn rel_str(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}
