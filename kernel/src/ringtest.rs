use crate::acpi::MAX_CPUS;
use crate::fabric::{self, Msg};
use crate::{cpu, pit, smp, task};
use alloc::vec::Vec;

const STRESS: u64 = 1;
const DONE: u64 = 2;
const PING: u64 = 3;
const PONG: u64 = 4;
const MODE: u64 = 5;
const FLOOD: u64 = 6;
const FLOOD_DONE: u64 = 7;
const STOP: u64 = 8;

const STRESS_PER_PEER: u64 = 2000;
const RTT_ROUNDS: usize = 10_000;
const DOORBELL_ROUNDS: usize = 1000;
const FLOOD_MSGS: u64 = 100_000;

pub fn spawn() {
    task::spawn(run, 0).expect("ringtest: no room for its task");
}

fn run(_: u64) {
    let me = smp::core();
    let cores = fabric::cores();
    if cores < 2 {
        return;
    }
    stress(me, cores);
    match me {
        0 => bench(),
        1 => respond(),
        _ => {}
    }
}

fn msg(kind: u64, a: u64, b: u64) -> Msg {
    Msg([kind, a, b, 0, 0, 0, 0])
}

fn unexpected(src: usize, m: Msg) {
    panic!("ringtest: unexpected {:?} from cpu {}", m, src);
}

// a value the receiver can recompute, so a torn or misrouted slot shows up
fn check(src: usize, dst: usize, n: u64) -> u64 {
    (n ^ 0x9E37_79B9_7F4A_7C15).rotate_left((src * 7 + dst) as u32 % 64)
}

struct Stress {
    me: usize,
    next: [u64; MAX_CPUS],
    done: usize,
}

impl Stress {
    fn take(&mut self, src: usize, m: Msg) {
        match m.0[0] {
            STRESS => {
                let n = m.0[1];
                assert!(n == self.next[src], "ringtest: cpu {} sent {} out of order", src, n);
                assert!(m.0[2] == check(src, self.me, n), "ringtest: bad payload from cpu {}", src);
                self.next[src] += 1;
            }
            DONE => self.done += 1,
            _ => unexpected(src, m),
        }
    }

    fn received_all(&self, cores: usize) -> bool {
        (0..cores).all(|p| p == self.me || self.next[p] == STRESS_PER_PEER)
    }
}

// every core sends to every other core at once. 64 slots per ring against
// thousands of messages keeps the full-ring path busy the whole time.
fn stress(me: usize, cores: usize) {
    let mut st = Stress { me, next: [0; MAX_CPUS], done: 0 };
    for n in 0..STRESS_PER_PEER {
        for peer in (0..cores).filter(|&p| p != me) {
            fabric::send(peer, &msg(STRESS, n, check(me, peer, n)), |src, m| st.take(src, m));
        }
    }
    let finished = |st: &Stress| st.received_all(cores) && (me != 0 || st.done == cores - 1);
    while !finished(&st) {
        match fabric::poll() {
            Some((src, m)) => st.take(src, m),
            None => fabric::wait(),
        }
    }
    if me == 0 {
        let total = STRESS_PER_PEER * (cores * (cores - 1)) as u64;
        println!("ringtest: all-pairs ok, {} messages over {} rings", total, cores * (cores - 1));
    } else {
        fabric::send(0, &msg(DONE, 0, 0), unexpected);
    }
}

fn respond() {
    let mut sleepy = false;
    let mut flood = 0;
    loop {
        let Some((src, m)) = fabric::poll() else {
            if sleepy {
                fabric::wait();
            } else {
                core::hint::spin_loop();
            }
            continue;
        };
        match m.0[0] {
            PING => fabric::send(src, &msg(PONG, m.0[1], 0), unexpected),
            MODE => sleepy = m.0[1] != 0,
            FLOOD => {
                flood += 1;
                if flood == m.0[2] {
                    flood = 0;
                    fabric::send(src, &msg(FLOOD_DONE, 0, 0), unexpected);
                }
            }
            STOP => return,
            _ => unexpected(src, m),
        }
    }
}

fn recv_from(peer: usize) -> Msg {
    loop {
        match fabric::poll() {
            Some((src, m)) if src == peer => return m,
            Some((src, m)) => unexpected(src, m),
            None => core::hint::spin_loop(),
        }
    }
}

fn round_trips(rounds: usize, wait_for_sleep: bool) -> Vec<u64> {
    let mut samples = Vec::with_capacity(rounds);
    for i in 0..rounds as u64 {
        // start timing only once the peer has really gone to sleep
        while wait_for_sleep && !fabric::asleep(1) {
            core::hint::spin_loop();
        }
        let t0 = cpu::rdtsc();
        fabric::send(1, &msg(PING, i, 0), unexpected);
        let m = recv_from(1);
        samples.push(cpu::rdtsc() - t0);
        assert!(m.0[0] == PONG && m.0[1] == i, "ringtest: bad pong {:?}", m);
    }
    samples.sort_unstable();
    samples
}

fn bench() {
    let t0 = cpu::rdtsc();
    pit::sleep_us(10_000);
    let tsc_per_ms = (cpu::rdtsc() - t0) / 10;
    let ns = |cycles: u64| cycles * 1_000_000 / tsc_per_ms;

    let report = |name: &str, s: &[u64]| {
        let (min, med) = (s[0], s[s.len() / 2]);
        println!(
            "ringtest: rtt {:<9} min {:>7} ns  median {:>7} ns  ({} rounds)",
            name,
            ns(min),
            ns(med),
            s.len()
        );
    };

    report("polling", &round_trips(RTT_ROUNDS, false));

    fabric::send(1, &msg(MODE, 1, 0), unexpected);
    report("doorbell", &round_trips(DOORBELL_ROUNDS, true));
    fabric::send(1, &msg(MODE, 0, 0), unexpected);

    let t0 = cpu::rdtsc();
    for i in 0..FLOOD_MSGS {
        fabric::send(1, &msg(FLOOD, i, FLOOD_MSGS), unexpected);
    }
    let m = recv_from(1);
    assert!(m.0[0] == FLOOD_DONE, "ringtest: bad flood reply {:?}", m);
    let elapsed = ns(cpu::rdtsc() - t0).max(1);
    println!(
        "ringtest: stream {} msgs in {} us, {} msgs/s one way",
        FLOOD_MSGS,
        elapsed / 1000,
        FLOOD_MSGS * 1_000_000_000 / elapsed
    );

    fabric::send(1, &msg(STOP, 0, 0), unexpected);
}
