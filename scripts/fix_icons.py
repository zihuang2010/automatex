#!/usr/bin/env python3
"""
统一从单一高分辨率源图重建所有桌面图标资源，避免 macOS / Windows
仍然引用旧的 icns/ico 资源而出现白边、旧底色或不一致样式。
"""

import os
import shutil
import subprocess
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
ICONS_DIR = ROOT / "backends" / "icons"
SOURCE_ICON = ICONS_DIR / "icon_1024.png"

PNG_SIZES = {
    "32x32.png": 32,
    "64x64.png": 64,
    "128x128.png": 128,
    "128x128@2x.png": 256,
    "icon.png": 512
}

IOS_SIZES = {}


def save_pngs(source: Image.Image) -> None:
    print("重建 PNG 图标...")
    for name, size in PNG_SIZES.items():
        target = ICONS_DIR / name
        source.resize((size, size), Image.LANCZOS).save(target, "PNG")
        print(f"  ✓ {name}")


def save_ios_icons(source: Image.Image) -> None:
    ios_dir = ICONS_DIR / "ios"
    ios_dir.mkdir(parents=True, exist_ok=True)
    print("重建 iOS 图标...")
    for name, size in IOS_SIZES.items():
        target = ios_dir / name
        source.resize((size, size), Image.LANCZOS).save(target, "PNG")
        print(f"  ✓ ios/{name}")


def save_icns(source: Image.Image) -> None:
    iconset_dir = ICONS_DIR / "icon.iconset"
    if iconset_dir.exists():
        shutil.rmtree(iconset_dir)
    iconset_dir.mkdir(parents=True, exist_ok=True)

    mac_sizes = {
        "icon_16x16.png": 16,
        "icon_16x16@2x.png": 32,
        "icon_32x32.png": 32,
        "icon_32x32@2x.png": 64,
        "icon_128x128.png": 128,
        "icon_128x128@2x.png": 256,
        "icon_256x256.png": 256,
        "icon_256x256@2x.png": 512,
        "icon_512x512.png": 512,
        "icon_512x512@2x.png": 1024,
    }

    print("重建 icon.icns...")
    for name, size in mac_sizes.items():
        source.resize((size, size), Image.LANCZOS).save(iconset_dir / name, "PNG")

    subprocess.run(
        ["iconutil", "-c", "icns", str(iconset_dir), "-o", str(ICONS_DIR / "icon.icns")],
        check=True,
    )
    shutil.rmtree(iconset_dir)
    print("  ✓ icon.icns")


def save_ico(source: Image.Image) -> None:
    print("重建 icon.ico...")
    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    images = [source.resize((size, size), Image.LANCZOS) for size in ico_sizes]
    images[0].save(
        ICONS_DIR / "icon.ico",
        format="ICO",
        sizes=[(size, size) for size in ico_sizes],
        append_images=images[1:],
    )
    print("  ✓ icon.ico")


def main() -> None:
    if not SOURCE_ICON.exists():
        raise SystemExit(f"缺少源图标: {SOURCE_ICON}")

    source = Image.open(SOURCE_ICON).convert("RGBA")
    source.save(SOURCE_ICON, "PNG")

    save_pngs(source)
    save_icns(source)
    save_ico(source)

    print("图标资源已统一重建完成。")


if __name__ == "__main__":
    main()
