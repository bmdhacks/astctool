// Embeds the kram binary and drives it as a subprocess. One process per
// texture keeps progress, errors and paths-with-spaces trivial.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::{MAX_MIP_LEVELS};

/// The kram executable, baked in at compile time (see build.rs).
pub const KRAM_BYTES: &[u8] = include_bytes!(env!("ASTCTOOL_KRAM_BIN"));

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "kram.exe"
    } else {
        "kram"
    }
}

/// Extract kram to a per-user cache dir, keyed by content hash.
pub fn ensure_kram() -> Result<PathBuf> {
    let mut hasher = Sha256::new();
    hasher.update(KRAM_BYTES);
    let digest = hasher.finalize();
    let tag: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();

    let base = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("astctool");
    let dir = base.join(format!("kram-{tag}"));
    let path = dir.join(exe_name());

    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() as usize == KRAM_BYTES.len() {
            return Ok(path);
        }
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create cache dir {}", dir.display()))?;
    let tmp = dir.join(format!("{}.tmp", exe_name()));
    std::fs::write(&tmp, KRAM_BYTES).with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .context("chmod kram")?;
    }
    std::fs::rename(&tmp, &path).context("install kram")?;
    Ok(path)
}

/// Which sidecars are actually usable. Invalid chains fall back to generation.
pub fn usable_sidecars(sidecars: &[(u32, PathBuf)]) -> (Vec<(u32, PathBuf)>, bool) {
    if sidecars.is_empty() {
        return (Vec::new(), false);
    }
    let count = sidecars.iter().map(|(l, _)| *l).max().unwrap_or(0) + 1;
    if count > MAX_MIP_LEVELS {
        return (Vec::new(), true);
    }
    for level in 1..count {
        if !sidecars.iter().any(|(l, _)| *l == level) {
            return (Vec::new(), true);
        }
    }
    (sidecars.to_vec(), false)
}

/// Build the `encode` argv for one family.
pub fn build_argv(
    job_base: &Path,
    sidecars: &[(u32, PathBuf)],
    out: &Path,
    format: &str,
    quality: i32,
    threads: usize,
) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    a.push("encode".into());
    a.push("-i".into());
    a.push(job_base.into());
    if sidecars.is_empty() {
        a.push("-f".into());
        a.push(format.into());
        a.push("-quality".into());
        a.push(quality.to_string().into());
        a.push("-mipcount".into());
        a.push(MAX_MIP_LEVELS.to_string().into());
    } else {
        for (level, path) in sidecars {
            a.push("-mip".into());
            a.push(level.to_string().into());
            a.push(path.into());
        }
        a.push("-f".into());
        a.push(format.into());
        a.push("-quality".into());
        a.push(quality.to_string().into());
    }
    a.push("-j".into());
    a.push(threads.max(1).to_string().into());
    a.push("-o".into());
    a.push(out.into());
    a
}

/// Run one encode. On failure, returns kram's output (often empty, kram is quiet).
pub fn run_encode(kram: &Path, argv: &[OsString]) -> std::result::Result<(), String> {
    let out = Command::new(kram)
        .args(argv)
        .output()
        .map_err(|e| format!("spawn kram: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let mut msg = String::new();
    msg.push_str(&String::from_utf8_lossy(&out.stdout));
    msg.push_str(&String::from_utf8_lossy(&out.stderr));
    let trimmed = msg.trim();
    if trimmed.is_empty() {
        Err(format!("kram exited with {}", out.status))
    } else {
        Err(trimmed.to_string())
    }
}

pub fn kram_version(kram: &Path) -> Result<String> {
    let out = Command::new(kram)
        .output()
        .context("run kram")?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().next().unwrap_or_default().to_string())
}

pub fn check_available(kram: &Path) -> Result<()> {
    if !kram.is_file() {
        bail!("kram binary missing at {}", kram.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_chain_validity() {
        let sc = vec![(1, PathBuf::from("a")), (2, PathBuf::from("b"))];
        assert_eq!(usable_sidecars(&sc), (sc.clone(), false));

        let hole = vec![(1, PathBuf::from("a")), (3, PathBuf::from("c"))];
        assert_eq!(usable_sidecars(&hole), (Vec::new(), true));
    }

    #[test]
    fn argv_uses_sidecars_when_valid() {
        let a = build_argv(
            Path::new("base.png"),
            &[(1, PathBuf::from("m1.png"))],
            Path::new("out.ktx"),
            "astc6x6",
            98,
            1,
        );
        let s: Vec<String> = a.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(s.contains(&"-mip".to_string()));
        assert!(!s.contains(&"-mipcount".to_string()));
    }

    #[test]
    fn argv_generates_without_sidecars() {
        let a = build_argv(
            Path::new("base.png"),
            &[],
            Path::new("out.ktx"),
            "astc6x6",
            98,
            1,
        );
        let s: Vec<String> = a.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(s.contains(&"-mipcount".to_string()));
    }
}
