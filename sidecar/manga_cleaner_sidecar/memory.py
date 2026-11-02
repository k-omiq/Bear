"""Memory measurement and instrumentation for Manga Cleaner sidecar.

**Which quantity is the honest one.** This module reports two different
measures of "memory used", because on macOS they differ by more than a factor
of two for this workload and only one of them is the number
rule 9 is written against.

*Resident set size* - `getrusage(RUSAGE_SELF).ru_maxrss`, and
`MACH_TASK_BASIC_INFO.resident_size` - counts pages mapped into the task's own
address space. It does **not** count the unified-memory buffers MLX allocates
through Metal, which the kernel accounts to the process under the
`IOAccelerator` tag, nor pages the process owns that are not currently mapped.
For a FLUX render essentially the whole model lives in exactly those buffers,
so RSS misses most of the cost. Worse, it is not merely low but *insensitive*:
measured on an Apple M5, a 512² render and a 1024² render both read about
2.9 GB of RSS while their true costs were 6.58 GB and 10.29 GB.

*Physical footprint* - `TASK_VM_INFO.phys_footprint`, and its kernel-maintained
high-water mark `ledger_phys_footprint_peak` - is the figure macOS itself uses
for a process's memory: it includes IOAccelerator/Metal allocations, owned
unmapped pages, and the compressor. It is the number `/usr/bin/footprint`
breaks down, the number `vmmap` prints as `Physical footprint`, the number
`/usr/bin/time -l` reports as `peak memory footprint`, and the number a user's
Activity Monitor shows. Rule 9's subject is the user's machine, so this is the
measure the wire protocol's `peak_rss_bytes` carries and the parent's cap is
checked against.

Both are reported. `instrument` names which quantity the primary fields hold so
a reader never has to guess, and the resident readings are kept alongside under
their own names so the disagreement stays visible rather than being averaged
away.
"""

from __future__ import annotations

import ctypes
import os
import sys
from typing import Optional

try:
    import resource
except ImportError:
    # POSIX only: Python on Windows has no such module.
    resource = None

from .protocol import MemoryReport

# Flavours from <mach/task_info.h>.
MACH_TASK_BASIC_INFO = 20
TASK_VM_INFO = 22


class _ProcessMemoryCounters(ctypes.Structure):
    """PROCESS_MEMORY_COUNTERS from <psapi.h>."""

    _fields_ = [
        ("cb", ctypes.c_uint32),
        ("PageFaultCount", ctypes.c_uint32),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
    ]


def _windows_memory_counters() -> Optional[_ProcessMemoryCounters]:
    """This process's working set from `K32GetProcessMemoryInfo`, or None off Windows."""
    if sys.platform != "win32":
        return None
    try:
        kernel32 = ctypes.WinDLL("kernel32")
        kernel32.GetCurrentProcess.restype = ctypes.c_void_p
        kernel32.K32GetProcessMemoryInfo.restype = ctypes.c_int
        kernel32.K32GetProcessMemoryInfo.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(_ProcessMemoryCounters),
            ctypes.c_uint32,
        ]
        counters = _ProcessMemoryCounters()
        counters.cb = ctypes.sizeof(counters)
        if kernel32.K32GetProcessMemoryInfo(
            kernel32.GetCurrentProcess(), ctypes.byref(counters), counters.cb
        ):
            return counters
    except (AttributeError, OSError):
        pass
    return None


class _MachTaskBasicInfo(ctypes.Structure):
    """struct mach_task_basic_info from <mach/task_info.h>."""

    _fields_ = [
        ("virtual_size", ctypes.c_uint64),
        ("resident_size", ctypes.c_uint64),
        ("resident_size_max", ctypes.c_uint64),
        ("user_time", ctypes.c_uint64),
        ("system_time", ctypes.c_uint64),
        ("policy", ctypes.c_int32),
        ("suspend_count", ctypes.c_int32),
    ]


