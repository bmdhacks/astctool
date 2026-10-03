use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regex::Regex;

/// Extensions kram can read.
pub fn is_kram_input(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "dds" | "ktx" | "ktx2")
    )
}

/// A base texture plus any explicit `-mipN` sidecar levels.
pub struct Family {
    /// Absolute path to the base image.
    pub base: PathBuf,
    /// Path relative to the replacements root, used to name the output `.ktx`.
    pub rel_base: PathBuf,
    /// level (>=1) -> absolute sidecar path.
    pub sidecars: BTreeMap<u32, PathBuf>,
}

pub struct Scan {
    pub families: Vec<Family>,
    pub unsupported: Vec<PathBuf>,
}

/// Walk `root` and group files into mip families, same rule as migrate.py:
/// siblings sharing a stem after stripping a trailing `-mipN`.
pub fn discover(root: &Path) -> Result<Scan> {
    let mip_re = Regex::new(r"(?i)-mip(\d+)$").unwrap();

    let mut files = Vec::new();
    let mut unsupported = Vec::new();
    walk(root, &mut files)?;

    // (parent, stem-without-mip) -> files
    let mut groups: BTreeMap<(PathBuf, String), Vec<PathBuf>> = BTreeMap::new();
    for f in files {
        if is_kram_input(&f) {
            let stem = f
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let stripped = mip_re.replace(&stem, "").to_string();
            let parent = f.parent().unwrap_or(root).to_path_buf();
            groups.entry((parent, stripped)).or_default().push(f);
        } else {
            unsupported.push(f);
        }
    }

    let mut families = Vec::new();
    for ((_, _), group) in groups {
        let base = group
            .iter()
            .find(|f| {
                f.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| !mip_re.is_match(s))
                    .unwrap_or(false)
            })
            .cloned();
        let Some(base) = base else {
            // Only sidecars, no base to convert. Treat as unsupported.
            unsupported.extend(group);
            continue;
        };

        let mut sidecars = BTreeMap::new();
        for f in &group {
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            if let Some(c) = mip_re.captures(stem) {
                if let Ok(level) = c[1].parse::<u32>() {
                    sidecars.insert(level, f.clone());
                }
            }
        }

        let rel_base = base
            .strip_prefix(root)
            .with_context(|| format!("{} not under {}", base.display(), root.display()))?
            .to_path_buf();

        families.push(Family {
            base,
            rel_base,
            sidecars,
        });
    }

    families.sort_by(|a, b| a.rel_base.cmp(&b.rel_base));
    unsupported.sort();
    Ok(Scan {
        families,
        unsupported,
    })
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        bail!("not a directory: {}", dir.display());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("read dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let ft = entry.file_type().ok();
        let path = entry.path();
        // Do not follow symlinks; avoids cycles and surprise traversal.
        match ft {
            Some(ft) if ft.is_dir() => walk(&path, out)?,
            Some(ft) if ft.is_file() => out.push(path),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert!(is_kram_input(Path::new("a.PNG")));
        assert!(is_kram_input(Path::new("a.Dds")));
        assert!(is_kram_input(Path::new("a.ktx2")));
        assert!(!is_kram_input(Path::new("a.jpg")));
        assert!(!is_kram_input(Path::new("a")));
    }

    #[test]
    fn groups_base_with_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("rock.png"), b"x").unwrap();
        std::fs::write(root.join("rock-mip1.png"), b"x").unwrap();
        std::fs::write(root.join("rock-mip2.png"), b"x").unwrap();
        std::fs::write(root.join("other.png"), b"x").unwrap();
        std::fs::write(root.join("readme.txt"), b"x").unwrap();

        let scan = discover(root).unwrap();
        assert_eq!(scan.families.len(), 2);
        assert_eq!(scan.unsupported.len(), 1);
        let rock = scan
            .families
            .iter()
            .find(|f| f.rel_base == Path::new("rock.png"))
            .unwrap();
        assert_eq!(rock.sidecars.len(), 2);
    }

    #[test]
    fn orphan_sidecars_are_unsupported() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("lonely-mip1.png"), b"x").unwrap();
        let scan = discover(root).unwrap();
        assert!(scan.families.is_empty());
        assert_eq!(scan.unsupported.len(), 1);
    }
}
