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
# `--out-name manila`: the artifact is this fork's, and the page loads it by name. The CRATE
# stays `wenilla` - renaming it would move `wenilla-host`/`wenilla-realm` and the pin bot's
# `WENILLA_COMMIT`, which is prod's, for a filename.
wasm-bindgen --target web --no-typescript "${strip[@]}" --out-name manila --out-dir "${DIST}" "${WASM}"
# **The character-skin compositor's own module** (`crates/manila-skin`), instantiated inside a Web
# Worker with its own linear memory. A separate instance rather than a thread because bevy
# hard-disables its multi-threaded executor on wasm32 and cpal's worklet host needs atomics: this
# needs neither, and it is what takes a 248 ms median composite off the drawing thread.
#
# Built after the client so a failure here cannot leave a half-written main bundle behind. It is
# optional at RUNTIME - the page falls back to compositing on the main thread when the worker does
# not start - but not optional here: a silent miss would look exactly like the fallback working.
cargo build --profile "${PROFILE}" --target wasm32-unknown-unknown -p manila-skin
SKIN_WASM="target/wasm32-unknown-unknown/${PROFILE}/manila_skin.wasm"
[ -f "${SKIN_WASM}" ] || { echo "web-build: ${SKIN_WASM} was not produced" >&2; exit 1; }
wasm-bindgen --target web --no-typescript "${strip[@]}" --out-name manila_skin --out-dir "${DIST}" "${SKIN_WASM}"
cp web/index.html web/wasi_stubs.js web/boot.js web/platform.js web/bridge.js \
   web/skin_worker.js web/skin_worker_entry.js web/frame_trace.js "${DIST}/"
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
    "${DIST}/manila_bg.wasm" -o "${DIST}/manila_bg.wasm.opt" || opt_status=$?
  # wasm-opt has returned 0 without writing its output here (twice, on a ~90 MB module in a
  # memory-constrained shell). Say so rather than letting `mv` fail with a stat error that reads
  # like a path typo.
  if [ ! -s "${DIST}/manila_bg.wasm.opt" ]; then
    echo "web-build: wasm-opt exited $opt_status and produced no output - shipping the unoptimised module" >&2
  else
    # Say so on SUCCESS too, with the sizes. The success path was silent, so a log could not tell
    # "wasm-opt ran" from "wasm-opt was skipped" - only the failures spoke, and a pass worth +6 %
    # of the frame rate should not be something you infer from the absence of a warning.
    before=$(wc -c < "${DIST}/manila_bg.wasm")
    after=$(wc -c < "${DIST}/manila_bg.wasm.opt")
    mv "${DIST}/manila_bg.wasm.opt" "${DIST}/manila_bg.wasm"
    echo "web-build: wasm-opt -O3 ok - $((before / 1024)) KiB -> $((after / 1024)) KiB"
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
# The compressed twins must decompress to their source. A twin left over from the previous build
# is invisible from the outside - the page loads, every check on the `.wasm` reports the new hash,
# and the browser runs last build's code because `wenilla-host` serves the twin.
#
# **Compare CONTENT LENGTH, never mtime.** The first version of this guard tested `"$z" -ot "$f"`
# and failed every build: gzip copies its source's mtime onto its output and truncates the
# sub-second part, so a `.gz` written seconds later reads as ~0.2 s OLDER than the `.wasm` it was
# made from. A guard that cries stale on every healthy build is worse than no guard, because the
# line gets skipped by eye. (fdca1966's message says this was already fixed. It was not - that
# commit only touched the wasm-opt block, and the timestamp test survived it.)
for f in "${DIST}"/*.wasm "${DIST}"/*.js; do
  want=$(stat -c%s "$f")
  if [ -e "$f.gz" ]; then
    # `gzip -l` reads the uncompressed length out of the trailer - no decompression pass.
    got=$(gzip -l "$f.gz" | awk 'NR==2 {print $2}')
    if [ "$got" != "$want" ]; then
      echo "web-build: $f.gz decompresses to $got bytes, $f is $want - stale twin" >&2
      exit 1
    fi
  fi
  if [ -e "$f.br" ]; then
    # brotli has no trailer to read, so this one costs a decompression pass.
    got=$(brotli -dc "$f.br" | wc -c)
    if [ "$got" != "$want" ]; then
      echo "web-build: $f.br decompresses to $got bytes, $f is $want - stale twin" >&2
      exit 1
    fi
  fi
done
ls -la "${DIST}"