class _TaskVMInfo(ctypes.Structure):
    """struct task_vm_info from <mach/task_info.h>, declared through rev3.

    Only the fields up to `ledger_phys_footprint_peak` are read, but the whole
    revision is declared so `task_info` is handed a count the kernel recognises.
    """

    _fields_ = [
        ("virtual_size", ctypes.c_uint64),
        ("region_count", ctypes.c_int32),
        ("page_size", ctypes.c_int32),
        ("resident_size", ctypes.c_uint64),
        ("resident_size_peak", ctypes.c_uint64),
        ("device", ctypes.c_uint64),
        ("device_peak", ctypes.c_uint64),
        ("internal", ctypes.c_uint64),
        ("internal_peak", ctypes.c_uint64),
        ("external", ctypes.c_uint64),
        ("external_peak", ctypes.c_uint64),
        ("reusable", ctypes.c_uint64),
        ("reusable_peak", ctypes.c_uint64),
        ("purgeable_volatile_pmap", ctypes.c_uint64),
        ("purgeable_volatile_resident", ctypes.c_uint64),
        ("purgeable_volatile_virtual", ctypes.c_uint64),
        ("compressed", ctypes.c_uint64),
        ("compressed_peak", ctypes.c_uint64),
        ("compressed_lifetime", ctypes.c_uint64),
        # rev1
        ("phys_footprint", ctypes.c_uint64),
        # rev2
        ("min_address", ctypes.c_uint64),
        ("max_address", ctypes.c_uint64),
        # rev3
        ("ledger_phys_footprint_peak", ctypes.c_int64),
        ("ledger_purgeable_nonvolatile", ctypes.c_int64),
        ("ledger_purgeable_novolatile_compressed", ctypes.c_int64),
        ("ledger_purgeable_volatile", ctypes.c_int64),
        ("ledger_purgeable_volatile_compressed", ctypes.c_int64),
        ("ledger_tag_network_nonvolatile", ctypes.c_int64),
        ("ledger_tag_network_nonvolatile_compressed", ctypes.c_int64),
        ("ledger_tag_network_volatile", ctypes.c_int64),
        ("ledger_tag_network_volatile_compressed", ctypes.c_int64),
        ("ledger_tag_media_footprint", ctypes.c_int64),
        ("ledger_tag_media_footprint_compressed", ctypes.c_int64),
        ("ledger_tag_media_nofootprint", ctypes.c_int64),
        ("ledger_tag_media_nofootprint_compressed", ctypes.c_int64),
        ("ledger_tag_graphics_footprint", ctypes.c_int64),
        ("ledger_tag_graphics_footprint_compressed", ctypes.c_int64),
        ("ledger_tag_graphics_nofootprint", ctypes.c_int64),
        ("ledger_tag_graphics_nofootprint_compressed", ctypes.c_int64),
        ("ledger_tag_neural_footprint", ctypes.c_int64),
        ("ledger_tag_neural_footprint_compressed", ctypes.c_int64),
        ("ledger_tag_neural_nofootprint", ctypes.c_int64),
        ("ledger_tag_neural_nofootprint_compressed", ctypes.c_int64),
    ]


_libc = None


def _get_libc():
    """Bind libSystem once, with argument types declared.

    `task_info` takes a `mach_port_t` and writes through two pointers; leaving
    ctypes to guess the signature truncates the port on some builds.
    """
    global _libc
    if _libc is None and sys.platform == "darwin":
        lib = ctypes.CDLL(None)
        lib.mach_task_self.restype = ctypes.c_uint32
        lib.task_info.restype = ctypes.c_int
        lib.task_info.argtypes = [
            ctypes.c_uint32,
            ctypes.c_int,
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_uint32),
        ]
        _libc = lib
    return _libc


