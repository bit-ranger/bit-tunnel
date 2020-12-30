#!/bin/bash

set -e


# windows
docker run --rm -u $(id -u):$(id -g) -v $(pwd):/workdir -e CROSS_TRIPLE=x86_64-w64-mingw32 rust-crossbuild /usr/local/rust/bin/cargo build --target=x86_64-pc-windows-gnu  --release

