#!/bin/sh
# the serial port is the terminal. ctrl-c goes to sydonOS, ctrl-a x quits qemu.
exec qemu-system-x86_64 \
    -machine q35 -m 256M -smp 4 \
    -drive file=out/disk.img,format=raw \
    -drive file=out/fs.img,if=none,id=fs,format=raw \
    -device virtio-blk-pci,drive=fs,disable-legacy=on \
    -serial mon:stdio -display none \
    -no-reboot "$@"
