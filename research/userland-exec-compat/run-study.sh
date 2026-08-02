#!/usr/bin/env bash
set -euo pipefail

CLONES=${1:?usage: run-study.sh CLONE_ROOT [OUTPUT_ROOT] [TRAP_PRELOAD_SOURCE]}
OUTPUT_ROOT=${2:-${TMPDIR:-/tmp}/fspy-userland-exec-compat-results}
SOURCE_ROOT=$(cd "$(dirname "$0")" && pwd)
TRAP_PRELOAD_SOURCE=${3:-"$SOURCE_ROOT/../sigsys-prototype/trap_preload.c"}
WORK_ROOT=${FSPY_USERLAND_WORK_ROOT:-${TMPDIR:-/tmp}/fspy-userland-exec-compat-work}
BUILD_ROOT="$WORK_ROOT/build"
SOURCE_COPY="$WORK_ROOT/source"
RESULTS="$OUTPUT_ROOT/raw"
ESBUILD_ROOT="$WORK_ROOT/esbuild"

mkdir -p "$BUILD_ROOT" "$SOURCE_COPY" "$RESULTS" \
  "$ESBUILD_ROOT/platform" "$ESBUILD_ROOT/js"
rm -f "$OUTPUT_ROOT/summary.tsv" "$OUTPUT_ROOT/environment.txt"
cp -a "$SOURCE_ROOT/." "$SOURCE_COPY"

{
  uname -a
  sed -n '1,12p' /etc/os-release
  gcc --version | head -1
  python3 --version
  node --version
  go version
} >"$OUTPUT_ROOT/environment.txt" 2>&1

cp -a "$CLONES/mettle/libreflect/." "$BUILD_ROOT/libreflect"
# Build the AArch64 assembly path directly.  libreflect's generated configure
# tests an absolute-looking header name as a system include and incorrectly
# selects its memfd_create/execveat fallback here.  The fallback would perform
# a real kernel exec and invalidate this userland-exec experiment.
sed 's/@HAVE_ASM@/1/' "$BUILD_ROOT/libreflect/include/reflect.h.in" \
  >"$BUILD_ROOT/libreflect/include/reflect.h"

gcc -O2 -g -Wall -Wextra -pthread \
  -I"$BUILD_ROOT/libreflect/include" \
  -I"$BUILD_ROOT/libreflect/src" \
  -I"$BUILD_ROOT/libreflect/arch/linux/aarch64" \
  "$SOURCE_COPY/libreflect_runner.c" \
  "$BUILD_ROOT/libreflect/src/map_elf.c" \
  "$BUILD_ROOT/libreflect/src/stack_setup.c" \
  "$BUILD_ROOT/libreflect/src/jump.c" \
  "$BUILD_ROOT/libreflect/src/exec.c" \
  -o "$BUILD_ROOT/libreflect-runner" \
  >"$OUTPUT_ROOT/build-libreflect.log" 2>&1
gcc -O2 -g -Wall -Wextra -pthread "$SOURCE_COPY/compat_probe.c" \
  -o "$BUILD_ROOT/compat-probe"
gcc -O2 -g -Wall -Wextra -pthread -no-pie "$SOURCE_COPY/compat_probe.c" \
  -o "$BUILD_ROOT/glibc-nopie-probe"
musl-gcc -O2 -g -Wall -Wextra -pthread -static \
  "$SOURCE_COPY/compat_probe.c" -o "$BUILD_ROOT/musl-static-probe"
CGO_ENABLED=0 go build -trimpath -o "$BUILD_ROOT/static-go-probe" \
  "$SOURCE_COPY/static_probe.go"
gcc -O2 -g -Wall -Wextra -fPIC -shared "$CLONES/sigsys-test/sigsys_preload.c" \
  -o "$BUILD_ROOT/libsigsys-preload.so"
gcc -O2 -g -Wall -Wextra "$CLONES/sigsys-test/sigsys_target.c" \
  -o "$BUILD_ROOT/sigsys-target"
gcc -O2 -Wall -Wextra -Werror -shared -fPIC "$TRAP_PRELOAD_SOURCE" \
  -o "$BUILD_ROOT/libtrap-preload.so"
chmod +x "$SOURCE_COPY/shebang_probe.sh"

# Pin a real, statically linked frontend tool and its JavaScript API.  The API
# case exercises Node -> transformed host -> userland-loaded esbuild service.
curl -fsSL \
  https://registry.npmjs.org/@esbuild/linux-arm64/-/linux-arm64-0.28.1.tgz \
  -o "$ESBUILD_ROOT/platform.tgz"
