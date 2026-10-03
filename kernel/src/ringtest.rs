use crate::acpi::MAX_CPUS;
use crate::fabric::{self, Msg};
use crate::{clock, cpu, smp};
use alloc::vec::Vec;

const STRESS: u64 = 1;
const DONE: u64 = 2;
const PING: u64 = 3;
const PONG: u64 = 4;
const MODE: u64 = 5;
const FLOOD: u64 = 6;
const FLOOD_DONE: u64 = 7;
const GO: u64 = 8;
const START: u64 = 9;
const REPORT: u64 = 10;
const DATA: u64 = 11;

const PAIRS: u64 = 1;
const ALL: u64 = 2;
const PAIR_MSGS: u64 = 2_000_000;
const ALL_MSGS: u64 = 300_000;

const STRESS_PER_PEER: u64 = 2000;
const RTT_ROUNDS: usize = 10_000;
const DOORBELL_ROUNDS: usize = 1000;
const FLOOD_MSGS: u64 = 1_000_000;
const BATCH: usize = 32;

// no core leaves before core 0 says GO, so nothing else can land in a ring
// while the test still reads it
pub fn run() {
    let me = smp::core();
    let cores = fabric::cores();
    if cores < 2 {
        return;
    }
    stress(me, cores);
    match me {
        0 => {
            bench();
            scaling(cores);
            for c in 1..cores {
                fabric::send(c, &msg(GO, 0, 0), unexpected);
            }
        }
        1 => respond(),
        _ => wait_go(),
    }
}

// data can beat the START that announced it: peers start sending as soon as
// their own START lands, and it travels on a different ring
fn wait_go() {
    let mut early = 0;
    loop {
        match fabric::poll() {
            Some((_, m)) if m.0[0] == GO => return,
            Some((_, m)) if m.0[0] == DATA => early += 1,
            Some((_, m)) if m.0[0] == START => {
                phase(m.0[1], early);
                early = 0;
            }
            Some((src, m)) => unexpected(src, m),
            None => fabric::wait(true),
        }
    }
}

fn phase(which: u64, early: u64) {
    let (me, cores) = (smp::core(), fabric::cores());
    let report = match which {
        PAIRS => pairs_role(me, cores, early),
        _ => Some(all_role(me, cores, &mut Vec::new(), early)),
    };
    if let Some((cycles, msgs)) = report {
        fabric::send(0, &msg(REPORT, cycles, msgs), unexpected);
    }
}

fn stream_to(dst: usize, count: u64) {
    let burst = [msg(DATA, 0, 0); BATCH];
    let mut sent = 0;
    while sent < count {
        let k = (count - sent).min(BATCH as u64) as usize;
        let n = fabric::push_burst(dst, &burst[..k]);
        if n == 0 {
            core::hint::spin_loop();
        }
        sent += n as u64;
    }
}

// the receiver times from its first message, so start skew between pairs does not count
fn pairs_role(me: usize, cores: usize, early: u64) -> Option<(u64, u64)> {
    if me % 2 == 0 {
        if me + 1 < cores {
            stream_to(me + 1, PAIR_MSGS);
        }
        return None;
    }
    let mut batch = [(0, Msg([0; 7])); BATCH];
    let mut got = early;
    let mut t0 = if early > 0 { cpu::rdtsc() } else { 0 };
    while got < PAIR_MSGS {
        let n = fabric::poll_many(&mut batch);
        if n > 0 && got == 0 {
            t0 = cpu::rdtsc();
        }
        for &(src, m) in &batch[..n] {
            if m.0[0] != DATA {
                unexpected(src, m);
            }
        }
        got += n as u64;
    }
    Some((cpu::rdtsc() - t0, got))
}

