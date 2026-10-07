#!/usr/bin/env python3
"""sydonOS benchmark runner. see ./bench.sh --help"""

import argparse
import json
import os
import re
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
OUT = os.path.join(ROOT, "out", "bench")
PROMPT = "sydon> "



def read(path):
    with open(path) as f:
        return f.read().strip()


def parse_list(text):
    cpus = []
    for part in text.split(","):
        if "-" in part:
            a, b = part.split("-")
            cpus.extend(range(int(a), int(b) + 1))
        elif part:
            cpus.append(int(part))
    return cpus


def host_cores():
    """one cpu per physical core, grouped by the L3 they share"""
    base = "/sys/devices/system/cpu"
    online = parse_list(read(f"{base}/online"))
    seen, clusters = set(), {}
    for cpu in online:
        topo = f"{base}/cpu{cpu}/topology"
        key = (read(f"{topo}/physical_package_id"), read(f"{topo}/core_id"))
        if key in seen:
            continue
        seen.add(key)
        try:
            l3 = read(f"{base}/cpu{cpu}/cache/index3/shared_cpu_list")
        except OSError:
            l3 = "all"
        clusters.setdefault(l3, []).append(cpu)
    return list(clusters.values())


def pick_cpus(n, spread=False):
    clusters = host_cores()
    if spread:
        order = [c for group in zip(*clusters) for c in group]
    else:
        order = [c for group in clusters for c in group]
    if n > len(order):
        extra = [c for c in parse_list(read("/sys/devices/system/cpu/online")) if c not in order]
        order += extra
        print(f"note: only {sum(map(len, clusters))} physical cores, using SMT siblings too", file=sys.stderr)
    return order[:n]


def cluster_of(cpu):
    for i, group in enumerate(host_cores()):
        if cpu in group:
            return i
    return 0


def qemu_topology(cpus):
    """present the host's L3 clusters to the guest as dies, when they split evenly"""
    per = {}
    for c in cpus:
        per.setdefault(cluster_of(c), []).append(c)
    counts = {len(v) for v in per.values()}
    if len(per) < 2 or len(set(cpus)) < len(cpus) or len(counts) != 1:
        return str(len(cpus)), "q35"
    # vcpus are numbered die by die, so reorder the pinning to match
    ordered = [c for k in sorted(per) for c in per[k]]
    cpus[:] = ordered
    smp = f"{len(cpus)},sockets=1,dies={len(per)},cores={counts.pop()}"
    return smp, "q35,smp-cache.0.cache=l3,smp-cache.0.topology=die"



