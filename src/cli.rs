// Headless mode. Also what CI runs to prove a built binary actually works.

use std::path::PathBuf;
use std::time::Duration;

use crate::pipeline::{self, PipelineOptions, Progress};
use crate::{report, DEFAULT_FORMAT, DEFAULT_QUALITY, FORMATS};

fn usage() -> String {
    format!(
        "astctool --cli -i <folder-or-zip> [-o out.tar.zst]\n\
         \x20   [--format {}] [--quality {}] [--jobs N] [--alpha-exact]\n\
         \n\
         input:  a folder of .png/.dds/.ktx/.ktx2, or a .zip of one\n\
         output: <name>.tar.zst plus report.json and astctool.log\n\
         --alpha-exact: keep flat PS2 alpha (0x80 = opaque) exact, at some colour cost\n",
        FORMATS.join("|"),
        DEFAULT_QUALITY
    )
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Returns a process exit code.
pub fn run(args: &[String]) -> i32 {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut format = DEFAULT_FORMAT.to_string();
    let mut quality = DEFAULT_QUALITY;
    let mut jobs = default_jobs();
    let mut alpha_exact = false;

    let mut it = args.iter().peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--cli" | "--selftest" => {}
            "-h" | "--help" => {
                print!("{}", usage());
                return 0;
            }
            "--version" | "-V" => {
                println!("astctool {}", env!("CARGO_PKG_VERSION"));
                return 0;
            }
            "-i" | "--input" => match it.next() {
                Some(v) => input = Some(PathBuf::from(v)),
                None => return fail("--input needs a path"),
            },
            "-o" | "--output" => match it.next() {
                Some(v) => output = Some(PathBuf::from(v)),
                None => return fail("--output needs a path"),
            },
            "--format" => match it.next() {
                Some(v) => {
                    if !FORMATS.contains(&v.as_str()) {
                        return fail(&format!("unknown format {v:?}; use {}", FORMATS.join("|")));
                    }
                    format = v.clone();
                }
                None => return fail("--format needs a value"),
            },
            "--quality" => match it.next().and_then(|v| v.parse::<i32>().ok()) {
                Some(q) if (0..=100).contains(&q) => quality = q,
                _ => return fail("--quality needs 0..100"),
            },
            "--jobs" => match it.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(j) if j >= 1 => jobs = j,
                _ => return fail("--jobs needs a positive integer"),
            },
            "--alpha-exact" => alpha_exact = true,
            other => return fail(&format!("unknown argument {other:?}")),
        }
    }

    let Some(input) = input else {
        return fail("missing --input");
    };

    let opts = PipelineOptions {
        input,
        output,
        format,
        quality,
        jobs,
        alpha_exact,
    };

    let mut last_bucket = usize::MAX;
    let mut progress = |p: Progress| match p {
        Progress::Phase(name) => println!("== {name} =="),
        Progress::Discovered {
            families,
            unsupported,
        } => println!("found {families} textures ({unsupported} unsupported)"),
        Progress::Encode {
            done,
            total,
            failed,
            eta,
            ..
        } => {
            let bucket = done * 10 / total.max(1);
            if bucket != last_bucket || done == total {
                last_bucket = bucket;
                println!(
                    "  encode {done}/{total} ({failed} failed) ETA {}",
                    fmt_eta(eta)
                );
            }
        }
        Progress::Validate { done, total, failed } => {
            println!("validate {done}/{total} ({failed} not ok)")
        }
        Progress::Log(msg) => println!("  {msg}"),
    };

    match pipeline::run(&opts, &mut progress) {
        Ok(rep) => {
            match report::write_report(&rep, std::path::Path::new(&rep.output)) {
                Ok(w) => {
                    println!("wrote {}", w.report_json.display());
                    println!("wrote {}", w.log.display());
                }
                Err(e) => eprintln!("warning: could not write report: {e}"),
            }
            println!(
                "done: {}/{} textures, {} failed in {:.1}s -> {}",
                rep.succeeded,
                rep.total_textures,
                rep.failed_count,
                rep.elapsed_ms as f64 / 1000.0,
                rep.output
            );
            0
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

fn fail(msg: &str) -> i32 {
    eprintln!("error: {msg}");
    eprintln!("{}", usage());
    2
}

fn fmt_eta(eta: Option<Duration>) -> String {
    match eta {
        None => "--".into(),
        Some(d) => {
            let secs = d.as_secs();
            if secs >= 60 {
                format!("{}m{:02}s", secs / 60, secs % 60)
            } else {
                format!("{secs}s")
            }
        }
    }
}
