// End-to-end tests: run the real pipeline (with the embedded kram) over the
// committed fixtures. These run on every platform in CI.

use std::path::PathBuf;

use astctool::pipeline::{self, PipelineOptions, Progress};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn opts(input: PathBuf, output: PathBuf) -> PipelineOptions {
    PipelineOptions {
        input,
        output: Some(output),
        format: "astc6x6".into(),
        quality: 98,
        jobs: 2,
    }
}

#[test]
fn packs_a_clean_pack() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("pack.tar.zst");
    let mut progress = |_p: Progress| {};

    let rep = pipeline::run(&opts(fixture("pack"), out.clone()), &mut progress).unwrap();

    // rock-a (base + mip sidecar), grass (in a subdir) => 2 textures.
    assert_eq!(rep.succeeded, 2, "failures: {:?}", rep.failures);
    assert_eq!(rep.failed_count, 0);
    assert_eq!(rep.unsupported.len(), 1); // notes.txt
    assert_eq!(rep.archive.ktx_files, 2);
    assert!(out.is_file());
    assert!(std::fs::metadata(&out).unwrap().len() > 0);
}

#[test]
fn marches_on_past_a_bad_texture() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("pack.tar.zst");
    let mut progress = |_p: Progress| {};

    let rep = pipeline::run(&opts(fixture("broken"), out.clone()), &mut progress).unwrap();

    assert!(rep.succeeded >= 1, "expected at least one good texture");
    assert!(rep.failed_count >= 1, "expected the corrupt png to fail");
    assert!(out.is_file(), "archive should still be produced");
}

/// Decode a KTX1 file with kram and return the alpha bytes of every mip level.
fn decoded_alpha_levels(kram: &std::path::Path, ktx: &std::path::Path) -> Vec<Vec<u8>> {
    let dec = ktx.with_extension("rgba.ktx");
    let st = std::process::Command::new(kram)
        .arg("decode")
        .arg("-i")
        .arg(ktx)
        .arg("-o")
        .arg(&dec)
        .output()
        .unwrap();
    assert!(st.status.success(), "kram decode failed: {:?}", st);
    let b = std::fs::read(&dec).unwrap();
    let word = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as usize;
    // KTX1: 12-byte identifier, 13 u32 fields; glInternalFormat 0x8058 is RGBA8.
    assert_eq!(word(12 + 4 * 4), 0x8058, "kram decode did not write RGBA8");
    let mips = word(12 + 11 * 4).max(1);
    let mut off = 64 + word(12 + 12 * 4);
    let mut levels = Vec::new();
    for _ in 0..mips {
        let size = word(off);
        off += 4;
        levels.push(b[off..off + size].iter().skip(3).step_by(4).copied().collect());
        off += (size + 3) & !3;
    }
    levels
}

#[test]
fn flat_ps2_opaque_alpha_decodes_exactly() {
    // 96x96 noisy RGB with every alpha 0x80 (PS2 opaque). Without the
    // alpha-exact pass about 85% of these texels decode as 129..139.
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("pack.tar.zst");
    let mut progress = |_p: Progress| {};
    let rep = pipeline::run(&opts(fixture("alpha"), out.clone()), &mut progress).unwrap();
    assert_eq!(rep.succeeded, 1, "failures: {:?}", rep.failures);

    let unpacked = tmp.path().join("unpacked");
    let f = std::fs::File::open(&out).unwrap();
    tar::Archive::new(zstd::Decoder::new(f).unwrap()).unpack(&unpacked).unwrap();
    let ktx = walk(&unpacked)
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "ktx"))
        .expect("no ktx in archive");

    let kram = astctool::kram::ensure_kram().unwrap();
    let levels = decoded_alpha_levels(&kram, &ktx);
    assert!(levels.len() > 1, "expected a mip chain");
    for (i, level) in levels.iter().enumerate() {
        let wrong = level.iter().filter(|&&a| a != 0x80).count();
        assert_eq!(wrong, 0, "mip {i}: {wrong} of {} texels decode to alpha other than 0x80", level.len());
    }
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
