#!/bin/sh
# the serial port is the terminal. ctrl-c goes to sydonOS, ctrl-a x quits qemu.
exec qemu-system-x86_64 \
    -machine q35 -m 256M -smp 4 \
    -drive file=out/disk.img,format=raw \
    -serial mon:stdio -display none \
    -no-reboot "$@"
