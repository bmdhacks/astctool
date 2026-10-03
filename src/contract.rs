// Consumer-contract validator. This is a Rust port of the ARMSX2 migration
// tool's tarcheck.py: the same stream checks the Android/PCSX2 extractor
// applies, so we can prove a pack before declaring success.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

use unicode_normalization::UnicodeNormalization;

pub const RECORD: usize = 512;
const MAX_ENTRIES: usize = 100_000;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_COMPONENTS: usize = 32;
const MAX_COMPONENT_BYTES: usize = 255;
const MAX_LONGNAME_BYTES: u64 = 4097;
const SKIP_CHUNK: usize = 1 << 20;

const DIR_TYPE: u8 = b'5';
const LONGNAME_TYPE: u8 = b'L';

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub entries: usize,
    pub ktx_files: usize,
    pub decompressed_size: u64,
}

fn octal(field: &[u8], what: &str) -> Result<u64, String> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return Err(format!("base-256 {what} not accepted"));
    }
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    let text = &field[..end];
    let text = trim_ascii(text);
    if text.is_empty() {
        return Ok(0);
    }
    let s = std::str::from_utf8(text).map_err(|_| format!("malformed octal {what}"))?;
    u64::from_str_radix(s, 8).map_err(|_| format!("malformed octal {what}"))
}

fn trim_ascii(mut b: &[u8]) -> &[u8] {
    while b.first() == Some(&b' ') {
        b = &b[1..];
    }
    while b.last() == Some(&b' ') {
        b = &b[..b.len() - 1];
    }
    b
}

fn signed_byte_sum(b: &[u8]) -> i64 {
    b.iter().map(|&x| x as i8 as i64).sum()
}

fn checksum_ok(header: &[u8]) -> bool {
    let Some(stored) = octal(&header[148..156], "checksum").ok() else {
        return false;
    };
    let unsigned: u64 =
        header[..148].iter().map(|&b| b as u64).sum::<u64>() + 8 * 0x20 + header[156..].iter().map(|&b| b as u64).sum::<u64>();
    if unsigned == stored {
        return true;
    }
    let signed = signed_byte_sum(&header[..148]) + 8 * 0x20 + signed_byte_sum(&header[156..]);
    signed as u64 == stored
}

fn split_components(path: &str) -> Result<Vec<&str>, String> {
    let trimmed = path.trim_end_matches('/');
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(format!("empty path component in {path:?}"));
    }
    if parts.len() > MAX_COMPONENTS {
        return Err(format!("path depth exceeds {MAX_COMPONENTS}: {path:?}"));
    }
    for part in &parts {
        if *part == ".." {
            return Err(format!("parent traversal rejected: {path:?}"));
        }
        if *part == "." {
            return Err(format!("dot component in {path:?}"));
        }
        if part.len() > MAX_COMPONENT_BYTES {
            return Err(format!(
                "component exceeds {MAX_COMPONENT_BYTES} bytes: {path:?}"
            ));
        }
    }
    Ok(parts)
}

fn collision_key(path: &str) -> String {
    path.nfc().collect::<String>().to_lowercase()
}

struct Reader<R: Read> {
    inner: R,
    total: u64,
}

impl<R: Read> Reader<R> {
    fn new(inner: R) -> Self {
        Self { inner, total: 0 }
    }

    fn read_record(&mut self) -> Result<[u8; RECORD], String> {
        let mut buf = [0u8; RECORD];
        self.inner
            .read_exact(&mut buf)
            .map_err(|_| "truncated tar stream".to_string())?;
        self.total += RECORD as u64;
        Ok(buf)
    }

    /// Read exactly `n` bytes and throw them away.
    fn read_discard(&mut self, mut n: u64, what: &str) -> Result<(), String> {
        let mut buf = vec![0u8; SKIP_CHUNK];
        while n > 0 {
            let want = n.min(SKIP_CHUNK as u64) as usize;
            let got = self
                .inner
                .read(&mut buf[..want])
                .map_err(|_| format!("truncated {what}"))?;
            if got == 0 {
                return Err(format!("truncated {what}"));
            }
            self.total += got as u64;
            n -= got as u64;
        }
        Ok(())
    }

    /// Read `size` payload bytes, advancing past 512-byte padding.
    fn skip_payload(&mut self, size: u64, what: &str) -> Result<(), String> {
        let padded = size.div_ceil(RECORD as u64) * RECORD as u64;
        self.read_discard(padded, what)
    }

    fn read_exact_payload(&mut self, size: u64) -> Result<Vec<u8>, String> {
        let mut out = vec![0u8; size as usize];
        self.inner
            .read_exact(&mut out)
            .map_err(|_| "truncated GNU L payload".to_string())?;
        self.total += size;
        let pad = (size.div_ceil(RECORD as u64) * RECORD as u64) - size;
        if pad > 0 {
            self.read_discard(pad, "GNU L padding")?;
        }
        Ok(out)
    }
}

