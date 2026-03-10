/**
 * mirror.ts — 设备投屏模块
 *
 * 使用 WebCodecs VideoDecoder 解码 H.264 帧，Canvas2D 即时渲染。
 * 相比 jMuxer/MSE 方案，消除了 500-800ms 的 MSE 缓冲延迟。
 * 帧数据通过 Tauri Channel 以 Vec<u8> 直传（不再 base64）。
 */
import { Channel, invoke } from '@tauri-apps/api/core';
import { type UnlistenFn, listen } from '@tauri-apps/api/event';

import { $, showToast } from './utils';

// ─── 类型 ─────────────────────────────────────────────

interface FramePayload {
  data: number[]; // Vec<u8> 直传
  is_config: boolean;
  ts: number;
}

interface MirrorStartedPayload {
  serial: string;
  width: number;
  height: number;
}

const TOUCH_ACTION = {
  DOWN: 0,
  UP: 1,
  MOVE: 2,
} as const;

// ─── 状态 ─────────────────────────────────────────────

let activeMirror: {
  serial: string;
  decoder: VideoDecoder;
  unlistenStopped: UnlistenFn;
  width: number;
  height: number;
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
} | null = null;

// ─── 公开 API ─────────────────────────────────────────

/**
 * 启动投屏
 */
export async function startMirror(serial: string) {
  if (activeMirror) {
    if (activeMirror.serial === serial) return;
    await stopMirror();
  }

  const modal = $('#mirror-modal') as HTMLElement;
  const canvas = $('#scrcpy-canvas') as HTMLCanvasElement;
  const statusEl = $('#mirror-status') as HTMLElement;
  const titleEl = $('#mirror-title') as HTMLElement;
  const backdrop = $('#mirror-backdrop') as HTMLElement;
  const drawer = $('#mirror-drawer') as HTMLElement;
  const loading = $('#mirror-loading') as HTMLElement;

  if (!modal || !canvas) return;

  // 抽屉滑入动画
  modal.style.display = 'block';
  if (loading) loading.style.display = 'flex';
  requestAnimationFrame(() => {
    if (backdrop) backdrop.classList.add('opacity-100');
    if (drawer) drawer.classList.remove('translate-x-full');
  });
  if (statusEl) statusEl.textContent = '正在连接…';
  if (titleEl) titleEl.textContent = serial;

  try {
    const ctx = canvas.getContext('2d')!;

    // WebCodecs 解码器：每帧解码后立即绘制到 Canvas（零缓冲）
    const decoder = new VideoDecoder({
      output: (frame: VideoFrame) => {
        if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
          canvas.width = frame.displayWidth;
          canvas.height = frame.displayHeight;
        }
        ctx.drawImage(frame, 0, 0);
        frame.close();

        // 首帧到达时隐藏 loading 占位
        const ld = $('#mirror-loading') as HTMLElement;
        if (ld && ld.style.display !== 'none') ld.style.display = 'none';
      },
      error: (e: DOMException) => {
        console.error('[mirror] 解码错误:', e);
      },
    });

    // 帧 Channel
    let lastTs = 0;
    let configData: Uint8Array | null = null; // 缓存 SPS/PPS
    let isKeyFrameNeeded = true;

    const onFrame = new Channel<FramePayload>();
    onFrame.onmessage = (frame: FramePayload) => {
      const { data, is_config, ts } = frame;

      // 丢帧
      if (!is_config && ts < lastTs) return;
      lastTs = ts;

      if (data.length === 0) return;

      const chunk = new Uint8Array(data);

      if (is_config) {
        // SPS/PPS 配置帧：用于初始化或重新配置解码器
        configData = chunk;

        // 从 SPS 解析 codec 字符串并配置解码器
        const codecStr = parseCodecFromSPS(chunk);
        try {
          decoder.configure({
            codec: codecStr,
            optimizeForLatency: true,
          });
          isKeyFrameNeeded = true;
          console.log('[mirror] 解码器已配置:', codecStr);
        } catch (e) {
          console.error('[mirror] 解码器配置失败:', e);
        }
        return;
      }

      if (decoder.state !== 'configured') return;

      // 判断是否为关键帧（NAL type 5 = IDR）
      const nalType = chunk[4] & 0x1f;
      const isKey = nalType === 5;

      // 等待第一个关键帧
      if (isKeyFrameNeeded && !isKey) return;
      isKeyFrameNeeded = false;

      // 关键帧需要附带 SPS/PPS
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      let frameData: any = chunk;
      if (isKey && configData) {
        frameData = concatUint8Arrays(configData, chunk);
      }

      decoder.decode(
        new EncodedVideoChunk({
          type: isKey ? 'key' : 'delta',
          timestamp: ts * 1000, // 微秒
          data: frameData,
        }),
      );
    };

    // 调用后端，传入 Channel
    const result: MirrorStartedPayload = await invoke('scrcpy_start_mirror', {
      serial,
      onFrame,
    });

    // 设置 canvas 初始尺寸
    canvas.width = result.width;
    canvas.height = result.height;

    // 监听断开
    const unlistenStopped = await listen<string>('scrcpy-stopped', event => {
      if (activeMirror?.serial === event.payload) {
        cleanupMirrorState();
        showToast('投屏已断开', 'info');
      }
    });

    activeMirror = {
      serial,
      decoder,
      unlistenStopped,
      width: result.width,
      height: result.height,
      canvas,
      ctx,
    };

    if (statusEl) statusEl.textContent = `${result.width}×${result.height}`;

    const container = $('#mirror-video-wrap') as HTMLElement;
    if (container) {
      container.style.aspectRatio = `${result.width} / ${result.height}`;
    }
  } catch (e) {
    if (statusEl) statusEl.textContent = `连接失败: ${e}`;
    console.error('[mirror] 启动失败:', e);
  }
}

