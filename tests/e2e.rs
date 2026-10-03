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
