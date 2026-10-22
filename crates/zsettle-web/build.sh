#!/bin/sh
# Build the browser wallet into www/pkg: the WebAssembly module and its JavaScript glue.
# zsettle-app serves www/ with its page (--web-wallet crates/zsettle-web/www).
set -e
cd "$(dirname "$0")"
wasm-pack build --release --target web --out-dir www/pkg --no-typescript --no-pack
# The thread helper finds the main module by its folder, which only bundlers resolve.
sed -i "s|import('../../..')|import('../../../zsettle_web.js')|" www/pkg/snippets/*/src/workerHelpers.js
grep -q "zsettle_web.js" www/pkg/snippets/*/src/workerHelpers.js
rm -f www/pkg/.gitignore