/**
 * 停止投屏
 */
export async function stopMirror() {
  if (!activeMirror) return;

  const { serial } = activeMirror;
  cleanupMirrorState();

  try {
    await invoke('scrcpy_stop_mirror', { serial });
  } catch (e) {
    console.warn('[mirror] 停止失败:', e);
  }
}

function cleanupMirrorState() {
  if (!activeMirror) return;

  const { decoder, unlistenStopped } = activeMirror;
  activeMirror = null;

  unlistenStopped();

  try {
    decoder.close();
  } catch {
    /* ignore */
  }

  // 抽屉滑出动画
  const modal = $('#mirror-modal') as HTMLElement;
  const backdrop = $('#mirror-backdrop') as HTMLElement;
  const drawer = $('#mirror-drawer') as HTMLElement;

  if (backdrop) backdrop.classList.remove('opacity-100');
  if (drawer) drawer.classList.add('translate-x-full');

  // 等待 transition 结束后隐藏
  setTimeout(() => {
    if (modal) modal.style.display = 'none';
  }, 320);
}

/**
 * 初始化投屏模块：绑定 modal 事件
 */
export function initMirror() {
  $('#mirror-close')?.addEventListener('click', () => stopMirror());

  // 遮罩点击关闭
  $('#mirror-backdrop')?.addEventListener('click', () => stopMirror());

  $('#mirror-back')?.addEventListener('click', () => {
    if (activeMirror) {
      invoke('scrcpy_press_back', { serial: activeMirror.serial }).catch(console.warn);
    }
  });

  $('#mirror-home')?.addEventListener('click', () => {
    if (activeMirror) {
      invoke('scrcpy_inject_key', {
        serial: activeMirror.serial,
        keycode: 3,
        meta_state: 0,
      }).catch(console.warn);
    }
  });

  // ── 触控事件 ──
  const canvas = $('#scrcpy-canvas') as HTMLCanvasElement;
  if (!canvas) return;

  let isDown = false;

  const getDeviceCoords = (e: MouseEvent): { x: number; y: number } | null => {
    if (!activeMirror) return null;
    const rect = canvas.getBoundingClientRect();
    const scaleX = activeMirror.width / rect.width;
    const scaleY = activeMirror.height / rect.height;
    return {
      x: Math.round((e.clientX - rect.left) * scaleX),
      y: Math.round((e.clientY - rect.top) * scaleY),
    };
  };

  canvas.addEventListener('mousedown', e => {
    e.preventDefault();
    isDown = true;
    const coords = getDeviceCoords(e);
    if (coords && activeMirror) {
      invoke('scrcpy_inject_touch', {
        serial: activeMirror.serial,
        action: TOUCH_ACTION.DOWN,
        x: coords.x,
        y: coords.y,
      }).catch(console.warn);
    }
  });

  let pendingMove: { x: number; y: number } | null = null;
  let rafId = 0;

  canvas.addEventListener('mousemove', e => {
    if (!isDown) return;
    const coords = getDeviceCoords(e);
    if (!coords || !activeMirror) return;

    pendingMove = coords;
    if (!rafId) {
      rafId = requestAnimationFrame(() => {
        rafId = 0;
        if (pendingMove && activeMirror) {
          invoke('scrcpy_inject_touch', {
            serial: activeMirror.serial,
            action: TOUCH_ACTION.MOVE,
            x: pendingMove.x,
            y: pendingMove.y,
          }).catch(console.warn);
          pendingMove = null;
        }
      });
    }
  });

  canvas.addEventListener('mouseup', e => {
    isDown = false;
    if (rafId) {
      cancelAnimationFrame(rafId);
      rafId = 0;
      pendingMove = null;
    }
    const coords = getDeviceCoords(e);
    if (coords && activeMirror) {
      invoke('scrcpy_inject_touch', {
        serial: activeMirror.serial,
        action: TOUCH_ACTION.UP,
        x: coords.x,
        y: coords.y,
      }).catch(console.warn);
    }
  });

  canvas.addEventListener('mouseleave', () => {
    if (isDown && activeMirror) {
      isDown = false;
      if (rafId) {
        cancelAnimationFrame(rafId);
        rafId = 0;
        pendingMove = null;
      }
      invoke('scrcpy_inject_touch', {
        serial: activeMirror.serial,
        action: TOUCH_ACTION.UP,
        x: 0,
        y: 0,
      }).catch(console.warn);
    }
  });
}

