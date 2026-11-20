"""komotype phase-0 probe: finds keyboards running the komotype ZMK module and prints their reports.

Usage:
    pip install hidapi
    python tools/probe.py            # listen to all komotype keyboards (USB and Bluetooth)
    python tools/probe.py --list     # list all raw HID interfaces (usage page 0xFF60)
"""

import argparse
import struct
import sys
import threading
import time

import hid

USAGE_PAGE = 0xFF60
USAGE = 0x61
REPORT_SIZE = 32

MAGIC = 0x4B
VERSION = 1
MSG_STATE = 0x01
MSG_SYNC = 0x80

BUS_NAMES = {1: "USB", 2: "Bluetooth", 3: "I2C", 4: "SPI"}


def raw_hid_interfaces():
    return [
        d for d in hid.enumerate()
        if d["usage_page"] == USAGE_PAGE and d["usage"] == USAGE
    ]


def describe(d):
    bus = BUS_NAMES.get(d.get("bus_type"), "?")
    name = f'{d["manufacturer_string"] or ""} {d["product_string"] or ""}'.strip() or "(no name)"
    return f'{name} [{bus}] VID {d["vendor_id"]:04X} PID {d["product_id"]:04X}'


def battery(value):
    return "?" if value == 0xFF else f"{value}%"


def decode(data):
    if len(data) < 4 or data[0] != MAGIC:
        return f"foreign report: {bytes(data).hex(' ')}"
    if data[1] != VERSION:
        return f"unknown protocol version {data[1]}"
    if data[2] != MSG_STATE:
        return f"unknown message type 0x{data[2]:02X}"
    seq, layer = data[3], data[4]
    (mask,) = struct.unpack_from("<I", bytes(data), 5)
    central, count = data[9], data[10]
    peripherals = " ".join(battery(b) for b in data[11:11 + count]) or "-"
    (uptime,) = struct.unpack_from("<I", bytes(data), 15)
    active = [i for i in range(32) if mask >> i & 1]
    return (f"seq {seq:3} | layer {layer} (active {active}) | battery central {battery(central)}"
            f" peripherals {peripherals} | uptime {uptime}s")


def listen(info, stop):
    label = describe(info)
    last_seq = None
    while not stop.is_set():
        dev = hid.device()
        try:
            dev.open_path(info["path"])
        except OSError as e:
            print(f"[{label}] cannot open: {e}")
            return
        print(f"[{label}] opened, sending SYNC")
        try:
            # Leading 0x00 = report ID (raw HID uses no report IDs).
            packet = [0x00, MAGIC, VERSION, MSG_SYNC] + [0] * (REPORT_SIZE - 3)
            dev.write(packet)
            while not stop.is_set():
                data = dev.read(REPORT_SIZE, 1000)
                if not data:
                    continue
                stamp = time.strftime("%H:%M:%S")
                line = decode(data)
                if last_seq is not None and data[0] == MAGIC and data[3] != (last_seq + 1) % 256:
                    line += f"  (gap: expected seq {(last_seq + 1) % 256})"
                last_seq = data[3] if data[0] == MAGIC else last_seq
                print(f"{stamp} [{label}] {line}")
        except OSError as e:
            print(f"[{label}] disconnected: {e}")
            return
        finally:
            dev.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--list", action="store_true", help="only list raw HID interfaces")
    args = parser.parse_args()

    interfaces = raw_hid_interfaces()
    if args.list or not interfaces:
        if not interfaces:
            print("No raw HID interface (usage page 0xFF60) found.")
            print("Check: komotype firmware flashed? Keyboard connected via USB or paired via Bluetooth?")
        for d in interfaces:
            print(describe(d), "\n   ", d["path"].decode(errors="replace"))
        return 0 if interfaces else 1

    stop = threading.Event()
    threads = [threading.Thread(target=listen, args=(d, stop), daemon=True) for d in interfaces]
    for t in threads:
        t.start()
    print("Listening. Change layers to see reports, Ctrl+C to quit.")
    try:
        while any(t.is_alive() for t in threads):
            time.sleep(0.5)
    except KeyboardInterrupt:
        stop.set()
    return 0


if __name__ == "__main__":
    sys.exit(main())
