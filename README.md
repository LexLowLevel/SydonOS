## Benchmarks

Benchmarks run with qemu-x86_64 on an AMD Ryzen 7 4800H.

### RPC Latency (4 Cores)

| Scenario         | Min     | Avg     | Max (noise)     |
|------------------|---------|---------|----------|
| RPC (same core)  | 1.6 µs  | 1.7 µs  | 12.3 µs  |
| RPC (cross core) | 0.5 µs  | 0.6 µs  | 22.0 µs  |

> Max values are noise spikes.

### Ring Test and Scaling

| Cores | Parallel Streams | All-to-All  | RTT Polling Min |
|-------|------------------|-------------|-----------------|
| 2C    | 139M msgs/s      | 143M msgs/s | 89–99 ns        |
| 4C    | 233M msgs/s      | 280M msgs/s | 89–119 ns       |
| 8C    | 466M msgs/s      | 324M msgs/s | 99 ns           |
| 16C   | 549M msgs/s      | 416M msgs/s | 159–169 ns      |

> The 16C configuration is actually 8 physical cores with SMT (16 threads).