def _task_info(flavor: int, struct_type):
    """Call Mach `task_info` on this task, or return None if it cannot be had."""
    if sys.platform != "darwin":
        return None
    try:
        lib = _get_libc()
        if lib is None:
            return None
        info = struct_type()
        count = ctypes.c_uint32(ctypes.sizeof(info) // 4)
        ret = lib.task_info(
            lib.mach_task_self(), flavor, ctypes.byref(info), ctypes.byref(count)
        )
        if ret == 0:
            return info
    except Exception:
        pass
    return None


def get_phys_footprint_bytes() -> Optional[int]:
    """Physical footprint **now**, in bytes - the honest current cost.

    This is what macOS calls the process's memory: resident pages plus
    IOAccelerator/Metal unified-memory allocations plus owned-but-unmapped
    pages plus the compressor's share. `None` off Darwin.
    """
    info = _task_info(TASK_VM_INFO, _TaskVMInfo)
    return int(info.phys_footprint) if info is not None else None


def get_peak_phys_footprint_bytes() -> Optional[int]:
    """Peak physical footprint since the process started, in bytes.

    Read from the kernel's own ledger high-water mark, so it needs no polling
    and cannot miss a spike that occurred between two samples. It is the same
    quantity `/usr/bin/time -l` prints as `peak memory footprint`.
    """
    info = _task_info(TASK_VM_INFO, _TaskVMInfo)
    if info is None:
        return None
    peak = int(info.ledger_phys_footprint_peak)
    # The ledger is signed and is zero on kernels that do not maintain it;
    # fall back to the current reading rather than reporting a peak below it.
    current = int(info.phys_footprint)
    return max(peak, current) if peak > 0 else current


def get_peak_rss_bytes() -> int:
    """Peak **resident set size** in bytes.

    Understates this workload badly - see the module docstring - and is kept
    only so the disagreement between instruments stays on the wire. On macOS
    (Darwin), getrusage reports ru_maxrss directly in bytes. On Linux systems,
    getrusage reports ru_maxrss in kilobytes. Windows has no getrusage, and its
    peak working set is the same measure.
    """
    if resource is None:
        counters = _windows_memory_counters()
        return int(counters.PeakWorkingSetSize) if counters is not None else 0
    usage = resource.getrusage(resource.RUSAGE_SELF)
    if sys.platform == "darwin":
        return int(usage.ru_maxrss)
    else:
        return int(usage.ru_maxrss * 1024)


def get_current_rss_bytes() -> Optional[int]:
    """Current **resident set size** in bytes, from Mach `task_info`.

    Uses Darwin Mach kernel task_info via ctypes on macOS to query the kernel
    directly without requiring third-party extensions like psutil. Understates
    unified-memory allocations; prefer `get_phys_footprint_bytes`. On Windows it
    is the current working set.
    """
    counters = _windows_memory_counters()
    if counters is not None:
        return int(counters.WorkingSetSize)
    info = _task_info(MACH_TASK_BASIC_INFO, _MachTaskBasicInfo)
    return int(info.resident_size) if info is not None else None


def get_mlx_cache_bytes() -> Optional[int]:
    """Query MLX for its current cached buffer allocation in bytes."""
    try:
        import mlx.core as mx  # type: ignore

        if hasattr(mx, "get_cache_memory"):
            return int(mx.get_cache_memory())
        if hasattr(mx, "get_active_memory"):
            return int(mx.get_active_memory())
    except Exception:
        pass
    return 0


def get_mlx_peak_bytes() -> Optional[int]:
    """MLX's own allocation high-water mark, in bytes, where MLX can say.

    A third instrument again, and it agrees with neither of the others: it
    counts only what MLX asked for, so it misses the Metal driver's overhead
    and the Python heap, and it can exceed the physical footprint because it
    charges buffers that were recycled out of the cache rather than held.
    """
    try:
        import mlx.core as mx  # type: ignore

        return int(mx.get_peak_memory())
    except Exception:
        return None


def get_gpu_bytes() -> Optional[int]:
    """What torch holds on a discrete GPU now, in bytes, or None with none in use.

    Metal is left out: its buffers are unified memory, already in the footprint.
    Read from the modules already loaded, so a health probe never loads torch.
    """
    torch = sys.modules.get("torch")
    if torch is None:
        return None
    try:
        if torch.cuda.is_initialized():
            return int(torch.cuda.memory_reserved())
        xpu = getattr(torch, "xpu", None)
        if xpu is not None and xpu.is_initialized():
            return int(xpu.memory_reserved())
    except Exception:
        pass
    return None


def build_memory_report() -> MemoryReport:
    """Build a complete MemoryReport for inclusion in wire responses.

    `peak_rss_bytes` and `rss_bytes` carry the **physical footprint**, because
    those are the fields the parent checks its budget and its floor against and
    the budget is stated against the user's machine. `instrument` names the
    quantity, and the resident readings ride alongside under their own names.
    """
    footprint = get_phys_footprint_bytes()
    peak_footprint = get_peak_phys_footprint_bytes()
    resident = get_current_rss_bytes()
    peak_resident = get_peak_rss_bytes()

    if peak_footprint is not None:
        instrument = "phys_footprint"
        primary_peak = peak_footprint
        primary_now = footprint if footprint is not None else resident
    else:
        # No Mach ledger to read - say so rather than passing resident off as
        # the footprint, so a consumer can tell a real figure from a fallback.
        instrument = "ru_maxrss"
        primary_peak = peak_resident
        primary_now = resident

    return MemoryReport(
        peak_rss_bytes=primary_peak,
        rss_bytes=primary_now,
        cache_bytes=get_mlx_cache_bytes(),
        instrument=instrument,
        footprint_bytes=footprint,
        peak_footprint_bytes=peak_footprint,
        resident_bytes=resident,
        peak_resident_bytes=peak_resident,
        backend_peak_bytes=get_mlx_peak_bytes(),
        gpu_bytes=get_gpu_bytes(),
    )


def calculate_dir_size_bytes(path: str) -> Optional[int]:
    """Calculate the total size of all files on disk within a directory tree."""
    if not os.path.exists(path):
        return None
    if os.path.isfile(path):
        return os.path.getsize(path)

    total_bytes = 0
    for root, _, files in os.walk(path):
        for f in files:
            fp = os.path.join(root, f)
            if not os.path.islink(fp):
                try:
                    total_bytes += os.path.getsize(fp)
                except OSError:
                    pass

    return total_bytes if total_bytes > 0 else None
