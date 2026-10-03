// Writes the GNU tar stream (root "./" + "replacements/**") into zstd
// level 19 with a 128MB window, matching the published corpus contract.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tar::{Builder, EntryType, Header};
use zstd::stream::raw::CParameter;

pub const PREFIX: &str = "replacements";
const ZSTD_LEVEL: i32 = 19;
const ZSTD_WINDOW_LOG: u32 = 27;

#[derive(Debug, Clone, Default)]
pub struct ArchiveStats {
    pub ktx_files: usize,
    pub uncompressed_bytes: u64,
    pub compressed_bytes: u64,
    pub decompressed_size: u64,
}

enum Kind {
    Dir,
    File(PathBuf),
}

struct Entry {
    archive_path: String,
    kind: Kind,
}

/// Pack `replacements_root`'s tree under a `replacements/` prefix into `out`.
pub fn write_archive(out: &Path, replacements_root: &Path) -> Result<ArchiveStats> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create output dir {}", parent.display()))?;
    }

    let mut entries = Vec::new();
    collect(replacements_root, replacements_root, &mut entries)?;
    // Parent paths sort before their children, so dirs land first naturally.
    entries.sort_by(|a, b| a.archive_path.cmp(&b.archive_path));

    let file = File::create(out).with_context(|| format!("create {}", out.display()))?;
    let buf = BufWriter::new(file);
    let mut enc = zstd::stream::write::Encoder::new(buf, ZSTD_LEVEL)
        .context("init zstd encoder")?;
    enc.set_parameter(CParameter::WindowLog(ZSTD_WINDOW_LOG))
        .context("set zstd window log")?;
    // --long in the zstd CLI enables long-distance matching; set it too so we
    // match the reference compressor's behavior.
    let _ = enc.set_parameter(CParameter::EnableLongDistanceMatching(true));

    let mut ktx_files = 0usize;
    let mut uncompressed_bytes = 0u64;

    {
        let mut builder = Builder::new(enc);

        // Root directory entry, exactly "./", which the consumer strips.
        append_dir(&mut builder, "./")?;
        append_dir(&mut builder, &format!("{PREFIX}/"))?;

        for e in &entries {
            match &e.kind {
                Kind::Dir => append_dir(&mut builder, &format!("{}/", e.archive_path))?,
                Kind::File(disk) => {
                    let len = append_file(&mut builder, disk, &e.archive_path)?;
                    uncompressed_bytes += len;
                    if e.archive_path.to_lowercase().ends_with(".ktx") {
                        ktx_files += 1;
                    }
                }
            }
        }

        enc = builder.into_inner().context("finish tar stream")?;
    }

    let mut buf = enc.finish().context("finish zstd stream")?;
    buf.flush().context("flush archive")?;
    drop(buf);

    let compressed_bytes = fs::metadata(out)
        .with_context(|| format!("stat {}", out.display()))?
        .len();

    Ok(ArchiveStats {
        ktx_files,
        uncompressed_bytes,
        compressed_bytes,
        decompressed_size: 0, // filled by the contract validator
    })
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<Entry>) -> Result<()> {
    let mut read: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("read dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .collect();
    read.sort_by_key(|e| e.file_name());
    for entry in read {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .with_context(|| format!("{} not under {}", path.display(), root.display()))?;
        let archive_path = format!("{PREFIX}/{}", rel.to_string_lossy().replace('\\', "/"));
        if ft.is_dir() {
            out.push(Entry {
                archive_path,
                kind: Kind::Dir,
            });
            collect(root, &path, out)?;
        } else if ft.is_file() {
            out.push(Entry {
                archive_path,
                kind: Kind::File(path),
            });
        }
    }
    Ok(())
}

fn new_header(entry_type: EntryType, mode: u32) -> Header {
    let mut h = Header::new_gnu();
    h.set_entry_type(entry_type);
    h.set_mode(mode);
    h.set_mtime(0);
    h.set_uid(0);
    h.set_gid(0);
    h
}

fn append_dir(builder: &mut Builder<impl Write>, path: &str) -> Result<()> {
    let mut h = new_header(EntryType::Directory, 0o755);
    h.set_size(0);
    // append_data handles >100-byte paths via a GNU 'L' record, which the
    // consumer contract accepts (pax does not).
    builder
        .append_data(&mut h, path, io::empty())
        .with_context(|| format!("append dir {path}"))?;
    Ok(())
}

fn append_file(builder: &mut Builder<impl Write>, disk: &Path, path: &str) -> Result<u64> {
    let meta = fs::metadata(disk).with_context(|| format!("stat {}", disk.display()))?;
    let len = meta.len();
    let mut h = new_header(EntryType::Regular, 0o644);
    h.set_size(len);
    let f = File::open(disk).with_context(|| format!("open {}", disk.display()))?;
    builder
        .append_data(&mut h, path, f)
        .with_context(|| format!("append file {path}"))?;
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract;

    #[test]
    fn round_trips_through_consumer_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let rep = tmp.path().join("replacements");
        fs::create_dir_all(rep.join("sub dir")).unwrap();
        fs::write(rep.join("a.ktx"), b"AAAA").unwrap();
        fs::write(rep.join("sub dir").join("b.ktx"), b"BBBBBB").unwrap();

        let out = tmp.path().join("pack.tar.zst");
        let stats = write_archive(&out, &rep).unwrap();
        assert_eq!(stats.ktx_files, 2);

        let summary = contract::validate_tar_zst(&out, Some(2)).unwrap();
        assert_eq!(summary.ktx_files, 2);
        assert!(summary.decompressed_size > 0);
    }

    #[test]
    fn long_paths_use_gnu_longname_not_pax() {
        let tmp = tempfile::tempdir().unwrap();
        let rep = tmp.path().join("replacements");
        fs::create_dir_all(&rep).unwrap();
        let long = "a".repeat(150) + ".ktx";
        fs::write(rep.join(&long), b"AAAA").unwrap();

        let out = tmp.path().join("pack.tar.zst");
        write_archive(&out, &rep).unwrap();
        let summary = contract::validate_tar_zst(&out, Some(1)).unwrap();
        assert_eq!(summary.ktx_files, 1);
    }
}
