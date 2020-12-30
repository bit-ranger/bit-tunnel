#!/bin/bash

set -e

# macos
docker run --rm -u $(id -u):$(id -g) -v $(pwd):/workdir -e RUSTFLAGS="-C linker=/workdir/script/macbuild.sh" -e CROSS_TRIPLE=x86_64-apple-darwin rust-crossbuild /usr/local/rust/bin/cargo build --target=x86_64-apple-darwin  --release

