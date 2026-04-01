#!/usr/bin/env python3
"""
生成图标 v5 — 纯白背景 + 银杏叶

策略：
  1. 纯白色 (#FFFFFF) 不透明正方形背景
  2. 银杏叶居中，占 60% 画布
  3. 四角 Alpha=255，完全不透明
  4. 让 macOS 自动裁剪 squircle
"""

import os
import subprocess
import shutil
from PIL import Image
import numpy as np

ICONS_DIR = os.path.join(os.path.dirname(__file__), '..', 'backends', 'icons')
ORIGINAL_ICON = '/tmp/icon_1b34586.png'


def extract_leaf(img: Image.Image, tolerance: int = 35) -> Image.Image:
    """从原始图标中提取银杏叶（移除米色背景）"""
    img = img.convert('RGBA')
    data = np.array(img)
    h, w = data.shape[:2]

    corners = [data[0, 0, :3], data[0, w-1, :3], data[h-1, 0, :3], data[h-1, w-1, :3]]
    bg_color = np.mean(corners, axis=0).astype(np.uint8)

    diff = np.sqrt(np.sum((data[:, :, :3].astype(float) - bg_color.astype(float)) ** 2, axis=2))
    is_bg_like = diff < tolerance

    from collections import deque
    mask = np.zeros((h, w), dtype=bool)
    queue = deque()

    for x in range(w):
        if is_bg_like[0, x]:
            queue.append((0, x)); mask[0, x] = True
        if is_bg_like[h-1, x]:
            queue.append((h-1, x)); mask[h-1, x] = True
    for y in range(1, h-1):
        if is_bg_like[y, 0]:
            queue.append((y, 0)); mask[y, 0] = True
        if is_bg_like[y, w-1]:
            queue.append((y, w-1)); mask[y, w-1] = True

    while queue:
        cy, cx = queue.popleft()
        for dy, dx in [(-1,0),(1,0),(0,-1),(0,1)]:
            ny, nx = cy+dy, cx+dx
            if 0 <= ny < h and 0 <= nx < w and not mask[ny, nx] and is_bg_like[ny, nx]:
                mask[ny, nx] = True
                queue.append((ny, nx))

    data[mask, 3] = 0

    from scipy.ndimage import distance_transform_edt
    inner = ~mask
    dist = distance_transform_edt(~inner)
    feather_width = 2
    feather_zone = (dist > 0) & (dist <= feather_width) & mask
    data[feather_zone, 3] = ((1.0 - dist[feather_zone] / feather_width) * 255).astype(np.uint8)

    return Image.fromarray(data)


def create_icon(leaf: Image.Image, size: int = 1024) -> Image.Image:
    """深棕渐变背景 + 银杏叶居中（Full Bleed 不透明正方形）"""
    # 1. 深棕→琥珀渐变背景 — 四角完全不透明
    bg_data = np.zeros((size, size, 4), dtype=np.uint8)
    for y in range(size):
        t = y / (size - 1)
        bg_data[y, :, 0] = int(42 + t * 28)   # R: 42→70
        bg_data[y, :, 1] = int(28 + t * 18)   # G: 28→46
        bg_data[y, :, 2] = int(18 + t * 14)   # B: 18→32
        bg_data[y, :, 3] = 255                 # 完全不透明
    result = Image.fromarray(bg_data)

    # 2. 处理银杏叶
    bbox = leaf.getbbox()
    leaf_cropped = leaf.crop(bbox) if bbox else leaf

    content_ratio = 0.60
    content_area = int(size * content_ratio)

    lw, lh = leaf_cropped.size
    ratio = min(content_area / lw, content_area / lh)
    new_w, new_h = int(lw * ratio), int(lh * ratio)
    leaf_resized = leaf_cropped.resize((new_w, new_h), Image.LANCZOS)

    ox = (size - new_w) // 2
    oy = (size - new_h) // 2

    # 3. 合成
    result.paste(leaf_resized, (ox, oy), leaf_resized)

    return result


def generate_all(src: Image.Image):
    """生成所有尺寸"""
    sizes = {
        '32x32.png': 32, '64x64.png': 64, '128x128.png': 128,
        '128x128@2x.png': 256, 'icon.png': 512,
        'Square30x30Logo.png': 30, 'Square44x44Logo.png': 44,
        'Square71x71Logo.png': 71, 'Square89x89Logo.png': 89,
        'Square107x107Logo.png': 107, 'Square142x142Logo.png': 142,
        'Square150x150Logo.png': 150, 'Square284x284Logo.png': 284,
        'Square310x310Logo.png': 310, 'StoreLogo.png': 50,
    }
    for name, s in sizes.items():
        path = os.path.join(ICONS_DIR, name)
        src.resize((s, s), Image.LANCZOS).save(path, 'PNG')
        print(f"  ✓ {name}")

    # .icns
    iconset = os.path.join(ICONS_DIR, 'icon.iconset')
    os.makedirs(iconset, exist_ok=True)
    for name, s in {'icon_16x16.png':16,'icon_16x16@2x.png':32,'icon_32x32.png':32,
                     'icon_32x32@2x.png':64,'icon_128x128.png':128,'icon_128x128@2x.png':256,
                     'icon_256x256.png':256,'icon_256x256@2x.png':512,
                     'icon_512x512.png':512,'icon_512x512@2x.png':1024}.items():
        src.resize((s, s), Image.LANCZOS).save(os.path.join(iconset, name), 'PNG')
    r = subprocess.run(['iconutil', '-c', 'icns', iconset, '-o', os.path.join(ICONS_DIR, 'icon.icns')],
                       capture_output=True, text=True)
    print(f"  ✓ icon.icns" if r.returncode == 0 else f"  ✗ icon.icns: {r.stderr}")
    shutil.rmtree(iconset)

    # .ico
    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    imgs = [src.resize((s, s), Image.LANCZOS) for s in ico_sizes]
    imgs[0].save(os.path.join(ICONS_DIR, 'icon.ico'), 'ICO',
                 sizes=[(s,s) for s in ico_sizes], append_images=imgs[1:])
    print(f"  ✓ icon.ico")