// ─── 工具函数 ─────────────────────────────────────────

/**
 * 从 SPS NAL unit 解析 H.264 codec 字符串
 * 格式: avc1.PPCCLL (profile_idc, constraint_flags, level_idc)
 */
function parseCodecFromSPS(configData: Uint8Array): string {
  // 查找 SPS NAL (type 7) —— 在 Annex B 格式中
  for (let i = 0; i < configData.length - 4; i++) {
    // 找到 start code (00 00 00 01 或 00 00 01)
    let nalStart = -1;
    if (configData[i] === 0 && configData[i + 1] === 0) {
      if (configData[i + 2] === 0 && configData[i + 3] === 1) {
        nalStart = i + 4;
      } else if (configData[i + 2] === 1) {
        nalStart = i + 3;
      }
    }

    if (nalStart >= 0 && nalStart < configData.length) {
      const nalType = configData[nalStart] & 0x1f;
      if (nalType === 7 && nalStart + 3 < configData.length) {
        // SPS 找到：profile_idc, constraint_flags, level_idc
        const profile = configData[nalStart + 1];
        const constraints = configData[nalStart + 2];
        const level = configData[nalStart + 3];
        const codec = `avc1.${hex(profile)}${hex(constraints)}${hex(level)}`;
        console.log(`[mirror] 解析 codec: ${codec}`);
        return codec;
      }
    }
  }

  // 回退：H.264 Baseline Profile Level 3.1
  console.warn('[mirror] 未找到 SPS，使用默认 codec');
  return 'avc1.42001f';
}

function hex(n: number): string {
  return n.toString(16).padStart(2, '0');
}

function concatUint8Arrays(a: Uint8Array, b: Uint8Array): Uint8Array {
  const result = new Uint8Array(a.length + b.length);
  result.set(a, 0);
  result.set(b, a.length);
  return result;
}
