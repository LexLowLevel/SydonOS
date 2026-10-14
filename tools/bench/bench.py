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


def siblings(cpu):
    return parse_list(read(f"/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list"))


def qemu_topology(cpus):
    """present the host's L3 clusters to the guest as dies and its SMT siblings
    as threads, when they split evenly"""
    if len(set(cpus)) < len(cpus):
        return str(len(cpus)), "q35"
    groups = sorted({tuple(s for s in siblings(c) if s in cpus) for c in cpus})
    if len({len(g) for g in groups}) != 1:
        groups = [(c,) for c in sorted(cpus)]
    threads = len(groups[0])
    per = {}
    for g in groups:
        per.setdefault(cluster_of(g[0]), []).append(g)
    if len({len(v) for v in per.values()}) != 1:
        per = {0: groups}
    # vcpus are numbered die by die, core by core, thread by thread
    cpus[:] = [c for k in sorted(per) for g in per[k] for c in g]
    dies, cores = len(per), len(groups) // len(per)
    smp = f"{len(cpus)},sockets=1,dies={dies},cores={cores},threads={threads}"
    machine = "q35,smp-cache.0.cache=l3,smp-cache.0.topology=die" if dies > 1 else "q35"
    return smp, machine


class Vm:
    def __init__(self, cpus, args, log_path, linux=None):
        self.cpus = list(cpus)
        self.log = open(log_path, "w")
        self.text = ""
        self.dir = tempfile.mkdtemp(prefix="sydon-bench-")
        qmp, ser = os.path.join(self.dir, "qmp"), os.path.join(self.dir, "ser")
        smp, machine = qemu_topology(self.cpus)
        accel = ["-accel", "kvm", "-cpu", "host"] if args.kvm else ["-accel", "tcg"]
        if getattr(args, "cpu_pm", False):
            # the guest gets mwait, and hlt and mwait no longer exit to the host
            accel += ["-overcommit", "cpu-pm=on"]
        self.proc = subprocess.Popen(
            ["qemu-system-x86_64", "-machine", machine, "-smp", smp, "-m", args.mem,
             *accel, "-name", "sydon,debug-threads=on",
             *(["-kernel", linux[0], "-initrd", linux[1], "-append", linux[2]] if linux else
               ["-drive", f"file={os.path.join(ROOT, 'out', 'disk.img')},format=raw,snapshot=on"]),
             "-drive", f"file={args.fs or os.path.join(ROOT, 'out', 'fs.img')},if=none,id=fs,format=raw"
                       f"{'' if args.fs else ',snapshot=on'}{args.disk_opts}",
             *args.disk_device,
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

<<<<<<< Updated upstream
    # the ring test runs at boot, then the director starts the shell
=======
>>>>>>> Stashed changes
    def boot(self, timeout=120):
        end = time.time() + timeout
        while PROMPT not in self.text:
            if time.time() > end or "kernel panic" in self.text:
                raise RuntimeError("boot failed, see " + self.log.name)
            self.pump(0.1)
        return self.text

<<<<<<< Updated upstream
    # types a line at the shell and returns what came back up to the next prompt
=======
>>>>>>> Stashed changes
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
<<<<<<< Updated upstream
=======
FSBENCH_SEQ = r"fsbench: cpu (\d+) read \d+ MiB sequentially in \d+ ms: (\d+) MiB/s"
FSBENCH_RAND = r"fsbench: cpu \d+ did \d+ random 4K reads: (\d+) k reads/s"
FSRAND = r"fsrand: cpu \d+ did \d+ random 4K reads: (\d+) k reads/s"
READ_FAILURES = r"random 4K reads: \d+ k reads/s, (\d+) failed"
>>>>>>> Stashed changes
KVBENCH = r"kvbench: cpu \d+ did \d+ ops on \d+ shards in \d+ ms: (\d+) k ops/s, (\d+) failed"


def started_on(out):
    m = re.search(r"started \w+ as pid (\d+) on cpu (\d+)", out)
    if not m:
        raise RuntimeError("spawn failed:\n" + out)
    return int(m.group(1)), int(m.group(2))
<<<<<<< Updated upstream
=======

>>>>>>> Stashed changes


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
<<<<<<< Updated upstream
        vm.cmd("spawn echod --on 1")
        for core in [1] + [c for c in range(args.cores) if c != 1]:
            m = re.search(PING, vm.cmd(f"run ping --on {core}"))
            if m:
                name = "same core as echo" if m.group(1) == m.group(2) else "other core"
                res.add(f"{name}: avg", num(m.group(4)), "us")
                res.add(f"{name}: min", num(m.group(3)), "us")
=======
        _, echo = started_on(vm.cmd("spawn echod"))
        variants = [("placed by director", "run ping")]
        variants.append(("same core as echo", f"run ping --on {echo}"))
        clusters = {}
        for line in vm.cmd("cores").splitlines():
            m = re.match(r"\s+(\d+)\s+(\d+)\s+up", line)
            if m:
                clusters[int(m.group(1))] = int(m.group(2))
        other = [c for c, cl in clusters.items() if cl != clusters.get(echo) and c != 0]
        if other:
            variants.append(("other L3 cluster", f"run ping --on {other[0]}"))
        for name, command in variants:
            m = re.search(PING, vm.cmd(command))
            if m:
                res.add(f"{name}: avg", num(m.group(4)), "us")
                res.add(f"{name}: min", num(m.group(3)), "us")
        if args.stats:
            print(vm.cmd("stats"))
>>>>>>> Stashed changes

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
<<<<<<< Updated upstream
            for pid in pids:
                vm.cmd(f"wait {pid}")
            total, failed = 0.0, 0
=======
            total, failed = 0.0, 0
            for pid in pids:
                vm.cmd(f"wait {pid}")
>>>>>>> Stashed changes
            for m in re.finditer(KVBENCH, vm.text):
                total += num(m.group(1))
                failed += int(m.group(2))
            if failed:
                print(f"warning: {failed} kv operations failed", file=sys.stderr)
            res.add(f"{shards} shards, {clients} clients", total, "k ops/s")
<<<<<<< Updated upstream
=======
            if args.stats:
                print(vm.cmd("stats"))
>>>>>>> Stashed changes
        return body

    for shards in args.shards:
        for clients in args.clients:
            for r in range(args.runs):
                run_vm(args, args.cores, f"kv-{shards}s-{clients}c-run{r}", scenario(shards, clients))
    res.show(f"kv store, total throughput, {args.cores} cores, {args.runs} runs")


<<<<<<< Updated upstream
=======
def bench_io(args):
    res = Results()

    def body(vm):
        vm.cmd("run fsbench")
        vm.cmd("run fsbench")
        owner = int(re.search(r"storage: virtio-blk on cpu (\d+)", vm.text).group(1))
        other = next(c for c in range(args.cores) if c not in (0, owner)) if args.cores > 2 else 0
        runs = [("placed by director", "run fsbench"), ("on the disk owner", f"run fsbench --on {owner}"),
                ("on another core", f"run fsbench --on {other}")]
        for name, command in runs:
            out = vm.cmd(command)
            record(name, out)
        for name, command in [("fsrand placed by director", "run fsrand"), ("fsrand on another core", f"run fsrand --on {other}")]:
            m = re.search(FSRAND, vm.cmd(command))
            if m:
                res.add(f"{name}: random 4K", num(m.group(1)), "k reads/s")
        hogs = [started_on(vm.cmd(f"spawn crunch --on {c}"))[0] for c in (owner, other)]
        for name, core in [("both busy, owner", owner), ("both busy, other core", other)]:
            record(name, vm.cmd(f"run fsbench --on {core}"))
            m = re.search(FSRAND, vm.cmd(f"run fsrand --on {core}"))
            if m:
                res.add(f"{name}: fsrand", num(m.group(1)), "k reads/s")
        for pid in hogs:
            vm.cmd(f"wait {pid}")
        if args.stats:
            print(vm.cmd("stats"))

    def record(name, out):
        seq, rand = re.search(FSBENCH_SEQ, out), re.search(FSBENCH_RAND, out)
        if seq:
            res.add(f"{name}: sequential", num(seq.group(2)), "MiB/s")
        if rand:
            res.add(f"{name}: random 4K", num(rand.group(1)), "k reads/s")

    def checked(vm):
        body(vm)
        failed = sum(int(m.group(1)) for m in re.finditer(READ_FAILURES, vm.text))
        if failed:
            print(f"warning: {failed} random reads failed and were not counted", file=sys.stderr)

    for r in range(args.runs):
        run_vm(args, args.cores, f"io-{args.cores}c-run{r}", checked)
    res.show(f"file reads (fsbench on /data/big), {args.cores} cores, {args.runs} runs")


REAL = r"real ([\d.]+) ms"


def bench_pipe(args):
    res = Results()
    n = args.bytes

    def clusters_of(vm):
        out = {}
        for line in vm.cmd("cores").splitlines():
            m = re.match(r"\s+(\d+)\s+(\d+)\s+up", line)
            if m:
                out[int(m.group(1))] = int(m.group(2))
        return out

    def body(vm):
        cl = clusters_of(vm)
        cores = sorted(cl)
        variants = [("placed by director", (None, None, None))]
        if len(cores) > 2:
            variants.append(("all on one core", (cores[2],) * 3))
        if len(cores) > 3:
            variants.append(("one core each", tuple(cores[1:4])))
        groups = {}
        for c in cores[1:]:
            groups.setdefault(cl[c], []).append(c)
        if len(groups) > 1:
            a, b = list(groups.values())[:2]
            variants.append(("hops across L3", (a[0], b[0], a[-1])))
        vm.cmd(f"yes | head -c {n} | wc -c", timeout=120)
        for name, (x, y, z) in variants:
            on = lambda c: f" --on {c}" if c is not None else ""
            out = vm.cmd(f"time yes{on(x)} | head -c {n}{on(y)} | wc -c{on(z)}", timeout=120)
            m = re.search(REAL, out)
            if m and str(n) in out:
                res.add(f"{name}: yes | head | wc", n / (num(m.group(1)) / 1e3) / 2**20, "MiB/s")
        for _ in range(3):
            m = re.search(REAL, vm.cmd("time echo x | cat | cat | wc -c"))
            if m:
                res.add("start and finish 4 stages", num(m.group(1)), "ms")
        if args.stats:
            print(vm.cmd("stats"))

    for r in range(args.runs):
        run_vm(args, args.cores, f"pipe-{args.cores}c-run{r}", body)
    for _ in range(args.runs):
        t = time.perf_counter()
        subprocess.run(["sh", "-c", f"yes | head -c {n} | wc -c"], check=True, capture_output=True)
        res.add("linux host: yes | head | wc", n / (time.perf_counter() - t) / 2**20, "MiB/s")
    res.show(f"pipes, {n >> 20} MiB through two pipes, {args.cores} cores, {args.runs} runs")


FSWRITE_SEQ = r"fswrite: cpu \d+ wrote \d+ MiB in \d+ ms: (\d+) MiB/s"
FSWRITE_SMALL = r"fswrite: \d+ small files: create and write (\d+) us each, remove (\d+) us each"


def bench_write(args):
    res = Results()

    def timed(vm, command):
        m = re.search(REAL, vm.cmd(command, timeout=120))
        return num(m.group(1)) if m else None

    def body(vm):
        vm.cmd("run fswrite", timeout=120)
        out = vm.cmd("run fswrite", timeout=120)
        m = re.search(FSWRITE_SEQ, out)
        if m:
            res.add("fswrite: sequential, 8 writes in flight", num(m.group(1)), "MiB/s")
        m = re.search(FSWRITE_SMALL, out)
        if m:
            res.add("fswrite: create + write a small file", num(m.group(1)), "us")
            res.add("fswrite: remove a small file", num(m.group(2)), "us")
        t = timed(vm, "time yes | head -c 50331648 > /data/w")
        if t:
            res.add("yes | head -c 48M > file", 48 / (t / 1e3), "MiB/s")
        vm.cmd("rm /data/w")
        t = timed(vm, "time cp /data/big /data/copy")
        if t:
            res.add("cp of a 32 MiB file", 32 / (t / 1e3), "MiB/s")
        if args.stats:
            print(vm.cmd("stats"))

    for r in range(args.runs):
        run_vm(args, args.cores, f"write-{args.cores}c-run{r}", body)
    res.show(f"file writes (thrown away after each run), {args.cores} cores, {args.runs} runs")


def newc(entries):
    """a cpio archive in the format the kernel unpacks as its initramfs"""
    out = bytearray()

    def add(name, mode, data=b""):
        name = name.encode() + b"\0"
        fields = [len(out) // 8 + 1, mode, 0, 0, 1, 0, len(data), 0, 0, 0, 0, len(name), 0]
        out.extend(b"070701" + b"".join(b"%08X" % f for f in fields) + name)
        out.extend(b"\0" * (-len(out) % 4) + data)
        out.extend(b"\0" * (-len(out) % 4))

    for name, mode, data in entries + [("TRAILER!!!", 0, b"")]:
        add(name, mode, data)
    return bytes(out)


LINUX_RESULT = r"linux: (.+?): ([\d.]+) ([^\n]+?)\s*$"


def bench_linuxvm(args):
    kernel = args.kernel or f"/usr/lib/modules/{os.uname().release}/vmlinuz"
    if not os.access(kernel, os.R_OK):
        raise SystemExit(f"cannot read {kernel}, pass --kernel with a bzImage")
    os.makedirs(OUT, exist_ok=True)
    src, exe = os.path.join(ROOT, "tools", "bench", "linuxvm", "bench.c"), os.path.join(OUT, "linuxvm-init")
    if not os.path.exists(exe) or os.path.getmtime(exe) < os.path.getmtime(src):
        subprocess.run(["gcc", "-O2", "-static", "-pthread", "-o", exe, src], check=True)
    initrd = os.path.join(OUT, "linuxvm.cpio")
    with open(exe, "rb") as f, open(initrd, "wb") as out:
        out.write(newc([("dev", 0o40755, b""), ("proc", 0o40755, b""), ("mnt", 0o40755, b""),
                        ("init", 0o100755, f.read())]))
    append = f"console=ttyS0 quiet panic=-1 bytes={args.bytes} {args.append}".strip()
    os.makedirs(args.logs, exist_ok=True)
    res = Results()
    version = ""
    for r in range(args.runs):
        cpus = args.cpus[:args.cores] if args.cpus else pick_cpus(args.cores)
        vm = Vm(cpus, args, os.path.join(args.logs, f"linuxvm-run{r}.log"), (kernel, initrd, append))
        try:
            end = time.time() + 900
            # the guest's own power-down message can land inside the last line,
            # so a guest that has exited counts as done too
            while "linux: done" not in vm.text and vm.proc.poll() is None:
                if time.time() > end or "Kernel panic" in vm.text:
                    raise RuntimeError("linux guest failed, see " + vm.log.name)
                vm.pump(0.2)
        finally:
            vm.close()
        m = re.search(r"linux: guest kernel (\S+) on (\d+) cpus", vm.text)
        version = m.group(0)[7:] if m else version
        for m in re.finditer(LINUX_RESULT, vm.text, re.M):
            if not m.group(1).startswith("guest kernel"):
                res.add(m.group(1), num(m.group(2)), m.group(3))
    res.show(f"linux in the same vm ({version}), {args.runs} runs, median (min - max)")


PING_IDLE = r"ping: \d+ calls after 2 ms idle cpu \d+ -> cpu \d+: min ([\d.]+) us, avg ([\d.]+) us"


def vcpu_seconds(vm):
    total = 0.0
    for tid in os.listdir(f"/proc/{vm.proc.pid}/task"):
        try:
            with open(f"/proc/{vm.proc.pid}/task/{tid}/stat") as f:
                fields = f.read().rsplit(")", 1)[1].split()
            if "CPU" in open(f"/proc/{vm.proc.pid}/task/{tid}/comm").read():
                total += (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
        except OSError:
            pass
    return total


def energy_j():
    try:
        with open("/sys/class/powercap/intel-rapl:0/energy_uj") as f:
            return int(f.read()) / 1e6
    except OSError:
        return None


def bench_idle(args):
    res = Results()
    modes = ("never", "adaptive", "always")

    def body(vm):
        pm = "mwait" if args.cpu_pm else "hlt"
        m = re.search(r"rtt doorbell\s+min\s+\d+ ns\s+median\s+(\d+) ns", vm.text)
        if m:
            res.add(f"{pm}: doorbell rtt at boot (adaptive)", num(m.group(1)), "ns")
        vm.cmd("spawn echod --on 2")
        for mode in modes:
            vm.cmd(f"poll all {mode}")
            out = vm.cmd("run ping --on 1")
            m, i = re.search(PING, out), re.search(PING_IDLE, out)
            if m:
                res.add(f"{pm}, {mode}: rpc back to back, avg", num(m.group(4)), "us")
            if i:
                res.add(f"{pm}, {mode}: rpc after 2 ms idle, avg", num(i.group(2)), "us")
            cpu0, e0, t0 = vcpu_seconds(vm), energy_j(), time.time()
            vm.pump(3)
            dt = time.time() - t0
            res.add(f"{pm}, {mode}: host cpu of idle vcpus", (vcpu_seconds(vm) - cpu0) / dt * 100, "% of 1 cpu")
            e1 = energy_j()
            if e0 is not None and e1 is not None:
                res.add(f"{pm}, {mode}: package power while idle", (e1 - e0) / dt, "W")

    for r in range(args.runs):
        run_vm(args, args.cores, f"idle-{args.cores}c-run{r}", body)
    note = "" if energy_j() is not None else " (no power: /sys/class/powercap/intel-rapl:0/energy_uj needs read access)"
    res.show(f"waking idle cores, {args.cores} cores, {args.runs} runs{note}")


YIELD_ALONE = r"yielder: cpu \d+ did \d+ yields alone in \d+ us: (\d+) ns each"
YIELD_PAIR = r"yielder: cpu \d+ switched \d+ times with its partner in \d+ us: (\d+) ns per switch"


LOAD = r"flops: cpu (\d+) load (\w+): ([\d.]+)"


def flops_load(vm, what, places):
    """starts one sustained flops job per place (a cpu, or None for the
    director's choice) and returns the total and the cpus they ran on"""
    before = len(vm.text)
    for c in places:
        vm.cmd(f"flops load {what}" + (f" --on {c}" if c is not None else "") + " &")
    end = time.time() + 30
    found = []
    while time.time() < end:
        found = [m for m in re.finditer(LOAD, vm.text[before:]) if m.group(2) == what]
        if len(found) >= len(places):
            break
        vm.pump(0.2)
    return sum(num(m.group(3)) for m in found), [int(m.group(1)) for m in found]


def bench_smt(args):
    """the same physical cores with one kernel per hardware thread, then with
    one kernel per core and the siblings left out"""
    phys = [c for group in host_cores() for c in group][:args.cores]
    every = sorted({s for c in phys for s in siblings(c)})
    if len(every) == len(phys):
        raise SystemExit("the host has no SMT siblings to compare")
    res = Results()
    setups = [("thread", every), ("core", phys)]

    def body(vm, kind, n):
        parse = Results()
        parse_ring(vm.text, parse)
        for name, vals in parse.values.items():
            res.add(f"{kind}: {name}", vals[0], parse.units[name])
        total, _ = flops_load(vm, "matmul", range(n))
        res.add(f"{kind}: dgemm on every kernel, total", total, "GFLOP/s")
        total, _ = flops_load(vm, "triad", range(n))
        res.add(f"{kind}: triad on every kernel, total", total, "GB/s")
        if kind == "thread":
            total, on = flops_load(vm, "matmul", [None] * args.cores)
            res.add(f"{kind}: dgemm x{args.cores} placed by the director", total, "GFLOP/s")
            # vcpus are numbered thread by thread, so siblings are next to each other
            res.add(f"{kind}: ... physical cores they got", len({c // (n // args.cores) for c in on}), f"of {args.cores}")
        before = len(vm.text)
        for _ in range(4):
            vm.cmd("spawn kv")
        pids = [started_on(vm.cmd("spawn kvbench"))[0] for _ in range(4)]
        for pid in pids:
            vm.cmd(f"wait {pid}", timeout=120)
        rates = [num(m.group(1)) for m in re.finditer(KVBENCH, vm.text[before:])]
        res.add(f"{kind}: kv 4 shards + 4 clients, total", sum(rates) / 1000, "M ops/s")

    saved = args.cpus
    for r in range(args.runs):
        for kind, cpus in setups:
            args.cpus = cpus
            run_vm(args, len(cpus), f"smt-{kind}-run{r}", lambda vm: body(vm, kind, len(cpus)))
    args.cpus = saved
    res.show(f"kernel per hardware thread ({len(every)}) vs per core ({len(phys)}), "
             f"same {args.cores} physical cores, {args.runs} runs")


def bench_switch(args):
    res = Results()

    def body(vm):
        core = min(2, args.cores - 1)
        m = re.search(YIELD_ALONE, vm.cmd(f"run yielder --on {core}"))
        if m:
            res.add("yield with nothing else to run", num(m.group(1)), "ns")
        vm.cmd(f"yielder pair --on {core} &")
        out = vm.cmd(f"yielder pair --on {core}")
        for m in re.finditer(YIELD_PAIR, out + vm.text[-2000:]):
            res.add("switch between two jobs (yield)", num(m.group(1)), "ns")
            break
        vm.cmd(f"spawn echod --on {core}")
        m = re.search(PING, vm.cmd(f"run ping --on {core}"))
        if m:
            res.add("request and reply on one core (block and wake)", num(m.group(4)) * 1000, "ns")

    for r in range(args.runs):
        run_vm(args, args.cores, f"switch-{args.cores}c-run{r}", body)
    res.show(f"context switches on one core, {args.cores} cores, {args.runs} runs")


SEL4_DONE = "All is well in the universe"
SEL4_JSON = r"JSON OUTPUT\n(.*?)END JSON OUTPUT"


def bench_sel4(args):
    images = os.path.join(args.sel4, "build_x86", "images")
    kernel, app = (os.path.join(images, f) for f in ("kernel-x86_64-pc99", "sel4benchapp-image-x86_64-pc99"))
    if not os.path.exists(app):
        raise SystemExit(f"no seL4 build in {images}, run tools/bench/sel4/setup.sh first")
    hz = float(subprocess.run([build_host_tool("tsc")], capture_output=True, text=True, check=True).stdout)
    us = lambda cycles: cycles / hz * 1e6
    if args.mem == "512M":
        args.mem = "1G"
    os.makedirs(args.logs, exist_ok=True)
    res = Results()
    for r in range(args.runs):
        cpus = args.cpus[:args.cores] if args.cpus else pick_cpus(args.cores)
        vm = Vm(cpus, args, os.path.join(args.logs, f"sel4-run{r}.log"), (kernel, app, ""))
        try:
            end = time.time() + 600
            while SEL4_DONE not in vm.text:
                if time.time() > end or "Benchmark failed" in vm.text:
                    raise RuntimeError("seL4 run failed, see " + vm.log.name)
                vm.pump(0.2)
            vm.pump(0.5)
        finally:
            vm.close()
        m = re.search(SEL4_JSON, vm.text, re.S)
        if not m:
            raise RuntimeError("no results from seL4, see " + vm.log.name)
        for bench in json.loads(m.group(1)):
            rows = bench["Results"]
            if bench["Benchmark"] == "One way IPC microbenchmarks":
                def one_way(fn, direction):
                    return next(x for x in rows if x["Function"] == fn and x["Direction"] == direction
                                and not x["Same vspace?"] and x["IPC length"] == 0)
                call, reply = one_way("seL4_Call", "client->server"), one_way("seL4_ReplyRecv", "server->client")
                res.add("IPC suite: Call + ReplyRecv, same core", us(call["Median"] + reply["Median"]), "us")
            elif bench["Benchmark"] == "Cross core request and reply":
                for x in rows:
                    res.add(f"{x['Test']}: median", us(x["Median"]), "us")
                    res.add(f"{x['Test']}: min", us(x["Min"]), "us")
    res.show(f"seL4 (sel4bench + xcore.patch) in the same vm, {args.cores} cores, {args.runs} runs, "
             f"tsc {hz / 1e9:.3f} GHz")


def build_host_tool(name):
    os.makedirs(OUT, exist_ok=True)
    src = os.path.join(ROOT, "tools", "bench", "host", f"{name}.c")
    exe = os.path.join(OUT, name)
    if not os.path.exists(exe) or os.path.getmtime(exe) < os.path.getmtime(src):
        subprocess.run(["gcc", "-O2", "-pthread", "-o", exe, src], check=True)
    return exe


def run_host(exe, cpus):
    out = subprocess.run([exe, *map(str, cpus)], capture_output=True, text=True, check=True).stdout
    return {k: float(v) for k, v in (line.split() for line in out.splitlines())}


def bench_host(args):
    pair, all2all = build_host_tool("pair"), build_host_tool("all2all")
    clusters = host_cores()
    pairs = [("same L3", clusters[0][:2])]
    if len(clusters) > 1:
        pairs.append(("other L3", [clusters[0][0], clusters[1][0]]))
    res = Results()
    for name, cpus in pairs:
        if len(cpus) < 2:
            continue
        for _ in range(args.runs):
            v = run_host(pair, cpus)
            res.add(f"{name}: cache line rtt median", v["line_rtt_median_ns"], "ns")
            res.add(f"{name}: slot ring stream", v["stream_msgs_per_s"] / 1e6, "M msgs/s")
            res.add(f"{name}: futex rtt median", v["futex_rtt_median_ns"] / 1000, "us")
            res.add(f"{name}: pipe rtt median", v["pipe_rtt_median_ns"] / 1000, "us")
    for n in args.counts or [2, 4, 8]:
        cpus = pick_cpus(n)
        for _ in range(args.runs):
            v = run_host(all2all, cpus)
            res.add(f"all-to-all {n} threads", v["all2all_msgs_per_s"] / 1e6, "M msgs/s")
    res.show(f"linux baseline on this machine, {args.runs} runs")


def bench_shell(args):
    def body(vm):
        print(vm.text, end="")
        for line in args.commands:
            print(vm.cmd(line), end="")
        print()

    run_vm(args, args.cores, "shell", body)



>>>>>>> Stashed changes
def main():
    p = argparse.ArgumentParser(
        prog="bench.sh",
        description="Boots sydonOS under QEMU with every virtual core pinned to its own physical core, "
<<<<<<< Updated upstream
                    "runs a benchmark through the shell several times and prints median (min - max).",
=======
                    "runs a benchmark several times and prints median (min - max).",
>>>>>>> Stashed changes
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""benchmarks:
  ring       the boot-time ring test: round trips, doorbell, streaming, all-to-all
  scaling    the ring test on several core counts (--counts 2 4 8)
<<<<<<< Updated upstream
  rpc        ping -> echo round trips: same core and every other core
  kv         sharded key-value store (--shards 1 4 --clients 1 4)
=======
  rpc        ping -> echo round trips, placed by the director, same core, other L3
  kv         sharded key-value store (--shards 1 4 --clients 1 4)
  io         file reads through the storage core: placed, on the owner, remote, and with both busy
  write      file writes: streaming, cp, many small creates and removes
  pipe       yes | head -c N | wc -c with different placements, and pipeline start up (--bytes)
  linuxvm    the same benchmarks on linux inside the same vm (--kernel, --append)
  idle       waking idle cores and what idling costs, per poll policy (--cpu-pm for mwait)
  smt        one kernel per hardware thread vs per core on the same cores (--cores = physical cores)
  switch     context switch costs: yield alone, two jobs yielding to each other, block and wake
  sel4       seL4 IPC on the same core and across cores, in the same vm (needs tools/bench/sel4/setup.sh)
  host       the same patterns as plain linux threads, for comparison
  shell      boot and run your own shell commands, e.g. shell -- "spawn echod" "run ping"
>>>>>>> Stashed changes

examples:
  ./bench.sh ring --cores 4
  ./bench.sh scaling --counts 2 4 6 8 --runs 3
<<<<<<< Updated upstream
  ./bench.sh rpc --cores 8
  ./bench.sh kv --shards 1 4 --clients 4""")

    p.add_argument("bench", choices=["ring", "scaling", "rpc", "kv"])
=======
  ./bench.sh kv --cores 8 --shards 1 4 --clients 4
  ./bench.sh host""")
    p.add_argument("bench", choices=["ring", "scaling", "rpc", "kv", "io", "write", "pipe", "linuxvm", "sel4", "switch", "idle", "smt", "host", "shell"])
>>>>>>> Stashed changes
    p.add_argument("--cores", type=int, default=4, help="virtual cores (default 4)")
    p.add_argument("--counts", type=int, nargs="+", help="core counts for scaling and host all-to-all")
    p.add_argument("--runs", type=int, default=3, help="repeat each measurement (default 3)")
    p.add_argument("--cpus", type=lambda s: [int(c) for c in s.split(",")], help="host cpus to pin to, e.g. 0,2,4,6")
    p.add_argument("--shards", type=int, nargs="+", default=[1, 4])
    p.add_argument("--clients", type=int, nargs="+", default=[1, 4])
<<<<<<< Updated upstream
=======
    p.add_argument("--bytes", type=int, default=1 << 30, help="bytes through the pipes (default 1 GiB)")
>>>>>>> Stashed changes
    p.add_argument("--mem", default="512M")
    p.add_argument("--kernel", help="bzImage for linuxvm (default: the host's kernel)")
    p.add_argument("--sel4", default=os.path.join(os.path.dirname(ROOT), "sel4bench"),
                   help="where tools/bench/sel4/setup.sh built seL4 (default: ../sel4bench)")
    p.add_argument("--append", default="", help="extra kernel command line for linuxvm, e.g. mitigations=off")
    p.add_argument("--fs", help="data disk image to write to for real; by default writes are thrown away")
    p.add_argument("--disk-opts", default="", help="extra -drive options for the data disk, e.g. ,aio=io_uring")
    p.add_argument("--iothread", action="store_true", help="give the data disk its own qemu io thread")
    p.add_argument("--no-kvm", dest="kvm", action="store_false", help="use TCG emulation")
    p.add_argument("--no-pin", dest="pin", action="store_false", help="let the host schedule the vcpus")
    p.add_argument("--cpu-pm", action="store_true",
                   help="give the guest mwait and let hlt/mwait run without exits (-overcommit cpu-pm=on)")
    p.add_argument("--stats", action="store_true", help="print per-core counters after rpc and kv runs")
    p.add_argument("--build", action="store_true", help="run ./build.sh first")
    p.add_argument("commands", nargs="*", help="shell commands for the shell benchmark")
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
    if args.bench not in ("host", "linuxvm", "sel4") and not os.path.exists(os.path.join(ROOT, "out", "disk.img")):
        raise SystemExit("out/disk.img missing, run ./build.sh or pass --build")
    args.logs = os.path.join(OUT, time.strftime("%Y%m%d-%H%M%S") + f"-{args.bench}")
    args.disk_device = ["-device", "virtio-blk-pci,drive=fs,disable-legacy=on"]
    if args.iothread:
        args.disk_device = ["-object", "iothread,id=io0", "-device", "virtio-blk-pci,drive=fs,disable-legacy=on,iothread=io0"]

<<<<<<< Updated upstream
    {"ring": bench_ring, "scaling": bench_scaling, "rpc": bench_rpc, "kv": bench_kv}[args.bench](args)
    print(f"\nlogs: {os.path.relpath(args.logs, ROOT)}")
=======
    {"ring": bench_ring, "scaling": bench_scaling, "rpc": bench_rpc, "kv": bench_kv,
     "io": bench_io, "pipe": bench_pipe, "write": bench_write, "linuxvm": bench_linuxvm, "sel4": bench_sel4, "switch": bench_switch, "idle": bench_idle, "smt": bench_smt, "host": bench_host, "shell": bench_shell}[args.bench](args)
    if args.bench != "host":
        print(f"\nlogs: {os.path.relpath(args.logs, ROOT)}")
>>>>>>> Stashed changes


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