def deploy_and_nuke_cache():
    """替换图标 + 暴力清理所有缓存"""
    icns = os.path.join(ICONS_DIR, 'icon.icns')
    targets = [
        '/Applications/AutomateX.app/Contents/Resources/icon.icns',
        os.path.join(ICONS_DIR, '..', 'target', 'aarch64-apple-darwin', 'release',
                     'bundle', 'macos', 'AutomateX.app', 'Contents', 'Resources', 'icon.icns'),
        os.path.join(ICONS_DIR, '..', 'target', 'aarch64-apple-darwin', 'release',
                     'bundle', 'dmg', 'icon.icns'),
    ]
    for t in targets:
        t = os.path.normpath(t)
        if os.path.exists(os.path.dirname(t)):
            shutil.copy2(icns, t)
            print(f"  ✓ 已替换 {t}")

    # touch 应用
    app_path = '/Applications/AutomateX.app'
    if os.path.exists(app_path):
        os.utime(app_path, None)
        for sub in ['Contents/Info.plist', 'Contents/Resources/icon.icns']:
            p = os.path.join(app_path, sub)
            if os.path.exists(p):
                os.utime(p, None)

    # 暴力清理图标缓存
    print("  暴力清除所有图标缓存...")

    # 1. lsregister 重建
    lsregister = ('/System/Library/Frameworks/CoreServices.framework/Versions/A/'
                  'Frameworks/LaunchServices.framework/Versions/A/Support/lsregister')
    subprocess.run([lsregister, '-kill', '-r', '-domain', 'local',
                    '-domain', 'system', '-domain', 'user'], capture_output=True)

    # 2. 删除 IconServices 缓存
    import glob
    home = os.path.expanduser('~')
    cache_dirs = glob.glob(os.path.join(home, 'Library/Caches/com.apple.iconservices*'))
    for d in cache_dirs:
        try:
            shutil.rmtree(d)
            print(f"  ✓ 删除: {d}")
        except Exception as e:
            print(f"  ⚠ 无法删除 {d}: {e}")

    # 3. 删除 Dock 缓存
    dock_caches = glob.glob(os.path.join(home, 'Library/Caches/com.apple.dock*'))
    for d in dock_caches:
        try:
            shutil.rmtree(d)
            print(f"  ✓ 删除: {d}")
        except Exception as e:
            print(f"  ⚠ 无法删除 {d}: {e}")

    # 4. 删除 IconServicesAgent 的 plist 缓存
    is_plist = os.path.join(home, 'Library/Preferences/com.apple.iconservices.store')
    if os.path.exists(is_plist):
        try:
            os.remove(is_plist)
            print(f"  ✓ 删除: {is_plist}")
        except:
            pass


def main():
    print("=" * 50)
    print("AutomateX 图标修复 v5 — 白色背景")
    print("=" * 50)

    # 0. 先恢复原始图标
    if not os.path.exists(ORIGINAL_ICON):
        print(f"\n[0/4] 从 git 恢复原始图标...")
        repo_root = os.path.join(os.path.dirname(__file__), '..')
        for commit in ['67450e7', '1b34586', 'HEAD~10']:
            r = subprocess.run(
                ['git', 'show', f'{commit}:backends/icons/icon.png'],
                capture_output=True, cwd=repo_root)
            if r.returncode == 0:
                with open(ORIGINAL_ICON, 'wb') as f:
                    f.write(r.stdout)
                print(f"  ✓ 已恢复 ({commit})")
                break
        else:
            print("  ✗ git 恢复失败")
            return

    # 1. 提取银杏叶
    print(f"\n[1/4] 提取银杏叶...")
    original = Image.open(ORIGINAL_ICON)
    leaf = extract_leaf(original)
    print(f"  ✓ 银杏叶已提取")

    # 2. 创建图标
    print(f"\n[2/4] 创建白色背景图标 (1024x1024)...")
    icon = create_icon(leaf, 1024)
    icon.save(os.path.join(ICONS_DIR, 'icon_1024.png'), 'PNG')

    # 验证四角
    pixels = np.array(icon)
    c = [pixels[0,0], pixels[0,-1], pixels[-1,0], pixels[-1,-1]]
    print(f"  左上角: RGBA={tuple(c[0])}")
    print(f"  右下角: RGBA={tuple(c[3])}")

    # 3. 生成所有尺寸
    print(f"\n[3/4] 生成所有尺寸...")
    generate_all(icon)

    # 4. 部署 + 清理缓存
    print(f"\n[4/4] 部署 + 清理缓存...")
    deploy_and_nuke_cache()

    print(f"\n{'=' * 50}")
    print("✅ 完成！")
    print("   1. 请执行: killall Dock")
    print("   2. 如果仍不生效，需要重新编译:")
    print("      cd backends && cargo tauri build")
    print(f"{'=' * 50}")


if __name__ == '__main__':
    main()
