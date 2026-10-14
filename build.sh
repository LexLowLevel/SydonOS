#!/bin/sh
set -e
cd "$(dirname "$0")"

mkdir -p out

nasm -f bin boot/stage0.asm -o out/stage0.bin
nasm -f bin boot/stage1.asm -o out/stage1.bin
nasm -f bin boot/trampoline.asm -o out/trampoline.bin

<<<<<<< Updated upstream
# the jobs are built into the kernel image, so they come first
for job in shell hello echod ping ticker fault flood kv kvbench; do
=======
for job in shell hello echod ping ticker fault kv kvbench fsbench fswrite fsrand crunch flood yielder flops utils; do
>>>>>>> Stashed changes
    set -- "$@" -p "job-$job"
done
# jobs use the hard-float linux target so they get sse and avx. nothing of
# linux or libc is linked: they bring their own start, heap and linker script.
jobflags='["-C", "relocation-model=static", "-C", "link-arg=-nostartfiles", "-C", "link-arg=-nostdlib", "-C", "link-arg=-static", "-C", "link-arg=-no-pie"]'
cargo build --release --target x86_64-unknown-linux-gnu --config "target.x86_64-unknown-linux-gnu.rustflags=$jobflags" "$@"
cargo build --release --target x86_64-unknown-none -p sydon-kernel

rm -rf out/fsroot
mkdir -p out/fsroot/bin out/fsroot/data
for job in hello echod ping ticker fault kv kvbench fsbench fswrite fsrand crunch flood yielder flops; do
    cp "target/x86_64-unknown-linux-gnu/release/job-$job" "out/fsroot/bin/$job"
done
for util in echo cat grep wc head tail yes seq sort touch rm mkdir cp tee; do
    cp "target/x86_64-unknown-linux-gnu/release/$util" "out/fsroot/bin/$util"
done
echo "hello from the disk" > out/fsroot/data/readme
head -c 32M /dev/urandom > out/fsroot/data/big
mke2fs -q -F -t ext2 -b 4096 -d out/fsroot out/fs.img 96M

cargo run --release -p mkimage -- \
    out/disk.img out/stage0.bin out/stage1.bin \
    target/x86_64-unknown-none/release/sydon-kernel

echo "build ok"