tar -xzf "$ESBUILD_ROOT/platform.tgz" -C "$ESBUILD_ROOT/platform" \
  --strip-components=1
curl -fsSL https://registry.npmjs.org/esbuild/-/esbuild-0.28.1.tgz \
  -o "$ESBUILD_ROOT/js.tgz"
tar -xzf "$ESBUILD_ROOT/js.tgz" -C "$ESBUILD_ROOT/js" \
  --strip-components=1
ESBUILD_BINARY="$ESBUILD_ROOT/platform/bin/esbuild"
ESBUILD_MODULE="$ESBUILD_ROOT/js"

ANVIL=(python3 "$CLONES/ulexecve/ulexecve.py")
LIBREFLECT=("$BUILD_ROOT/libreflect-runner")

printf 'case\texit\tduration_ms\n' >"$OUTPUT_ROOT/summary.tsv"

run_case() {
  local name=$1
  shift
  local started ended status
  started=$(date +%s%3N)
  set +e
  timeout --signal=KILL 25s "$@" >"$RESULTS/$name.stdout" 2>"$RESULTS/$name.stderr"
  status=$?
  set -e
  ended=$(date +%s%3N)
  printf '%s\t%s\t%s\n' "$name" "$status" "$((ended - started))" >>"$OUTPUT_ROOT/summary.tsv"
}

# Native controls.
run_case native-c-probe "$BUILD_ROOT/compat-probe" alpha beta
run_case native-glibc-nopie "$BUILD_ROOT/glibc-nopie-probe" alpha
run_case native-musl-static "$BUILD_ROOT/musl-static-probe" alpha
run_case native-node node "$SOURCE_COPY/node_probe.js" alpha
run_case native-static-go "$BUILD_ROOT/static-go-probe" alpha
run_case native-shebang "$SOURCE_COPY/shebang_probe.sh" alpha
run_case native-coreutils /bin/echo coreutils-ok
run_case native-esbuild-cli "$ESBUILD_BINARY" "$SOURCE_COPY/esbuild_entry.ts" \
  --bundle --sourcemap --outfile="$BUILD_ROOT/native-esbuild.js"
run_case native-esbuild-api node "$SOURCE_COPY/esbuild_api_probe.js" \
  "$ESBUILD_MODULE" "$ESBUILD_BINARY" "$ESBUILD_BINARY" \
  "$SOURCE_COPY/esbuild_entry.ts"

# libreflect: best embeddable dynamic-PIE reference.
run_case libreflect-c-probe "${LIBREFLECT[@]}" "$BUILD_ROOT/compat-probe" alpha beta
run_case libreflect-glibc-nopie "${LIBREFLECT[@]}" \
  "$BUILD_ROOT/glibc-nopie-probe" alpha
run_case libreflect-musl-static "${LIBREFLECT[@]}" \
  "$BUILD_ROOT/musl-static-probe" alpha
run_case libreflect-node "${LIBREFLECT[@]}" /usr/bin/node "$SOURCE_COPY/node_probe.js" alpha
# $1 and $2 belong to the nested bash command.
# shellcheck disable=SC2016
run_case libreflect-host-shaped-node env LIBREFLECT_TARGET=/usr/bin/node \
  bash -c 'exec -a /usr/bin/node "$1" "$2" alpha' bash \
  "$BUILD_ROOT/libreflect-runner" "$SOURCE_COPY/node_probe.js"
run_case libreflect-shell "${LIBREFLECT[@]}" /bin/sh -c 'printf shell-ok; /bin/echo shell-child'
run_case libreflect-coreutils "${LIBREFLECT[@]}" /bin/echo coreutils-ok
run_case libreflect-static-go "${LIBREFLECT[@]}" "$BUILD_ROOT/static-go-probe" alpha
run_case libreflect-esbuild-cli "${LIBREFLECT[@]}" "$ESBUILD_BINARY" \
  "$SOURCE_COPY/esbuild_entry.ts" --bundle --sourcemap \
  --outfile="$BUILD_ROOT/libreflect-esbuild.js"
run_case libreflect-esbuild-api "${LIBREFLECT[@]}" /usr/bin/node \
  "$SOURCE_COPY/esbuild_api_probe.js" "$ESBUILD_MODULE" \
  "$BUILD_ROOT/libreflect-runner" "$ESBUILD_BINARY" \
  "$SOURCE_COPY/esbuild_entry.ts"
