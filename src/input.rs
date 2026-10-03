// Resolve the input into the directory that holds the texture tree: either a
// folder, or a zip that we unpack to a temp dir first.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::scan;

pub struct Resolved {
    /// Directory whose contents become `replacements/` in the archive.
    pub replacements: PathBuf,
    /// Temp dir backing a zip input; keep alive for the pipeline's duration.
    pub _temp: Option<tempfile::TempDir>,
}

pub fn resolve(path: &Path) -> Result<Resolved> {
    if path.is_dir() {
        let replacements = find_replacements_root(path)?;
        Ok(Resolved {
            replacements,
            _temp: None,
        })
    } else if path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        let temp = tempfile::Builder::new()
            .prefix("astctool-zip-")
            .tempdir()
            .context("create temp dir")?;
        extract_zip(path, temp.path())?;
        let replacements = find_replacements_root(temp.path())?;
        Ok(Resolved {
            replacements,
            _temp: Some(temp),
        })
    } else {
        bail!(
            "input must be a folder or a .zip file: {}",
            path.display()
        )
    }
}

/// Prefer a `replacements` subdir if present, else the dir itself when it
/// contains textures somewhere below it (mirrors migrate.py's fallback).
fn find_replacements_root(base: &Path) -> Result<PathBuf> {
    let direct = base.join("replacements");
    if direct.is_dir() {
        return Ok(direct);
    }
    if contains_textures(base)? {
        return Ok(base.to_path_buf());
    }
    bail!("no textures found under {}", base.display())
}

fn contains_textures(dir: &Path) -> Result<bool> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("read dir {}", dir.display()))?
        .filter_map(|e| e.ok())
    {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let p = entry.path();
        if ft.is_file() && scan::is_kram_input(&p) {
            return Ok(true);
        }
        if ft.is_dir() && contains_textures(&p)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn extract_zip(zip_path: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("open {}", zip_path.display()))?;
    let mut archive =
        zip::ZipArchive::new(io::BufReader::new(file)).context("read zip archive")?;

    let mut skipped = 0usize;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("read zip entry")?;
        let Some(rel) = entry.enclosed_name() else {
            // Absolute, `..`, or otherwise unsafe entry name.
            skipped += 1;
            continue;
        };
        let out_path = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .with_context(|| format!("mkdir {}", out_path.display()))?;
        } else {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("mkdir {}", parent.display()))?;
            }
            let mut out = std::fs::File::create(&out_path)
                .with_context(|| format!("create {}", out_path.display()))?;
            io::copy(&mut entry, &mut out)
                .with_context(|| format!("extract {}", out_path.display()))?;
        }
    }
    if skipped > 0 {
        log::warn!("skipped {skipped} unsafe zip entries");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_with_replacements_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let rep = tmp.path().join("replacements");
        std::fs::create_dir_all(&rep).unwrap();
        std::fs::write(rep.join("a.png"), b"x").unwrap();
        let r = resolve(tmp.path()).unwrap();
        assert_eq!(r.replacements, rep);
    }

    #[test]
    fn folder_that_is_itself_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.png"), b"x").unwrap();
        let r = resolve(tmp.path()).unwrap();
        assert_eq!(r.replacements, tmp.path());
    }

    #[test]
    fn empty_folder_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve(tmp.path()).is_err());
    }
}
