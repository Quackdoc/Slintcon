#!/usr/bin/env python3
"""Recover a VT stuck in KD_GRAPHICS so chvt works again.

Linux ignores VT switches from a KD_GRAPHICS + VT_AUTO console. This restores
KD_TEXT / VT_AUTO on the stuck tty, then activates another VT.

    sudo python3 unlock-vt.py           # unlock current VT, switch to tty1
    sudo python3 unlock-vt.py --to 2    # switch to Cosmic's tty (see loginctl)
    sudo python3 unlock-vt.py --from 3 --to 1
"""

import argparse
import fcntl
import os
import struct
import sys

KDSETMODE = 0x4B3A
KD_TEXT = 0
VT_SETMODE = 0x5602
VT_RELDISP = 0x5605
VT_AUTO = 0
VT_ACTIVATE = 0x5606
VT_WAITACTIVE = 0x5607


def active_vt() -> int:
    with open("/sys/class/tty/tty0/active") as f:
        return int(f.read().strip().removeprefix("tty"))


def open_tty(path: str) -> int:
    return os.open(path, os.O_RDWR | os.O_NOCTTY)


def unlock(vt: int) -> None:
    fd = open_tty(f"/dev/tty{vt}")
    try:
        fcntl.ioctl(fd, KDSETMODE, KD_TEXT)
        try:
            fcntl.ioctl(fd, VT_RELDISP, 1)
        except OSError:
            pass
        fcntl.ioctl(fd, VT_SETMODE, struct.pack("bbhhh", VT_AUTO, 0, 0, 0, 0))
    finally:
        os.close(fd)


def switch_to(vt: int) -> None:
    fd = open_tty("/dev/tty0")
    try:
        fcntl.ioctl(fd, VT_ACTIVATE, vt)
        fcntl.ioctl(fd, VT_WAITACTIVE, vt)
    finally:
        os.close(fd)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--from",
        dest="from_vt",
        type=int,
        help="stuck VT number (default: currently active)",
    )
    parser.add_argument(
        "--to",
        dest="to_vt",
        type=int,
        default=1,
        help="VT to activate after unlock (default: 1)",
    )
    args = parser.parse_args()

    if os.geteuid() != 0:
        print("needs root (sudo)", file=sys.stderr)
        return 1

    from_vt = args.from_vt if args.from_vt is not None else active_vt()
    print(f"active before: tty{active_vt()}")
    print(f"unlocking tty{from_vt}, switching to tty{args.to_vt}")
    unlock(from_vt)
    switch_to(args.to_vt)
    print(f"active after: {open('/sys/class/tty/tty0/active').read().strip()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
