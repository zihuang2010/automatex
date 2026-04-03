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
    "icon.png": 512,
    "Square30x30Logo.png": 30,
    "Square44x44Logo.png": 44,
    "Square71x71Logo.png": 71,
    "Square89x89Logo.png": 89,
    "Square107x107Logo.png": 107,
    "Square142x142Logo.png": 142,
    "Square150x150Logo.png": 150,
    "Square284x284Logo.png": 284,
    "Square310x310Logo.png": 310,
    "StoreLogo.png": 50,
}

IOS_SIZES = {
    "AppIcon-20x20@1x.png": 20,
    "AppIcon-20x20@2x.png": 40,
    "AppIcon-20x20@2x-1.png": 40,
    "AppIcon-20x20@3x.png": 60,
    "AppIcon-29x29@1x.png": 29,
    "AppIcon-29x29@2x.png": 58,
    "AppIcon-29x29@2x-1.png": 58,
    "AppIcon-29x29@3x.png": 87,
    "AppIcon-40x40@1x.png": 40,
    "AppIcon-40x40@2x.png": 80,
    "AppIcon-40x40@2x-1.png": 80,
    "AppIcon-40x40@3x.png": 120,
    "AppIcon-60x60@2x.png": 120,
    "AppIcon-60x60@3x.png": 180,
    "AppIcon-76x76@1x.png": 76,
    "AppIcon-76x76@2x.png": 152,
    "AppIcon-83.5x83.5@2x.png": 167,
    "AppIcon-512@2x.png": 1024,
}


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
    save_ios_icons(source)
    save_icns(source)
    save_ico(source)

    print("图标资源已统一重建完成。")


if __name__ == "__main__":
    main()