fn all_role(me: usize, cores: usize, early: &mut Vec<(u64, u64)>, early_data: u64) -> (u64, u64) {
    let t0 = cpu::rdtsc();
    let mut sent = [0u64; MAX_CPUS];
    let mut got = early_data;
    let want = ALL_MSGS * (cores as u64 - 1);
    let burst = [msg(DATA, 0, 0); BATCH];
    let mut batch = [(0, Msg([0; 7])); BATCH];
    while got < want || (0..cores).any(|p| p != me && sent[p] < ALL_MSGS) {
        for p in 0..cores {
            if p != me && sent[p] < ALL_MSGS {
                let k = (ALL_MSGS - sent[p]).min(BATCH as u64) as usize;
                sent[p] += fabric::push_burst(p, &burst[..k]) as u64;
            }
        }
        // take about as much as was just sent, or the rings fill up
        for _ in 1..cores {
            let n = fabric::poll_many(&mut batch);
            for &(src, m) in &batch[..n] {
                match m.0[0] {
                    DATA => got += 1,
                    REPORT if me == 0 => early.push((m.0[1], m.0[2])),
                    _ => unexpected(src, m),
                }
            }
            if n == 0 {
                break;
            }
        }
    }
    (cpu::rdtsc() - t0, got)
}

fn collect(want: usize, mut reports: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    while reports.len() < want {
        match fabric::poll() {
            Some((_, m)) if m.0[0] == REPORT => reports.push((m.0[1], m.0[2])),
            Some((src, m)) => unexpected(src, m),
            None => core::hint::spin_loop(),
        }
    }
    reports
}

fn scaling(cores: usize) {
    let rate = |cycles: u64, msgs: u64| msgs * clock::tsc_per_ms() * 1000 / cycles.max(1);

    let pairs = cores / 2;
    for c in 1..cores {
        fabric::send(c, &msg(START, PAIRS, 0), unexpected);
    }
    stream_to(1, PAIR_MSGS);
    let reports = collect(pairs, Vec::new());
    let total: u64 = reports.iter().map(|&(c, m)| rate(c, m)).sum();
    println!(
        "ringtest: {} parallel streams: {} M msgs/s total, {} M per stream",
        pairs,
        total / 1_000_000,
        total / pairs as u64 / 1_000_000
    );

    for c in 1..cores {
        fabric::send(c, &msg(START, ALL, 0), unexpected);
    }
    let mut early = Vec::new();
    let mine = all_role(0, cores, &mut early, 0);
    let mut reports = collect(cores - 1, early);
    reports.push(mine);
    let slowest = reports.iter().map(|&(c, _)| c).max().unwrap_or(1);
    let msgs: u64 = reports.iter().map(|&(_, m)| m).sum();
    let total = rate(slowest, msgs);
    println!(
        "ringtest: all-to-all on {} cores: {} M msgs/s total, {} M per core",
        cores,
        total / 1_000_000,
        total / cores as u64 / 1_000_000
    );
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

// 64 slots per ring against thousands of messages keeps the full-ring path busy
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
            None => fabric::wait(true),
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
    let mut early = 0;
    let mut batch = [(0, Msg([0; 7])); BATCH];
    loop {
        let n = fabric::poll_many(&mut batch);
        if n == 0 {
            if sleepy {
                fabric::wait(true);
            } else {
                core::hint::spin_loop();
            }
            continue;
        }
        for (i, &(src, m)) in batch[..n].iter().enumerate() {
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
                GO => return,
                DATA => early += 1,
                // the rest of this batch is data for the phase that starts now
                START => {
                    early += batch[i + 1..n].iter().filter(|b| b.1 .0[0] == DATA).count() as u64;
                    phase(m.0[1], early);
                    early = 0;
                    break;
                }
                _ => unexpected(src, m),
            }
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
    let tsc_per_ms = clock::tsc_per_ms();
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
    let mut burst = [Msg([0; 7]); BATCH];
    let mut i = 0;
    while i < FLOOD_MSGS {
        let k = (FLOOD_MSGS - i).min(BATCH as u64) as usize;
        for (j, m) in burst[..k].iter_mut().enumerate() {
            *m = msg(FLOOD, i + j as u64, FLOOD_MSGS);
        }
        let mut sent = 0;
        while sent < k {
            let n = fabric::push_burst(1, &burst[sent..k]);
            if n == 0 {
                core::hint::spin_loop();
            }
            sent += n;
        }
        i += k as u64;
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
}
