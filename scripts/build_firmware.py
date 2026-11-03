#!/usr/bin/env python3
"""Build and package firmware for a named Astra918 board."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess


BOARDS = {
    "rpipico2": {
        "variant": "rp235xa",
        "flash_bytes": 4 * 1024 * 1024,
        "mcu_crystal_hz": 12_000_000,
    },
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--board", choices=BOARDS, default="rpipico2")
    args = parser.parse_args()
    board = BOARDS[args.board]
    root = Path(__file__).resolve().parents[1]
    subprocess.run(["cargo", "build", "--locked", "-p", "astra918-firmware", "--bin", "astra918",
                    "--release", "--target", "thumbv8m.main-none-eabihf", "--features",
                    f"device,{board['variant']}"], cwd=root, check=True)
    directory = root / "artifacts" / args.board
    directory.mkdir(parents=True, exist_ok=True)
    elf = directory / "astra918.elf"
    uf2 = root / "artifacts" / f"astra918-{args.board}.uf2"
    shutil.copy2(root / "target/thumbv8m.main-none-eabihf/release/astra918", elf)
    subprocess.run(["picotool", "uf2", "convert", str(elf), str(uf2)], check=True)
    subprocess.run(["picotool", "info", "-a", str(elf)], check=True)
    metadata = {"board": args.board, **board,
                "defmt_log": os.environ.get("DEFMT_LOG", "info"), "hardware_tested": False,
                "git_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
                "dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=root)),
                "sha256": {file.name: hashlib.sha256(file.read_bytes()).hexdigest() for file in (elf, uf2)}}
    (directory / "build.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(uf2)


if __name__ == "__main__":
    main()
