#!/usr/bin/env bash
# Build kram for the current platform into build/kram/.
# kram lives in vendor/kram (our fork, MIT).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
kram_src="$root/vendor/kram"
out="$root/build/kram"
mkdir -p "$out"

os="$(uname -s)"

case "$os" in
  Linux)
    cmake -S "$kram_src" -B "$root/build/kram-linux" -G Ninja \
      -DCMAKE_BUILD_TYPE=Release -DATE=OFF
    cmake --build "$root/build/kram-linux" --config Release --target kram
    cp "$root/build/kram-linux/kramc/kram" "$out/kram"
    ;;

  Darwin)
    # Universal: build both slices, then lipo.
    for a in arm64 x86_64; do
      cmake -S "$kram_src" -B "$root/build/kram-mac-$a" \
        -DCMAKE_BUILD_TYPE=Release -DATE=OFF -DKRAM_BUILD_EXTRAS=OFF \
        -DCMAKE_OSX_ARCHITECTURES=$a
      cmake --build "$root/build/kram-mac-$a" --config Release --target kram
    done
    lipo -create \
      "$root/build/kram-mac-arm64/kramc/kram" \
      "$root/build/kram-mac-x86_64/kramc/kram" \
      -output "$out/kram"
    ;;

  MINGW*|MSYS*|CYGWIN*)
    # Ninja + clang-cl. The Visual Studio generator name keeps changing across
    # runner images; this combo just needs the MSVC env (set up by the caller).
    cmake -S "$kram_src" -B "$root/build/kram-win" -G Ninja \
      -DCMAKE_C_COMPILER=clang-cl -DCMAKE_CXX_COMPILER=clang-cl \
      -DCMAKE_BUILD_TYPE=Release -DATE=OFF
    cmake --build "$root/build/kram-win" --config Release --target kram
    cp "$root/build/kram-win/kramc/kram.exe" "$out/kram.exe"
    ;;

  *)
    echo "unsupported os: $os" >&2
    exit 1
    ;;
esac

echo "kram built: $out"