pub fn validate_tar_stream<R: Read>(stream: R, expect_ktx: Option<usize>) -> Result<Summary, String> {
    let mut r = Reader::new(stream);
    let mut entries = 0usize;
    let mut ktx_count = 0usize;
    let mut pending_longname: Option<Vec<u8>> = None;
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut seen_dirs: HashSet<String> = HashSet::new();

    loop {
        let header = r.read_record()?;
        if header.iter().all(|&b| b == 0) {
            break;
        }

        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(format!("entry count exceeds {MAX_ENTRIES}"));
        }
        if !checksum_ok(&header) {
            let stored = octal(&header[148..156], "checksum").unwrap_or(0);
            let unsigned: u64 = header[..148].iter().map(|&b| b as u64).sum::<u64>()
                + 8 * 0x20
                + header[156..].iter().map(|&b| b as u64).sum::<u64>();
            let signed = signed_byte_sum(&header[..148]) + 8 * 0x20 + signed_byte_sum(&header[156..]);
            let name = String::from_utf8_lossy(&header[..100]);
            let name = name.split('\0').next().unwrap_or("");
            return Err(format!(
                "header checksum mismatch: stored={stored} unsigned={unsigned} signed={signed} name={name:?} type={:?}",
                header[156] as char
            ));
        }
        let magic = &header[257..263];
        if magic != b"ustar " && magic != b"ustar\0" {
            return Err(format!("unknown tar magic {:?}", String::from_utf8_lossy(magic)));
        }
        let typeflag = header[156];
        let size = octal(&header[124..136], "size")?;

        if typeflag == LONGNAME_TYPE {
            if pending_longname.is_some() {
                return Err("GNU L record without a following entry".into());
            }
            if size == 0 || size > MAX_LONGNAME_BYTES {
                return Err(format!("GNU L payload must be 1..{MAX_LONGNAME_BYTES} bytes"));
            }
            let payload = r.read_exact_payload(size)?;
            if payload[(size - 1) as usize] != 0 {
                return Err("GNU L payload is not NUL-terminated".into());
            }
            pending_longname = Some(payload[..(size - 1) as usize].to_vec());
            continue;
        }

        let is_dir = typeflag == DIR_TYPE;
        if !is_dir && typeflag != 0 && typeflag != b'0' {
            return Err(format!(
                "unsupported entry type {:?} (links/devices/pax are outside the producer contract)",
                typeflag as char
            ));
        }

        let name: Vec<u8> = match pending_longname.take() {
            Some(n) => n,
            None => {
                let end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
                header[..end].to_vec()
            }
        };
        if name.len() > MAX_PATH_BYTES {
            return Err(format!("path exceeds {MAX_PATH_BYTES} bytes"));
        }
        let mut path = String::from_utf8(name).map_err(|_| "entry name is not valid UTF-8")?;
        if let Some(rest) = path.strip_prefix("./") {
            path = rest.to_string();
        }
        if path.starts_with('/') || path.starts_with("../") || path == ".." {
            return Err(format!("absolute or escaping path rejected: {path:?}"));
        }

        if path.is_empty() {
            if !is_dir || size != 0 {
                return Err("empty entry name on a non-root entry".into());
            }
            continue; // root "./"
        }

        let parts = split_components(&path)?;
        let key = collision_key(&path);
        if seen_files.contains(&key) || seen_dirs.contains(&key) {
            return Err(format!("duplicate or colliding destination: {path:?}"));
        }
        for i in 1..parts.len() {
            let ancestor = collision_key(&parts[..i].join("/"));
            if seen_files.contains(&ancestor) {
                return Err(format!(
                    "file/directory ancestor conflict at {}",
                    parts[..i].join("/")
                ));
            }
        }

        if is_dir {
            seen_dirs.insert(key);
        } else {
            seen_files.insert(key);
            if size > MAX_FILE_BYTES {
                return Err(format!("payload exceeds {MAX_FILE_BYTES}"));
            }
            if path.to_lowercase().ends_with(".ktx") {
                ktx_count += 1;
            }
        }

        r.skip_payload(size, "entry payload")?;
    }

    // Second end block, then only zero padding.
    let second = r.read_record()?;
    if !second.iter().all(|&b| b == 0) {
        return Err("missing second end block".into());
    }
    let mut trailing = vec![0u8; SKIP_CHUNK];
    loop {
        let n = r
            .inner
            .read(&mut trailing)
            .map_err(|_| "read error in trailing padding".to_string())?;
        if n == 0 {
            break;
        }
        r.total += n as u64;
        if trailing[..n].iter().any(|&b| b != 0) {
            return Err("nonzero trailing tar padding".into());
        }
    }

    if let Some(expect) = expect_ktx {
        if ktx_count != expect {
            return Err(format!(
                "ktx count mismatch: stream has {ktx_count}, expected {expect}"
            ));
        }
    }

    Ok(Summary {
        entries,
        ktx_files: ktx_count,
        decompressed_size: r.total,
    })
}

pub fn validate_tar_zst(path: &Path, expect_ktx: Option<usize>) -> Result<Summary, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|e| format!("zstd decoder: {e}"))?;
    validate_tar_stream(decoder, expect_ktx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octal_parses_and_rejects_base256() {
        assert_eq!(octal(b"0000017\0", "size").unwrap(), 15);
        assert_eq!(octal(b"       \0", "size").unwrap(), 0);
        assert!(octal(&[0x80, 1, 2], "size").is_err());
    }

    #[test]
    fn empty_stream_is_two_zero_blocks() {
        let stream = vec![0u8; RECORD * 2];
        let s = validate_tar_stream(&stream[..], Some(0)).unwrap();
        assert_eq!(s.entries, 0);
        assert_eq!(s.ktx_files, 0);
        assert_eq!(s.decompressed_size, RECORD as u64 * 2);
    }

    #[test]
    fn truncated_stream_errors() {
        let stream = vec![0u8; RECORD];
        assert!(validate_tar_stream(&stream[..], None).is_err());
    }
}