run_case libreflect-direct-shebang "${LIBREFLECT[@]}" "$SOURCE_COPY/shebang_probe.sh" alpha
run_case libreflect-expanded-shebang "${LIBREFLECT[@]}" /bin/sh \
  "$SOURCE_COPY/shebang_probe.sh" alpha
run_case libreflect-state env RUNNER_CLOEXEC=1 RUNNER_SIGUSR1=1 \
  "${LIBREFLECT[@]}" "$BUILD_ROOT/compat-probe" state
run_case libreflect-residual-thread env RUNNER_BACKGROUND_THREAD=1 \
  "${LIBREFLECT[@]}" "$BUILD_ROOT/compat-probe" threaded

# Anvil: broadest pure-loader reference across ELF forms.
run_case anvil-c-probe "${ANVIL[@]}" "$BUILD_ROOT/compat-probe" alpha beta
run_case anvil-glibc-nopie "${ANVIL[@]}" \
  "$BUILD_ROOT/glibc-nopie-probe" alpha
run_case anvil-musl-static "${ANVIL[@]}" "$BUILD_ROOT/musl-static-probe" alpha
run_case anvil-node "${ANVIL[@]}" /usr/bin/node "$SOURCE_COPY/node_probe.js" alpha
run_case anvil-shell "${ANVIL[@]}" /bin/sh -c 'printf shell-ok; /bin/echo shell-child'
run_case anvil-coreutils "${ANVIL[@]}" /bin/echo coreutils-ok
run_case anvil-static-go "${ANVIL[@]}" "$BUILD_ROOT/static-go-probe" alpha
run_case anvil-esbuild-cli "${ANVIL[@]}" "$ESBUILD_BINARY" \
  "$SOURCE_COPY/esbuild_entry.ts" --bundle --sourcemap \
  --outfile="$BUILD_ROOT/anvil-esbuild.js"
run_case anvil-direct-shebang "${ANVIL[@]}" "$SOURCE_COPY/shebang_probe.sh" alpha

# Preserved SIGSYS/alternate-stack checks.
run_case sigsys-libreflect env LD_PRELOAD="$BUILD_ROOT/libsigsys-preload.so" \
  "${LIBREFLECT[@]}" "$BUILD_ROOT/sigsys-target"
run_case sigsys-anvil env LD_PRELOAD="$BUILD_ROOT/libsigsys-preload.so" \
  "${ANVIL[@]}" "$BUILD_ROOT/sigsys-target"
run_case sigsys-libreflect-c env LD_PRELOAD="$BUILD_ROOT/libsigsys-preload.so" \
  "${LIBREFLECT[@]}" "$BUILD_ROOT/compat-probe" sigsys
run_case sigsys-libreflect-node env LD_PRELOAD="$BUILD_ROOT/libsigsys-preload.so" \
  "${LIBREFLECT[@]}" /usr/bin/node "$SOURCE_COPY/node_probe.js" sigsys
run_case sigsys-libreflect-static-go env LD_PRELOAD="$BUILD_ROOT/libsigsys-preload.so" \
  "${LIBREFLECT[@]}" "$BUILD_ROOT/static-go-probe" sigsys
run_case sigsys-libreflect-esbuild env LD_PRELOAD="$BUILD_ROOT/libsigsys-preload.so" \
  "${LIBREFLECT[@]}" "$ESBUILD_BINARY" "$SOURCE_COPY/esbuild_entry.ts" \
  --bundle --outfile="$BUILD_ROOT/sigsys-esbuild.js"
run_case sigsys-full-libreflect-esbuild env \
  LD_PRELOAD="$BUILD_ROOT/libtrap-preload.so" \
  "${LIBREFLECT[@]}" "$ESBUILD_BINARY" "$SOURCE_COPY/esbuild_entry.ts" \
  --bundle --minify --outfile="$BUILD_ROOT/sigsys-full-esbuild.js"
run_case sigsys-full-libreflect-node env \
  LD_PRELOAD="$BUILD_ROOT/libtrap-preload.so" \
  "${LIBREFLECT[@]}" /usr/bin/node "$SOURCE_COPY/node_probe.js" fulltrap
run_case sigsys-anvil-static-go env LD_PRELOAD="$BUILD_ROOT/libsigsys-preload.so" \
  "${ANVIL[@]}" "$BUILD_ROOT/static-go-probe" sigsys

printf '%s\n' "$OUTPUT_ROOT"
