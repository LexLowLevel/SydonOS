use crate::cpu;

const HZ: u64 = 1_193_182;
const CH2: u16 = 0x42;
const CMD: u16 = 0x43;
const GATE: u16 = 0x61;
const OUT2: u8 = 0x20;

// busy-waits on channel 2, the one channel that can be polled without an IRQ
pub fn sleep_us(us: u64) {
    let mut left = us * HZ / 1_000_000;
    while left > 0 {
        let n = left.min(0xFFFF);
        let g = cpu::inb(GATE) & !0x03;
        cpu::outb(GATE, g);
        cpu::outb(CMD, 0b1011_0000);
        cpu::outb(CH2, n as u8);
        cpu::outb(CH2, (n >> 8) as u8);
        cpu::outb(GATE, g | 0x01);
        while cpu::inb(GATE) & OUT2 == 0 {
            core::hint::spin_loop();
        }
        left -= n;
    }
}
