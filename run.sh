#!/bin/sh
exec qemu-system-x86_64 \
    -machine q35 -m 256M \
    -drive file=out/disk.img,format=raw \
    -serial stdio -display none -monitor none \
    -no-reboot "$@"