class Vm:
    def __init__(self, cpus, args, log_path):
        self.cpus = list(cpus)
        self.log = open(log_path, "w")
        self.text = ""
        self.dir = tempfile.mkdtemp(prefix="sydon-bench-")
        qmp, ser = os.path.join(self.dir, "qmp"), os.path.join(self.dir, "ser")
        smp, machine = qemu_topology(self.cpus)
        accel = ["-accel", "kvm", "-cpu", "host"] if args.kvm else ["-accel", "tcg"]
        self.proc = subprocess.Popen(
            ["qemu-system-x86_64", "-machine", machine, "-smp", smp, "-m", args.mem,
             *accel, "-name", "sydon,debug-threads=on",
             "-drive", f"file={os.path.join(ROOT, 'out', 'disk.img')},format=raw,snapshot=on",
             "-display", "none", "-no-reboot",
             "-chardev", f"socket,id=ser,path={ser},server=on,wait=off", "-serial", "chardev:ser",
             "-S", "-qmp", f"unix:{qmp},server,nowait"],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        self.qmp = self._connect(qmp).makefile("rw")
        json.loads(self.qmp.readline())
        self._qmp("qmp_capabilities")
        self.ser = self._connect(ser)
        self.ser.setblocking(False)
        if args.pin:
            self._pin()
        self._qmp("cont")

    def _connect(self, path):
        for _ in range(200):
            try:
                s = socket.socket(socket.AF_UNIX)
                s.connect(path)
                return s
            except OSError:
                if self.proc.poll() is not None:
                    raise SystemExit("qemu failed:\n" + self.proc.stderr.read().decode())
                time.sleep(0.05)
        raise SystemExit("qemu did not come up")

    def _qmp(self, cmd):
        self.qmp.write(json.dumps({"execute": cmd}) + "\n")
        self.qmp.flush()
        while True:
            r = json.loads(self.qmp.readline())
            if "return" in r or "error" in r:
                return r

    def _pin(self):
        vcpus = {c["cpu-index"]: c["thread-id"] for c in self._qmp("query-cpus-fast")["return"]}
        rest = set(parse_list(read("/sys/devices/system/cpu/online"))) - set(self.cpus)
        if rest:
            for tid in os.listdir(f"/proc/{self.proc.pid}/task"):
                os.sched_setaffinity(int(tid), rest)
        for idx, tid in vcpus.items():
            os.sched_setaffinity(tid, {self.cpus[idx]})

    def pump(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            try:
                data = self.ser.recv(65536)
            except BlockingIOError:
                time.sleep(0.02)
                continue
            if not data:
                return
            s = data.decode(errors="replace")
            self.text += s
            self.log.write(s)
            self.log.flush()

    # the ring test runs at boot, then the director starts the shell
    def boot(self, timeout=120):
        end = time.time() + timeout
        while PROMPT not in self.text:
            if time.time() > end or "kernel panic" in self.text:
                raise RuntimeError("boot failed, see " + self.log.name)
            self.pump(0.1)
        return self.text

    # types a line at the shell and returns what came back up to the next prompt
    def cmd(self, line, timeout=300):
        start = len(self.text)
        before = self.text.count(PROMPT)
        self.ser.send((line + "\r").encode())
        end = time.time() + timeout
        while self.text.count(PROMPT) <= before:
            if time.time() > end:
                raise RuntimeError(f"'{line}' timed out, see {self.log.name}")
            self.pump(0.05)
        return self.text[start:]

    def close(self):
        try:
            self._qmp("quit")
        except Exception:
            pass
        try:
            self.qmp.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.send_signal(signal.SIGKILL)
        self.log.close()
        shutil.rmtree(self.dir, ignore_errors=True)



class Results:
    def __init__(self):
        self.values = {}
        self.units = {}

    def add(self, name, value, unit):
        self.values.setdefault(name, []).append(value)
        self.units[name] = unit

    def show(self, title):
        print(f"\n{title}")
        if not self.values:
            print("  no results")
            return
        width = max(len(n) for n in self.values)
        for name, vals in self.values.items():
            med = statistics.median(vals)
            spread = f"({fmt(min(vals))} - {fmt(max(vals))})" if len(vals) > 1 else ""
            print(f"  {name:<{width}}  {fmt(med):>10} {self.units[name]:<9} {spread}")


def fmt(v):
    return f"{v:,.0f}" if v >= 100 else f"{v:.2f}" if v < 10 else f"{v:.1f}"


def num(s):
    return float(s.replace(",", ""))


def parse_ring(text, res):
    m = re.search(r"rtt polling\s+min\s+(\d+) ns\s+median\s+(\d+) ns", text)
    if m:
        res.add("ring rtt min", num(m.group(1)), "ns")
        res.add("ring rtt median", num(m.group(2)), "ns")
    m = re.search(r"rtt doorbell\s+min\s+(\d+) ns\s+median\s+(\d+) ns", text)
    if m:
        res.add("doorbell rtt median", num(m.group(2)), "ns")
    m = re.search(r"stream \d+ msgs in \d+ us, (\d+) msgs/s", text)
    if m:
        res.add("single stream", num(m.group(1)) / 1e6, "M msgs/s")
    m = re.search(r"(\d+) parallel streams: (\d+) M msgs/s total", text)
    if m:
        res.add(f"{m.group(1)} parallel streams, total", num(m.group(2)), "M msgs/s")
    m = re.search(r"all-to-all on (\d+) cores: (\d+) M msgs/s total", text)
    if m:
        res.add(f"all-to-all on {m.group(1)} cores, total", num(m.group(2)), "M msgs/s")


PING = r"ping: \d+ calls cpu (\d+) -> cpu (\d+): min ([\d.]+) us, avg ([\d.]+) us, max ([\d.]+) us"
KVBENCH = r"kvbench: cpu \d+ did \d+ ops on \d+ shards in \d+ ms: (\d+) k ops/s, (\d+) failed"


def started_on(out):
    m = re.search(r"started \w+ as pid (\d+) on cpu (\d+)", out)
    if not m:
        raise RuntimeError("spawn failed:\n" + out)
    return int(m.group(1)), int(m.group(2))


def run_vm(args, cores, label, body, spread=False):
    os.makedirs(args.logs, exist_ok=True)
    online = parse_list(read("/sys/devices/system/cpu/online"))
    if cores > len(online) and not args.cpus:
        if args.pin:
            raise SystemExit(f"{cores} cores asked for but the host has {len(online)} cpus, and each vcpu is "
                             f"pinned to its own host cpu\npass --no-pin to overcommit (numbers will be noisy)")
        print(f"note: {cores} vcpus on {len(online)} host cpus, overcommitted", file=sys.stderr)
        cpus = (online * (cores // len(online) + 1))[:cores]
    else:
        cpus = args.cpus[:cores] if args.cpus else pick_cpus(cores, spread)
    if len(cpus) < cores:
        raise SystemExit(f"--cpus lists {len(cpus)} host cpus but {cores} cores were asked for\n"
                         f"--cpus picks which host cpus to pin to (e.g. 0,1,2,3); to set how many cores, "
                         f"use --cores, or --counts for scaling (e.g. --counts 2 4 8 16)")
    log = os.path.join(args.logs, f"{label}.log")
    vm = Vm(cpus, args, log)
    try:
        vm.boot()
        return body(vm)
    finally:
        vm.close()


def bench_ring(args):
    res = Results()
    for r in range(args.runs):
        run_vm(args, args.cores, f"ring-{args.cores}c-run{r}", lambda vm: parse_ring(vm.text, res))
    res.show(f"ring benchmarks, {args.cores} cores, {args.runs} runs, median (min - max)")


def bench_scaling(args):
    counts = args.counts or [2, 4, 8]
    for n in counts:
        res = Results()
        for r in range(args.runs):
            run_vm(args, n, f"scaling-{n}c-run{r}", lambda vm: parse_ring(vm.text, res))
        res.show(f"{n} cores, {args.runs} runs")


# echod on core 1, then ping from core 1 itself and from every other core
def bench_rpc(args):
    res = Results()

    def body(vm):
        vm.cmd("spawn echod --on 1")
        for core in [1] + [c for c in range(args.cores) if c != 1]:
            m = re.search(PING, vm.cmd(f"run ping --on {core}"))
            if m:
                name = "same core as echo" if m.group(1) == m.group(2) else "other core"
                res.add(f"{name}: avg", num(m.group(4)), "us")
                res.add(f"{name}: min", num(m.group(3)), "us")

    for r in range(args.runs):
        run_vm(args, args.cores, f"rpc-{args.cores}c-run{r}", body)
    res.show(f"rpc round trip (ping -> echo, 1000 calls), {args.cores} cores, {args.runs} runs")


def bench_kv(args):
    res = Results()

    def scenario(shards, clients):
        def body(vm):
            for _ in range(shards):
                started_on(vm.cmd("spawn kv"))
            pids = [started_on(vm.cmd("spawn kvbench"))[0] for _ in range(clients)]
            for pid in pids:
                vm.cmd(f"wait {pid}")
            total, failed = 0.0, 0
            for m in re.finditer(KVBENCH, vm.text):
                total += num(m.group(1))
                failed += int(m.group(2))
            if failed:
                print(f"warning: {failed} kv operations failed", file=sys.stderr)
            res.add(f"{shards} shards, {clients} clients", total, "k ops/s")
        return body

    for shards in args.shards:
        for clients in args.clients:
            for r in range(args.runs):
                run_vm(args, args.cores, f"kv-{shards}s-{clients}c-run{r}", scenario(shards, clients))
    res.show(f"kv store, total throughput, {args.cores} cores, {args.runs} runs")


def main():
    p = argparse.ArgumentParser(
        prog="bench.sh",
        description="Boots sydonOS under QEMU with every virtual core pinned to its own physical core, "
                    "runs a benchmark through the shell several times and prints median (min - max).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""benchmarks:
  ring       the boot-time ring test: round trips, doorbell, streaming, all-to-all
  scaling    the ring test on several core counts (--counts 2 4 8)
  rpc        ping -> echo round trips: same core and every other core
  kv         sharded key-value store (--shards 1 4 --clients 1 4)

examples:
  ./bench.sh ring --cores 4
  ./bench.sh scaling --counts 2 4 6 8 --runs 3
  ./bench.sh rpc --cores 8
  ./bench.sh kv --shards 1 4 --clients 4""")

    p.add_argument("bench", choices=["ring", "scaling", "rpc", "kv"])
    p.add_argument("--cores", type=int, default=4, help="virtual cores (default 4)")
    p.add_argument("--counts", type=int, nargs="+", help="core counts for scaling")
    p.add_argument("--runs", type=int, default=3, help="repeat each measurement (default 3)")
    p.add_argument("--cpus", type=lambda s: [int(c) for c in s.split(",")], help="host cpus to pin to, e.g. 0,2,4,6")
    p.add_argument("--shards", type=int, nargs="+", default=[1, 4])
    p.add_argument("--clients", type=int, nargs="+", default=[1, 4])
    p.add_argument("--mem", default="512M")
    p.add_argument("--no-kvm", dest="kvm", action="store_false", help="use TCG emulation")
    p.add_argument("--no-pin", dest="pin", action="store_false", help="let the host schedule the vcpus")
    p.add_argument("--build", action="store_true", help="run ./build.sh first")
    args = p.parse_args()

    if args.kvm and not os.access("/dev/kvm", os.R_OK | os.W_OK):
        print("note: /dev/kvm is not usable, falling back to TCG", file=sys.stderr)
        args.kvm = False
    if args.build:
        subprocess.run([os.path.join(ROOT, "build.sh")], check=True)
    if args.cpus:
        bad = set(args.cpus) - set(parse_list(read("/sys/devices/system/cpu/online")))
        if bad:
            raise SystemExit(f"--cpus takes host cpu ids, not a count; {sorted(bad)} not online "
                             f"(online: {read('/sys/devices/system/cpu/online').strip()})\n"
                             f"to set how many cores, use --cores, or --counts for scaling (e.g. --counts 2 4 8 16)")
    if not os.path.exists(os.path.join(ROOT, "out", "disk.img")):
        raise SystemExit("out/disk.img missing, run ./build.sh or pass --build")
    args.logs = os.path.join(OUT, time.strftime("%Y%m%d-%H%M%S") + f"-{args.bench}")

    {"ring": bench_ring, "scaling": bench_scaling, "rpc": bench_rpc, "kv": bench_kv}[args.bench](args)
    print(f"\nlogs: {os.path.relpath(args.logs, ROOT)}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
