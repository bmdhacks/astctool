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

use regex::Regex;

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
    /// `output` if given, else [`default_output`].
    pub fn output_path(&self, serial: Option<&str>) -> PathBuf {
        self.output
            .clone()
            .unwrap_or_else(|| default_output(&self.input, serial))
    }
}

/// Next to the input, named after it. The archive holds only `replacements/`,
/// so the file name is the one place ARMSX2 can read the game's serial from
/// when no game is running: when the input's name lacks one, `serial` goes in
/// front ("SLUS-21447-mypack.tar.zst"), or replaces a generic name like
/// "replacements" outright.
pub fn default_output(input: &Path, serial: Option<&str>) -> PathBuf {
    let (dir, stem) = if input.is_dir() {
        let name = input
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "texturepack".into());
        (input.parent().unwrap_or_else(|| Path::new(".")), name)
    } else {
        let name = input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "texturepack".into());
        (input.parent().unwrap_or_else(|| Path::new(".")), name)
    };
    let name = match serial {
        Some(serial) if find_serial(&stem).is_none() => {
            if ["replacements", "textures", "texturepack"]
                .iter()
                .any(|g| stem.eq_ignore_ascii_case(g))
            {
                serial.to_string()
            } else {
                format!("{serial}-{stem}")
            }
        }
        _ => stem,
    };
    dir.join(format!("{name}.tar.zst"))
}

/// A PS2 serial in `text` ("SLUS-21447", "slus_214.47", "SLUS 21447"),
/// normalized to "SLUS-21447". Same shape ARMSX2's importer looks for.
pub fn find_serial(text: &str) -> Option<String> {
    let re = Regex::new(r"(?i)(?:^|[^a-z])([a-z]{4})[-_ ]?(\d{3})[._ ]?(\d{2})(?:$|\D)").unwrap();
    let c = re.captures(text)?;
    Some(format!("{}-{}{}", c[1].to_ascii_uppercase(), &c[2], &c[3]))
}

/// The serial for an input: from the folders the textures sit in (the
/// innermost first, so a pack zipped as `SLUS-21447/replacements/` is found),
/// then from the input's own name, then from a folder inside the texture tree
/// when exactly one serial appears there (`mypack/SLUS-21447/replacements/`).
fn serial_for(input: &Path, replacements: &Path) -> Option<String> {
    replacements
        .ancestors()
        .filter_map(|p| p.file_name())
        .find_map(|n| find_serial(&n.to_string_lossy()))
        .or_else(|| input.file_name().and_then(|n| find_serial(&n.to_string_lossy())))
        .or_else(|| {
            let mut found = std::collections::BTreeSet::new();
            serials_below(replacements, &mut found);
            if found.len() == 1 {
                found.into_iter().next()
            } else {
                None
            }
        })
}

fn serials_below(dir: &Path, found: &mut std::collections::BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if let Some(serial) = find_serial(&entry.file_name().to_string_lossy()) {
                found.insert(serial);
            }
            serials_below(&entry.path(), found);
        }
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
    pub serial: Option<String>,
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
    let serial = serial_for(&opts.input, &root);
    match &serial {
        Some(serial) => note(&mut log, progress, format!("game serial: {serial}")),
        None => note(
            &mut log,
            progress,
            "no game serial in the input's name or folders; name the pack after the game's \
             serial (e.g. SLUS-21447.tar.zst) so ARMSX2 knows which game it is for"
                .to_string(),
        ),
    }
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
    let out_path = opts.output_path(serial.as_deref());
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
        serial: serial.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_serials_in_the_usual_spellings() {
        assert_eq!(find_serial("SLUS-21447").as_deref(), Some("SLUS-21447"));
        assert_eq!(find_serial("slus_214.47").as_deref(), Some("SLUS-21447"));
        assert_eq!(find_serial("GH2 SLES 52187 hd").as_deref(), Some("SLES-52187"));
        assert_eq!(find_serial("replacements"), None);
        assert_eq!(find_serial("pack-v123456"), None);
        assert_eq!(find_serial("XSLUS-21447"), None);
    }

    #[test]
    fn serial_comes_from_the_texture_folders() {
        let root = Path::new("/packs/my-pack/SLUS-21447/replacements");
        assert_eq!(
            serial_for(Path::new("/packs/my-pack"), root).as_deref(),
            Some("SLUS-21447")
        );
        assert_eq!(
            serial_for(Path::new("/dl/SLES-52187.zip"), Path::new("/tmp/x/replacements")).as_deref(),
            Some("SLES-52187")
        );
        assert_eq!(serial_for(Path::new("/dl/pack.zip"), Path::new("/tmp/x")), None);
    }

    #[test]
    fn serial_comes_from_one_folder_inside_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("mypack");
        std::fs::create_dir_all(root.join("SLUS-21447/replacements/sub")).unwrap();
        assert_eq!(
            serial_for(Path::new("/dl/mypack.zip"), &root).as_deref(),
            Some("SLUS-21447")
        );
        // Two games' folders: no way to pick, so no serial.
        std::fs::create_dir_all(root.join("SLES-52187/replacements")).unwrap();
        assert_eq!(serial_for(Path::new("/dl/mypack.zip"), &root), None);
    }

    #[test]
    fn default_name_carries_the_serial() {
        let s = Some("SLUS-21447");
        // A file input is never a dir, so these exercise the file branch.
        assert_eq!(
            default_output(Path::new("/dl/gh2-hd.zip"), s),
            Path::new("/dl/SLUS-21447-gh2-hd.tar.zst")
        );
        assert_eq!(
            default_output(Path::new("/dl/replacements.zip"), s),
            Path::new("/dl/SLUS-21447.tar.zst")
        );
        // Already named for the game: left alone.
        assert_eq!(
            default_output(Path::new("/dl/SLUS-21447 hd.zip"), s),
            Path::new("/dl/SLUS-21447 hd.tar.zst")
        );
        assert_eq!(
            default_output(Path::new("/dl/gh2-hd.zip"), None),
            Path::new("/dl/gh2-hd.tar.zst")
        );
    }
}
