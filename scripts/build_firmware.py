#!/usr/bin/env python3
"""Build and package an RP2350 image; never opens a probe or flashes hardware."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--variant", choices=("rp235xa", "rp235xb"), required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    subprocess.run(["cargo", "build", "--locked", "-p", "astra918-firmware", "--bin", "astra918",
                    "--release", "--target", "thumbv8m.main-none-eabihf", "--features",
                    f"device,{args.variant}"], cwd=root, check=True)
    directory = root / "artifacts" / args.variant
    directory.mkdir(parents=True, exist_ok=True)
    elf = directory / "astra918.elf"
    uf2 = directory / "astra918.uf2"
    shutil.copy2(root / "target/thumbv8m.main-none-eabihf/release/astra918", elf)
    subprocess.run(["picotool", "uf2", "convert", str(elf), str(uf2)], check=True)
    subprocess.run(["picotool", "info", "-a", str(elf)], check=True)
    metadata = {"variant": args.variant, "flash_bytes": 4194304, "mcu_crystal_hz": 12000000,
                "defmt_log": os.environ.get("DEFMT_LOG", "info"), "hardware_tested": False,
                "git_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
                "dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=root)),
                "sha256": {file.name: hashlib.sha256(file.read_bytes()).hexdigest() for file in (elf, uf2)}}
    (directory / "build.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(directory)


if __name__ == "__main__":
    main()
