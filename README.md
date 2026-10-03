# astctool

Turns a folder of textures into an ARMSX2 ASTC texture pack.

Pick a folder (the `replacements` dir, or a pack with one inside), hit build,
get a `.tar.zst`. No options you actually need to touch: block size and quality
are already set to 6x6 thorough.

## Install

Grab the installer for your OS from the Releases page.

- Windows: run the installer. If SmartScreen complains, More info -> Run anyway.
- macOS: open the dmg, drag to Applications. First launch: right-click the app,
  Open, then Open again. It's unsigned, so macOS is just being annoying.

## Use

The window wants one thing: a directory of images, or a `.zip` of one that
already works with PCSX2. Input formats are the ones kram reads:
`.png .dds .ktx .ktx2`. Anything else is skipped and counted in the report.

Output goes next to the input as `<name>.tar.zst`, plus `report.json` and
`astctool.log`. If a few textures fail, the build keeps going and tells you
which ones at the end.

## CLI

```
astctool --cli -i <folder-or-zip> -o <out.tar.zst> \
    [--format astc6x6] [--quality 98] [--jobs N]
```

## Build

Needs Rust, cmake, ninja, clang.

```
git submodule update --init --recursive
scripts/build-kram.sh          # builds kram for this platform into build/kram
cargo build --release
```

kram is MIT (vendor/kram). Rest of it is MIT (LICENSE, NOTICE).
