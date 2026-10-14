#![no_std]
#![no_main]

use core::arch::x86_64::__cpuid_count;
use sydon_rt::{io, println, sys, vec, Vec};

sydon_rt::main!(main);
sydon_rt::manifest!(class: Compute);

const N: usize = 256;
const ROUNDS: usize = 5;
const STREAM: usize = 2 << 20;

// i, k, j order so the inner loop runs along rows and vectorises
#[inline(always)]
fn matmul(a: &[f64], b: &[f64], c: &mut [f64]) {
    c.fill(0.0);
    for i in 0..N {
        for k in 0..N {
            let aik = a[i * N + k];
            let (row, out) = (&b[k * N..k * N + N], &mut c[i * N..i * N + N]);
            for j in 0..N {
                out[j] += aik * row[j];
            }
        }
    }
}

fn matmul_sse2(a: &[f64], b: &[f64], c: &mut [f64]) {
    matmul(a, b, c)
}

#[target_feature(enable = "avx2,fma")]
unsafe fn matmul_avx2(a: &[f64], b: &[f64], c: &mut [f64]) {
    matmul(a, b, c)
}

fn has_avx2_fma() -> bool {
    let fma = unsafe { __cpuid_count(1, 0) }.ecx & (1 << 12) != 0;
    let avx2 = unsafe { __cpuid_count(7, 0) }.ebx & (1 << 5) != 0;
    fma && avx2
}

fn best(mut f: impl FnMut()) -> u64 {
    (0..ROUNDS)
        .map(|_| {
            let t0 = sys::time_ns();
            f();
            sys::time_ns() - t0
        })
        .min()
        .unwrap()
}

// keeps one kernel busy for a while so several copies started a little
// apart all overlap during the timed part
fn load(core: usize, what: &str) -> u64 {
    const WARM: u64 = 1_000_000_000;
    const TIMED: u64 = 2_000_000_000;
    let a: Vec<f64> = (0..N * N).map(|i| (i % 7) as f64 * 0.5).collect();
    let b: Vec<f64> = (0..N * N).map(|i| (i % 5) as f64 * 0.25).collect();
    let mut c = vec![0.0f64; N * N];
    let (mut x, y, z) = (vec![0.0f64; STREAM], vec![1.0f64; STREAM], vec![2.0f64; STREAM]);
    let avx = has_avx2_fma();
    let mut step = || -> f64 {
        if what == "triad" {
            for i in 0..STREAM {
                x[i] = y[i] + 3.0 * z[i];
            }
            core::hint::black_box(&x);
            (3 * 8 * STREAM) as f64
        } else {
            if avx { unsafe { matmul_avx2(&a, &b, &mut c) } } else { matmul_sse2(&a, &b, &mut c) }
            core::hint::black_box(&c);
            2.0 * (N * N * N) as f64
        }
    };
    let t0 = sys::time_ns();
    while sys::time_ns() - t0 < WARM {
        step();
    }
    let (t1, mut done) = (sys::time_ns(), 0.0);
    while sys::time_ns() - t1 < TIMED {
        done += step();
    }
    let ns = sys::time_ns() - t1;
    while sys::time_ns() - t1 < TIMED + WARM {
        step();
    }
    let unit = if what == "triad" { "GB/s" } else { "GFLOP/s" };
    println!("flops: cpu {} load {}: {:.2} {}", core, what, done / ns as f64, unit);
    0
}

fn main() -> u64 {
    let core = sys::info().0;
    let args = io::args();
    if args.len() > 1 && args[1] == "load" {
        return load(core, args.get(2).map(|s| s.as_str()).unwrap_or("matmul"));
    }
    let a: Vec<f64> = (0..N * N).map(|i| (i % 7) as f64 * 0.5).collect();
    let b: Vec<f64> = (0..N * N).map(|i| (i % 5) as f64 * 0.25).collect();
    let mut c = vec![0.0f64; N * N];
    let flop = 2.0 * (N * N * N) as f64;

    let ns = best(|| matmul_sse2(&a, &b, &mut c));
    let check = c.iter().sum::<f64>();
    println!("flops: cpu {} matmul {} sse2: {:.2} GFLOP/s", core, N, flop / ns as f64);
    if has_avx2_fma() {
        let ns = best(|| unsafe { matmul_avx2(&a, &b, &mut c) });
        // both paths must agree before their speed means anything
        let same = c.iter().sum::<f64>() == check;
        println!("flops: cpu {} matmul {} avx2: {:.2} GFLOP/s{}", core, N, flop / ns as f64, if same { "" } else { " (WRONG RESULT)" });
    }

    let (mut x, y, z) = (vec![0.0f64; STREAM], vec![1.0f64; STREAM], vec![2.0f64; STREAM]);
    let ns = best(|| {
        for i in 0..STREAM {
            x[i] = y[i] + 3.0 * z[i];
        }
        core::hint::black_box(&x);
    });
    println!("flops: cpu {} stream triad: {:.2} GB/s", core, (3 * 8 * STREAM) as f64 / ns as f64);
    0
}
