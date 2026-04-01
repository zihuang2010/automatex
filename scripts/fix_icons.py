#!/usr/bin/env python3
"""
从原始银杏叶图标生成符合 macOS HIG 规范的 squircle 图标。
squircle 填满整个画布，银杏叶居中带适当内边距。
"""

import os
import subprocess
import shutil
from PIL import Image, ImageDraw
import numpy as np

ICONS_DIR = os.path.join(os.path.dirname(__file__), '..', 'backends', 'icons')

# 原始银杏叶图标（从 git 恢复到 /tmp）
ORIGINAL_ICON = '/tmp/icon_1b34586.png'


def extract_leaf(img: Image.Image, tolerance: int = 35) -> Image.Image:
    """
    从原始图标中提取银杏叶（移除米色背景）。
    使用 flood fill 从边缘开始移除连通的背景区域。
    """
    img = img.convert('RGBA')
    data = np.array(img)
    h, w = data.shape[:2]
    
    # 从四个角采样背景色
    corners = [data[0, 0, :3], data[0, w-1, :3], data[h-1, 0, :3], data[h-1, w-1, :3]]
    bg_color = np.mean(corners, axis=0).astype(np.uint8)
    
    # 计算每个像素与背景色的距离
    diff = np.sqrt(np.sum((data[:, :, :3].astype(float) - bg_color.astype(float)) ** 2, axis=2))
    is_bg_like = diff < tolerance
    
    # BFS flood fill 从边缘开始
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
    
    # 设置背景为透明
    data[mask, 3] = 0
    
    # 边缘羽化 2px
    from scipy.ndimage import distance_transform_edt
    inner = ~mask
    dist = distance_transform_edt(~inner)
    feather_width = 2
    feather_zone = (dist > 0) & (dist <= feather_width) & mask
    data[feather_zone, 3] = ((1.0 - dist[feather_zone] / feather_width) * 255).astype(np.uint8)
    
    return Image.fromarray(data)


def create_squircle_icon(leaf: Image.Image, size: int = 1024) -> Image.Image:
    """
    创建填满整个画布的 squircle 图标。
    """
    # 1. 创建 squircle mask - 填满整个画布
    print("  创建 squircle mask（填满画布）...")
    ss = 2  # 2x 超采样
    ss_size = size * ss
    
    y_coords, x_coords = np.mgrid[0:ss_size, 0:ss_size]
    center = ss_size / 2.0
    # scale = half size，让 squircle 正好触及画布边缘
    scale = ss_size / 2.0
    
    nx = (x_coords - center) / scale
    ny = (y_coords - center) / scale
    
    # 超椭圆 n=5 ≈ macOS continuous corner
    n = 5.0
    dist = np.abs(nx) ** n + np.abs(ny) ** n
    
    mask_data = np.zeros((ss_size, ss_size), dtype=np.uint8)
    mask_data[dist <= 1.0] = 255
    
    # 边缘抗锯齿
    aa = 0.02
    edge = (dist > 1.0 - aa) & (dist < 1.0 + aa)
    mask_data[edge] = ((1.0 - (dist[edge] - (1.0 - aa)) / (2 * aa)) * 255).clip(0, 255).astype(np.uint8)
    
    mask = Image.fromarray(mask_data).resize((size, size), Image.LANCZOS)
    
    # 2. 创建渐变背景
    print("  创建渐变背景...")
    bg_data = np.zeros((size, size, 4), dtype=np.uint8)
    for y in range(size):
        t = y / (size - 1)
        # 深棕到深琥珀渐变
        bg_data[y, :, 0] = int(42 + t * 28)   # R: 42→70
        bg_data[y, :, 1] = int(28 + t * 18)   # G: 28→46
        bg_data[y, :, 2] = int(18 + t * 14)   # B: 18→32
        bg_data[y, :, 3] = 255
    bg = Image.fromarray(bg_data)
    bg.putalpha(mask)
    
    # 3. 处理银杏叶 - 裁剪掉透明区域后居中放置
    print("  合成银杏叶...")
    bbox = leaf.getbbox()
    if bbox:
        leaf_cropped = leaf.crop(bbox)
    else:
        leaf_cropped = leaf
    
    # 银杏叶占画布 65% 的区域（留适当内边距，让叶子看起来大而自然）
    content_ratio = 0.65
    content_area = int(size * content_ratio)
    
    lw, lh = leaf_cropped.size
    ratio = min(content_area / lw, content_area / lh)
    new_w, new_h = int(lw * ratio), int(lh * ratio)
    leaf_resized = leaf_cropped.resize((new_w, new_h), Image.LANCZOS)
    
    # 居中
    ox = (size - new_w) // 2
    oy = (size - new_h) // 2
    
    # 4. 合成
    result = Image.new('RGBA', (size, size), (0, 0, 0, 0))
    result.paste(bg, (0, 0))
    result.paste(leaf_resized, (ox, oy), leaf_resized)
    
    # 重新应用 mask 确保 squircle 形状
    final_alpha = np.minimum(np.array(result.split()[3]), np.array(mask))
    result.putalpha(Image.fromarray(final_alpha))
    
    return result


def generate_all(src: Image.Image):
    """生成所有尺寸的图标文件"""
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


def deploy_to_app():
    """替换 /Applications/AutomateX.app 和构建缓存中的图标"""
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
    
    # touch .app 强制刷新
    app_path = '/Applications/AutomateX.app'
    if os.path.exists(app_path):
        os.utime(app_path, None)
        os.utime(os.path.join(app_path, 'Contents', 'Info.plist'), None)
        os.utime(os.path.join(app_path, 'Contents', 'Resources', 'icon.icns'), None)


def main():
    print("=" * 50)
    print("AutomateX 图标修复 v3")
    print("=" * 50)
    
    # 1. 从原始图标提取银杏叶
    print(f"\n[1/4] 提取银杏叶...")
    original = Image.open(ORIGINAL_ICON)
    leaf = extract_leaf(original)
    print(f"  ✓ 银杏叶已提取")
    
    # 2. 创建 squircle 图标
    print(f"\n[2/4] 创建 squircle 图标 (1024x1024)...")
    icon = create_squircle_icon(leaf, 1024)
    icon.save(os.path.join(ICONS_DIR, 'icon_1024.png'), 'PNG')
    
    # 3. 生成所有尺寸
    print(f"\n[3/4] 生成所有尺寸...")
    generate_all(icon)
    
    # 4. 部署到应用
    print(f"\n[4/4] 部署到应用...")
    deploy_to_app()
    
    print(f"\n{'=' * 50}")
    print("✅ 完成！请执行: killall Dock")
    print(f"{'=' * 50}")


if __name__ == '__main__':
    main()
