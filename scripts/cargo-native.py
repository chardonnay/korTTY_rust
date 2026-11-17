#!/usr/bin/env python3
"""Run Cargo with a job count bounded by available CPUs and memory."""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys


MIB = 1024 * 1024
RESERVE_MIB = {"darwin": 4096, "linux": 3072, "win32": 3072}


def logical_cpus():
    if hasattr(os, "process_cpu_count"):
        return os.process_cpu_count() or 1
    if hasattr(os, "sched_getaffinity"):
        return len(os.sched_getaffinity(0)) or 1
    return os.cpu_count() or 1


def read_limit(group, name):
    try:
        return (group / name).read_text(encoding="ascii").strip()
    except FileNotFoundError:
        return "max"


def linux_resources(cpus):
    memory = Path("/proc/meminfo").read_text(encoding="ascii")
    available = int(re.search(r"^MemAvailable:\s+(\d+) kB$", memory, re.MULTILINE)[1]) * 1024
    # An ancestor may impose a lower quota than the process's immediate group.
    mount = Path("/sys/fs/cgroup")
    for record in Path("/proc/self/cgroup").read_text(encoding="ascii").splitlines():
        if not record.startswith("0::/"):
            continue
        relative = Path(record[4:])
        if relative.is_absolute() or ".." in relative.parts:
            break
        group = mount / relative
        while True:
            maximum = read_limit(group, "memory.max")
            current = read_limit(group, "memory.current")
            if maximum != "max" and current != "max":
                available = min(available, max(0, int(maximum) - int(current)))
            capacity = read_limit(group, "cpu.max").split()
            if len(capacity) == 2 and capacity[0] != "max":
                quota, period = map(int, capacity)
                cpus = min(cpus, max(1, (quota + period - 1) // period))
            if group == mount:
                break
            group = group.parent
        break
    return cpus, available


def mac_resources(cpus):
    total = int(subprocess.check_output(["/usr/sbin/sysctl", "-n", "hw.memsize"], text=True))
    pressure = subprocess.check_output(["/usr/bin/memory_pressure", "-Q"], text=True)
    percent = int(re.search(r"System-wide memory free percentage:\s*(\d+)%", pressure)[1])
    return cpus, total * percent // 100


def windows_resources(cpus):
    import ctypes

    class MemoryStatus(ctypes.Structure):
        _fields_ = [
            ("length", ctypes.c_uint32),
            ("load", ctypes.c_uint32),
            ("total_physical", ctypes.c_uint64),
            ("available_physical", ctypes.c_uint64),
            ("total_page", ctypes.c_uint64),
            ("available_page", ctypes.c_uint64),
            ("total_virtual", ctypes.c_uint64),
            ("available_virtual", ctypes.c_uint64),
            ("available_extended", ctypes.c_uint64),
        ]

    memory = MemoryStatus()
    memory.length = ctypes.sizeof(memory)
    query = ctypes.WinDLL("kernel32", use_last_error=True).GlobalMemoryStatusEx
    query.argtypes = [ctypes.POINTER(MemoryStatus)]
    query.restype = ctypes.c_int
    if not query(ctypes.byref(memory)):
        raise ctypes.WinError(ctypes.get_last_error())
    return cpus, memory.available_physical


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--print-jobs", action="store_true", help="print the calculated job count")
    parser.add_argument("--memory-per-job-mib", type=int, default=1024)
    parser.add_argument("cargo_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.cargo_args[:1] == ["--"]:
        args.cargo_args = args.cargo_args[1:]
    if args.memory_per_job_mib < 1:
        parser.error("--memory-per-job-mib must be positive")
    if not args.print_jobs and not args.cargo_args:
        parser.error("provide a Cargo subcommand or --print-jobs")

    platform = sys.platform
    probe = {"darwin": mac_resources, "linux": linux_resources, "win32": windows_resources}.get(platform)
    if probe is None:
        parser.error(f"unsupported host platform: {platform}")
    try:
        cpus, available = probe(logical_cpus())
    except (OSError, ValueError, TypeError, subprocess.SubprocessError) as error:
        print(f"Cannot measure available build resources: {error}. Use cargo --jobs with a known limit.", file=sys.stderr)
        return 2
    available_mib = available // MIB
    reserve = RESERVE_MIB[platform]
    jobs = min(cpus, max(1, (available_mib - reserve) // args.memory_per_job_mib))
    print(f"Cargo build budget: {jobs} jobs of {cpus} CPUs, {available_mib} MiB available, {reserve} MiB reserved.", file=sys.stderr)
    if args.print_jobs:
        print(jobs)
        return 0
    environment = os.environ.copy()
    environment.setdefault("CARGO_BUILD_JOBS", str(jobs))
    if environment["CARGO_BUILD_JOBS"] != str(jobs):
        print(f"Using the explicit CARGO_BUILD_JOBS={environment['CARGO_BUILD_JOBS']} override.", file=sys.stderr)
    return subprocess.call(["cargo", *args.cargo_args], env=environment)


if __name__ == "__main__":
    sys.exit(main())
