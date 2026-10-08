"""Evict corpus files from the page cache and report their resident bytes with mincore."""

import ctypes
import os
import sys


def _files(root):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [
            d for d in dirnames if not os.path.islink(os.path.join(dirpath, d))
        ]
        for name in filenames:
            path = os.path.join(dirpath, name)
            if os.path.islink(path):
                continue
            try:
                st = os.stat(path)
            except OSError:
                continue
            if (st.st_mode & 0o170000) != 0o100000:
                continue
            if st.st_size == 0:
                continue
            yield path


def _lib():
    libc = ctypes.CDLL(None, use_errno=True)
    libc.mmap.restype = ctypes.c_void_p
    libc.mmap.argtypes = [
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_longlong,
    ]
    libc.mincore.restype = ctypes.c_int
    libc.mincore.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_void_p]
    libc.munmap.restype = ctypes.c_int
    libc.munmap.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
    return libc


def resident_bytes(root):
    page = os.sysconf("SC_PAGE_SIZE")
    libc = _lib()
    failed = ctypes.c_void_p(-1).value
    total = 0
    for path in _files(root):
        fd = os.open(path, os.O_RDONLY)
        try:
            size = os.fstat(fd).st_size
            if size == 0:
                continue
            pages = (size + page - 1) // page
            length = pages * page
            addr = libc.mmap(None, length, 1, 1, fd, 0)
            if addr == failed:
                err = ctypes.get_errno()
                raise OSError(err, os.strerror(err), path)
            try:
                vec = (ctypes.c_ubyte * pages)()
                if libc.mincore(addr, length, ctypes.addressof(vec)) != 0:
                    err = ctypes.get_errno()
                    raise OSError(err, os.strerror(err), path)
                for i in range(pages):
                    if vec[i] & 1:
                        total += page
            finally:
                libc.munmap(addr, length)
        finally:
            os.close(fd)
    return total


def evict(root):
    os.sync()
    for path in _files(root):
        fd = os.open(path, os.O_RDONLY)
        try:
            os.posix_fadvise(fd, 0, 0, 4)
        finally:
            os.close(fd)


def main(argv):
    if len(argv) != 3 or argv[1] not in ("evict", "resident"):
        print("usage: residency.py {evict|resident} DIR", file=sys.stderr)
        return 2
    if argv[1] == "evict":
        evict(argv[2])
    else:
        print(resident_bytes(argv[2]))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
