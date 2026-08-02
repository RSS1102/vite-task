#!/bin/sh
set -eu

case "$(uname -m)" in
    aarch64 | arm64) ;;
    *)
        echo "this bounded experiment requires native AArch64 Linux" >&2
        exit 1
        ;;
esac

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
experiment_dir=$(mktemp -d)
trap 'rm -rf -- "$experiment_dir"' EXIT INT TERM

esbuild_version=${ESBUILD_VERSION:-0.28.1}
package_url="https://registry.npmjs.org/@esbuild/linux-arm64/-/linux-arm64-${esbuild_version}.tgz"

cc -O2 -g -Wall -Wextra -Werror -std=gnu11 \
    -o "$experiment_dir/esbuild-injector" "$script_dir/esbuild_injector.c"
curl -L --fail --silent "$package_url" -o "$experiment_dir/esbuild.tgz"
mkdir "$experiment_dir/package"
tar -xzf "$experiment_dir/esbuild.tgz" -C "$experiment_dir/package" \
    --strip-components=1
cp "$script_dir/esbuild_input.js" "$experiment_dir/input.js"

cd "$experiment_dir"
./esbuild-injector "$(realpath package/bin/esbuild)" --version
./esbuild-injector "$(realpath package/bin/esbuild)" input.js --bundle \
    --platform=node --outfile=out.js
test -s out.js
grep -q 'answer' out.js

echo "PASS: esbuild ${esbuild_version} version and bundle operations completed"
