#!/bin/sh
set -e
cd "$(dirname "$0")"

mkdir -p out

nasm -f bin boot/stage0.asm -o out/stage0.bin
nasm -f bin boot/stage1.asm -o out/stage1.bin
nasm -f bin boot/trampoline.asm -o out/trampoline.bin

# the jobs are built into the kernel image, so they come first
for job in shell hello echod ping ticker fault flood kv kvbench; do
    set -- "$@" -p "job-$job"
done
cargo build --release --target x86_64-unknown-none "$@"
cargo build --release --target x86_64-unknown-none -p sydon-kernel

cargo run --release -p mkimage -- \
    out/disk.img out/stage0.bin out/stage1.bin \
    target/x86_64-unknown-none/release/sydon-kernel

echo "build ok"
