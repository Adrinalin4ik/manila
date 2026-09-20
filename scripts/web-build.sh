#!/usr/bin/env bash
#
# web-build.sh - build the browser client into web/dist/ (index.html + wasm + glue + .br/.gz).
#
#   scripts/web-build.sh            # WebGPU backend (the world needs it: storage buffers)
#   WEB_BACKEND=webgl2 scripts/web-build.sh   # WebGL2: every browser, glue screens only
#   WEB_PROFILE=ship scripts/web-build.sh    # compare fat LTO against the release default
#   WEB_DEBUG=1 scripts/web-build.sh          # keep the wasm name section (symbolic stack traces)
#
# Then serve web/dist with wenilla-host:
#   cargo run --release -p wenilla-host -- --www web/dist --data /path/to/WoW/Data
#
# Prerequisites: scripts/web-setup.sh (rustup target, matching wasm-bindgen-cli, wasi-sdk).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

DIST=web/dist
BACKEND="${WEB_BACKEND:-webgpu}"
PROFILE="${WEB_PROFILE:-release}"
case "${PROFILE}" in
  release|ship) ;;
  *) echo "WEB_PROFILE must be release or ship" >&2; exit 1 ;;
esac
WASM="target/wasm32-unknown-unknown/${PROFILE}/wenilla.wasm"
export WASI_SDK="${WASI_SDK:-$(pwd)/tools/wasi-sdk}"

command -v wasm-bindgen >/dev/null || { echo "wasm-bindgen not found — run scripts/web-setup.sh" >&2; exit 1; }
[ -d "${WASI_SDK}" ] || { echo "wasi-sdk not at ${WASI_SDK} — run scripts/web-setup.sh (or set WASI_SDK)" >&2; exit 1; }

cargo build --profile "${PROFILE}" --target wasm32-unknown-unknown -p wenilla --no-default-features --features "${BACKEND}"

mkdir -p "${DIST}"
# The name section is half the file (~170 MB -> ~90 MB) and only feeds stack-trace symbols.
strip=(--remove-name-section --remove-producers-section)
[ "${WEB_DEBUG:-0}" = 1 ] && strip=()
wasm-bindgen --target web --no-typescript "${strip[@]}" --out-dir "${DIST}" "${WASM}"
cp web/index.html web/wasi_stubs.js web/boot.js web/platform.js web/bridge.js "${DIST}/"
# The bridge examples (web/README.md § "JavaScript bridge"): a HUD, an idle loop.
mkdir -p "${DIST}/examples" && cp web/examples/*.js "${DIST}/examples/"
# The boot prefetch manifest (see web/boot.js) — optional so a tree that hasn't captured one
# yet still builds; the overlay just skips the data-prefetch line.
for m in web/boot-manifest.json web/world-manifest.json; do if [ -f "$m" ]; then cp "$m" "${DIST}/"; fi; done

# binaryen's wasm-opt -O3 (scripts/web-setup.sh puts it in tools/binaryen): ~35 s, and the
# client is CPU-bound on its one wasm thread, so this is a speed pass, not a size pass —
# measured +6 % frame rate in-world (docs: wenilla perf notes, 2026-08-29). The feature flags
# match what rustc emitted; without them wasm-opt refuses the module. Skipped when absent.
WASM_OPT="${WASM_OPT:-$(pwd)/tools/binaryen/bin/wasm-opt}"
command -v "${WASM_OPT}" >/dev/null || WASM_OPT="$(command -v wasm-opt || true)"
if [ -n "${WASM_OPT}" ] && [ "${WEB_DEBUG:-0}" != 1 ]; then
  # `|| opt_status=$?` rather than `|| true`: the latter swallows the code it was meant to
  # report, and a wasm-opt that fails silently is exactly what this block exists for.
  opt_status=0
  "${WASM_OPT}" -O3 --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    --enable-mutable-globals --enable-reference-types --enable-multivalue \
    "${DIST}/wenilla_bg.wasm" -o "${DIST}/wenilla_bg.wasm.opt" || opt_status=$?
  # wasm-opt has returned 0 without writing its output here (twice, on a ~90 MB module in a
  # memory-constrained shell). Say so rather than letting `mv` fail with a stat error that reads
  # like a path typo.
  if [ ! -s "${DIST}/wenilla_bg.wasm.opt" ]; then
    echo "web-build: wasm-opt exited $opt_status and produced no output - shipping the unoptimised module" >&2
  else
    mv "${DIST}/wenilla_bg.wasm.opt" "${DIST}/wenilla_bg.wasm"
  fi
elif [ "${WEB_DEBUG:-0}" = 1 ]; then
  echo "WEB_DEBUG=1 — skipping wasm-opt to preserve debugging symbols"
else
  echo "wasm-opt not found (scripts/web-setup.sh fetches binaryen) — shipping the unoptimised module"
fi

# Precompressed siblings for wenilla-host's precompressed_br()/gzip(): ~90 MB of wasm goes
# over the wire as ~15 MB without per-request CPU. brotli -q 5 is the speed/size knee.
# **`if`, not `&&`.** Under `set -e` a bare `cmd-a && cmd-b` whose FIRST half fails is a failed
# compound command, and the script dies on the spot. brotli is optional and was absent here, so
# this loop exited the build before the gzip line ever ran - every build silently left the
# PREVIOUS run's `.gz` in place, and `wenilla-host` serves `.gz` to any browser that asks for it.
# A client was shipped that way twice: fresh `.wasm`, stale compressed twin, and the browser took
# the stale one.
for f in "${DIST}"/*.wasm "${DIST}"/*.js; do
  if command -v brotli >/dev/null; then brotli -f -q 5 "$f" -o "$f.br"; fi
  if command -v gzip >/dev/null; then gzip -kf -6 "$f"; fi
done
# The compressed twins must not outlive their source. A `.gz` older than its `.wasm` is the exact
# shape of the failure above, and it is invisible from the outside - the page loads, the code is
# last build's.
for f in "${DIST}"/*.wasm "${DIST}"/*.js; do
  for z in "$f.gz" "$f.br"; do
    if [ -e "$z" ] && [ "$z" -ot "$f" ]; then
      echo "web-build: $z is older than $f - compression did not run" >&2
      exit 1
    fi
  done
done
ls -la "${DIST}"
