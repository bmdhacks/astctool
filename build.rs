use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=ASTCTOOL_KRAM_BIN");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let exe = if target_os == "windows" { "kram.exe" } else { "kram" };

    let candidates = [
        env::var_os("ASTCTOOL_KRAM_BIN").map(PathBuf::from),
        Some(manifest.join("build").join("kram").join(exe)),
        Some(manifest.join("vendor").join("kram").join("bin").join(exe)),
    ];

    let found = candidates.into_iter().flatten().find(|p| p.is_file());
    let path = match found {
        Some(p) => p,
        None => panic!(
            "cannot find the kram binary to embed.\n\
             run scripts/build-kram.sh first, or set ASTCTOOL_KRAM_BIN.\n\
             looked in {}/build/kram/{}",
            manifest.display(),
            exe
        ),
    };

    println!("cargo:rerun-if-changed={}", path.display());
    let abs = path.canonicalize().unwrap_or(path);
    println!("cargo:rustc-env=ASTCTOOL_KRAM_BIN={}", abs.display());
}
