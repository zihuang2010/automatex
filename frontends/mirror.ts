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

// Q-3: H.264 NAL 类型常量
const NAL_TYPE_IDR = 5;
const NAL_TYPE_SPS = 7;
const NAL_HEADER_OFFSET = 4;
// SG-2: 解码队列积压阈值
const MAX_DECODE_QUEUE_SIZE = 3;

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

      // Q-3: 判断是否为关键帧（NAL type 5 = IDR）
      const nalType = chunk[NAL_HEADER_OFFSET] & 0x1f;
      const isKey = nalType === NAL_TYPE_IDR;

      // 等待第一个关键帧
      if (isKeyFrameNeeded && !isKey) return;
      isKeyFrameNeeded = false;

      // SG-2: 解码队列积压时丢弃 delta 帧，等下个 keyframe
      if (!isKey && decoder.decodeQueueSize > MAX_DECODE_QUEUE_SIZE) {
        isKeyFrameNeeded = true;
        return;
      }

      // 关键帧需要附带 SPS/PPS
      let frameData: Uint8Array = chunk;
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

    // 聚焦隐藏 input 以接收键盘/IME 输入
    const mirrorInput = $('#mirror-text-input') as HTMLInputElement;
    if (mirrorInput) mirrorInput.focus();
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
        metaState: 0,
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
    if (textInput) textInput.focus(); // 点击画面时聚焦隐藏 input
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

  // ── 键盘事件（方案 C：特殊键 keycode + 文本 inject_text + IME） ──

  // PC key → Android KEYCODE 映射
  const KEY_MAP: Record<string, number> = {
    Enter: 66,
    Backspace: 67,
    Delete: 112,
    Tab: 61,
    Escape: 111,
    ArrowUp: 19,
    ArrowDown: 20,
    ArrowLeft: 21,
    ArrowRight: 22,
    Home: 122,
    End: 123,
    PageUp: 92,
    PageDown: 93,
    F1: 131,
    F2: 132,
    F3: 133,
    F4: 134,
    F5: 135,
    F6: 136,
    F7: 137,
    F8: 138,
    F9: 139,
    F10: 140,
    F11: 141,
    F12: 142,
    // 音量/电源
    AudioVolumeUp: 24,
    AudioVolumeDown: 25,
  };

  // 修饰键 key → Android META flag
  const META_ALT_ON = 0x02;
  const META_SHIFT_ON = 0x01;
  const META_CTRL_ON = 0x1000;

  function getMetaState(e: KeyboardEvent): number {
    let meta = 0;
    if (e.altKey) meta |= META_ALT_ON;
    if (e.shiftKey) meta |= META_SHIFT_ON;
    if (e.ctrlKey || e.metaKey) meta |= META_CTRL_ON;
    return meta;
  }

  // 判断是否为纯修饰键
  const MODIFIER_KEYS = new Set(['Control', 'Shift', 'Alt', 'Meta', 'CapsLock']);

  // 单字符 key → Android KEYCODE（仅用于 Ctrl+字母 组合）
  function charToKeycode(key: string): number {
    const c = key.toUpperCase();
    if (c >= 'A' && c <= 'Z') return c.charCodeAt(0) - 65 + 29; // KEYCODE_A=29
    if (c >= '0' && c <= '9') return c.charCodeAt(0) - 48 + 7; // KEYCODE_0=7
    return 0;
  }

  // 隐藏文本输入框（用于 IME 和普通文本输入）
  const textInput = $('#mirror-text-input') as HTMLInputElement;

  // ── 隐藏 input 始终聚焦，承接所有文本输入（英文 + 中文 IME） ──
  if (textInput) {
    // 使 input 可聚焦但不可见
    textInput.style.position = 'fixed';
    textInput.style.opacity = '0';
    textInput.style.pointerEvents = 'none';
    textInput.style.width = '1px';
    textInput.style.height = '1px';
    textInput.style.left = '0';
    textInput.style.top = '0';
    textInput.style.border = 'none';
    textInput.style.padding = '0';
    textInput.style.outline = 'none';
    // 允许 focus（programmatic）
    textInput.removeAttribute('tabindex');

    let isComposing = false;

    // ── 特殊键：在 input 上拦截（Backspace/Enter/方向键等） ──
    textInput.addEventListener('keydown', e => {
      if (!activeMirror) return;
      if (MODIFIER_KEYS.has(e.key)) return;

      // 特殊键 → inject_key（始终处理，包括 IME 状态下的 Backspace）
      if (KEY_MAP[e.key] && !isComposing) {
        e.preventDefault();
        const meta = getMetaState(e);
        invoke('scrcpy_inject_key', {
          serial: activeMirror.serial,
          keycode: KEY_MAP[e.key],
          metaState: meta,
        }).catch(err => console.error('[mirror] inject_key 失败:', err));
        return;
      }

      // Ctrl/Meta + 普通键组合（如 Ctrl+C / Cmd+A）
      // 禁止 Ctrl+V / Cmd+V（粘贴会导致阻塞卡死）
      if ((e.ctrlKey || e.metaKey) && e.key.length === 1) {
        if (e.key === 'v' || e.key === 'V') return; // 禁止粘贴
        e.preventDefault();
        const keycode = charToKeycode(e.key);
        if (keycode === 0) return;
        const meta = getMetaState(e);
        invoke('scrcpy_inject_key', {
          serial: activeMirror.serial,
          keycode,
          metaState: meta,
        }).catch(err => console.error('[mirror] inject_key 失败:', err));
        return;
      }

      // 普通字符：不 preventDefault，让 input 自然接收
      // → 通过下面的 input/compositionend 事件发送
    });

    // ── IME 组合状态跟踪 ──
    textInput.addEventListener('compositionstart', () => {
      isComposing = true;
    });

    textInput.addEventListener('compositionend', () => {
      isComposing = false;
      const text = textInput.value;
      if (text && activeMirror) {
        invoke('scrcpy_inject_text', {
          serial: activeMirror.serial,
          text,
        }).catch(err => console.error('[mirror] inject_text 失败:', err));
      }
      textInput.value = '';
    });

    // ── 非 IME 文本输入（英文字符、粘贴等） ──
    textInput.addEventListener('input', () => {
      if (isComposing) return;
      const text = textInput.value;
      if (text && activeMirror) {
        invoke('scrcpy_inject_text', {
          serial: activeMirror.serial,
          text,
        }).catch(err => console.error('[mirror] inject_text 失败:', err));
      }
      textInput.value = '';
    });

    // 防止 input 失焦（仅在 modal 可见时 re-focus）
    textInput.addEventListener('blur', () => {
      setTimeout(() => {
        const modal = $('#mirror-modal') as HTMLElement;
        if (activeMirror && textInput && modal?.style.display !== 'none') {
          textInput.focus();
        }
      }, 50);
    });
  }
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
      if (nalType === NAL_TYPE_SPS && nalStart + 3 < configData.length) {
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
