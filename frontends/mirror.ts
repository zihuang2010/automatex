/**
 * mirror.ts — 设备投屏模块（悬浮窗版 v5）
 *
 * 每个投屏是一个独立的悬浮窗口（position:absolute），
 * 可自由拖拽、缩放、层级切换。不影响主界面任何交互。
 *
 * 架构：
 *   mirrors: Map<serial, MirrorInstance>  — 独立窗口实例
 *   focusedSerial                         — 键盘输入路由目标
 *   zIndexCounter                         — 置顶层级计数器
 *   拖拽引擎挂在每个窗口的 header 上
 */
import { Channel, invoke } from '@tauri-apps/api/core';
import { type UnlistenFn, listen } from '@tauri-apps/api/event';

import { getCachedDeviceResolution } from './devices';
import { $, esc, showToast } from './utils';

// ─── 类型 ─────────────────────────────────────────────

interface MirrorStartedPayload {
  serial: string;
  width: number;
  height: number;
}

interface MirrorInstance {
  serial: string;
  decoder: VideoDecoder;
  unlistenStopped: UnlistenFn;
  width: number;
  height: number;
  aspectRatio: number;
  screenWidth: number;
  canvas: HTMLCanvasElement;
  viewport: HTMLElement;
  gl: WebGL2RenderingContext;
  texture: WebGLTexture;
  win: HTMLElement; // .mirror-window DOM
}

const TOUCH_ACTION = { DOWN: 0, UP: 1, MOVE: 2 } as const;

const NAL_TYPE_IDR = 5;
const NAL_TYPE_SPS = 7;
const NAL_HEADER_OFFSET = 4;
const MAX_DECODE_QUEUE_SIZE = 3;
const MAX_MIRRORS = 4;
const DEFAULT_DEVICE_WIDTH = 1080;
const DEFAULT_DEVICE_HEIGHT = 2400;
const MIN_SCREEN_WIDTH = 240;
const MAX_SCREEN_WIDTH = 440;

// ─── 状态 ─────────────────────────────────────────────

const mirrors = new Map<string, MirrorInstance>();
let focusedSerial: string | null = null;
let zIndexCounter = 1000;

// ─── WebGL ─────────────────────────────────────────────

const VERTEX_SHADER_SRC = `#version 300 es
in vec2 a_position;
in vec2 a_texCoord;
out vec2 v_texCoord;
void main() {
  gl_Position = vec4(a_position, 0.0, 1.0);
  v_texCoord = vec2(a_texCoord.x, 1.0 - a_texCoord.y);
}`;

const FRAGMENT_SHADER_SRC = `#version 300 es
precision mediump float;
in vec2 v_texCoord;
uniform sampler2D u_texture;
out vec4 fragColor;
void main() {
  fragColor = texture(u_texture, v_texCoord);
}`;

function initWebGL(canvas: HTMLCanvasElement) {
  const gl = canvas.getContext('webgl2', {
    alpha: false,
    desynchronized: true,
    antialias: false,
    preserveDrawingBuffer: false,
  });
  if (!gl) throw new Error('WebGL2 不可用');

  const vs = compileShader(gl, gl.VERTEX_SHADER, VERTEX_SHADER_SRC);
  const fs = compileShader(gl, gl.FRAGMENT_SHADER, FRAGMENT_SHADER_SRC);
  const program = gl.createProgram()!;
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    throw new Error(`着色器链接失败: ${gl.getProgramInfoLog(program)}`);
  }
  gl.useProgram(program);

  const vertices = new Float32Array([-1, -1, 0, 0, 1, -1, 1, 0, -1, 1, 0, 1, 1, 1, 1, 1]);
  const vbo = gl.createBuffer()!;
  gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
  gl.bufferData(gl.ARRAY_BUFFER, vertices, gl.STATIC_DRAW);

  const aPos = gl.getAttribLocation(program, 'a_position');
  const aTex = gl.getAttribLocation(program, 'a_texCoord');
  gl.enableVertexAttribArray(aPos);
  gl.vertexAttribPointer(aPos, 2, gl.FLOAT, false, 16, 0);
  gl.enableVertexAttribArray(aTex);
  gl.vertexAttribPointer(aTex, 2, gl.FLOAT, false, 16, 8);

  const texture = gl.createTexture()!;
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);

  return { gl, texture };
}

function compileShader(gl: WebGL2RenderingContext, type: number, source: string) {
  const shader = gl.createShader(type)!;
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const info = gl.getShaderInfoLog(shader);
    gl.deleteShader(shader);
    throw new Error(`着色器编译失败: ${info}`);
  }
  return shader;
}

// ─── 悬浮窗 DOM ─────────────────────────────────────

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

function getMaxScreenWidth() {
  return clamp(Math.floor(window.innerWidth * 0.34), MIN_SCREEN_WIDTH + 24, MAX_SCREEN_WIDTH);
}

function getRecommendedScreenWidth(width: number, height: number) {
  const targetScreenHeight = clamp(window.innerHeight - 180, 620, 760);
  const recommended = Math.round(targetScreenHeight * (width / height));
  return clamp(recommended, MIN_SCREEN_WIDTH, getMaxScreenWidth());
}

function setMirrorWindowVars(
  win: HTMLElement,
  width: number,
  height: number,
  screenWidth: number,
  aspectRatio = height / width,
) {
  const screenHeight = Math.round(screenWidth * aspectRatio);
  win.style.setProperty('--mirror-device-width', String(width));
  win.style.setProperty('--mirror-device-height', String(height));
  win.style.setProperty('--mirror-screen-width', `${screenWidth}px`);
  win.style.setProperty('--mirror-screen-height', `${screenHeight}px`);
}

function getInitialMirrorMetrics(serial: string) {
  const cached = getCachedDeviceResolution(serial);
  const width = cached?.width ?? DEFAULT_DEVICE_WIDTH;
  const height = cached?.height ?? DEFAULT_DEVICE_HEIGHT;
  return {
    width,
    height,
    screenWidth: getRecommendedScreenWidth(width, height),
  };
}

function buildMirrorMarkup(serial: string) {
  const shortSerial = serial.length > 16 ? serial.substring(0, 16) + '…' : serial;

  return `
    <div class="mirror-showcase-unit">
      <div class="mirror-info-panel mirror-win-header">
        <div class="flex min-w-0 flex-col">
          <span class="text-[8px] font-bold leading-none tracking-[0.28em] text-slate-400 uppercase mb-1">Device ID</span>
          <span class="mirror-info-text" title="${esc(serial)}">${esc(shortSerial)}</span>
        </div>
        <div class="mirror-win-res">-- <span class="opacity-30">×</span> --</div>
        <button class="mirror-win-close" title="关闭投屏" aria-label="关闭投屏">
          <svg fill="none" width="16" height="16" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <line x1="18" y1="6" x2="6" y2="18"></line>
            <line x1="6" y1="6" x2="18" y2="18"></line>
          </svg>
        </button>
      </div>

      <div class="mirror-phone-shell">
        <div class="mirror-side-button mirror-power-btn"></div>
        <div class="mirror-side-button mirror-volume-up"></div>
        <div class="mirror-side-button mirror-volume-down"></div>
        <div class="mirror-phone-screen">
          <div class="mirror-dynamic-island"></div>
          <div class="mirror-screen-reflection"></div>
          <div class="mirror-device-viewport">
            <div class="mirror-stream-placeholder">
              <div class="mirror-stream-placeholder-core">
                <div class="mirror-stream-placeholder-badge">LIVE PREVIEW</div>
                <div class="mirror-stream-placeholder-title">建立投屏链路中</div>
                <div class="mirror-stream-placeholder-subtitle">正在等待设备首帧回传，连接完成后将平滑切入实时画面</div>
                <div class="mirror-stream-placeholder-grid">
                  <span></span>
                  <span></span>
                  <span></span>
                </div>
              </div>
            </div>
            <canvas class="mirror-win-canvas"></canvas>
          </div>
        </div>
      </div>

      <button class="mirror-resize-handle" title="拖拽缩放" aria-label="拖拽缩放投屏窗口"></button>
    </div>
  `;
}

function createMirrorWindow(
  serial: string,
  index: number,
  initialMetrics = getInitialMirrorMetrics(serial),
): { win: HTMLElement; canvas: HTMLCanvasElement; viewport: HTMLElement } {
  const win = document.createElement('div');
  win.className = 'mirror-window';
  win.dataset.serial = serial;

  // 梯级偏移定位（避免多窗口完全重叠）
  const baseX = 60;
  const baseY = 30;
  win.style.left = `${baseX + index * 50}px`;
  win.style.top = `${baseY + index * 50}px`;
  win.style.zIndex = String(++zIndexCounter);
  setMirrorWindowVars(win, initialMetrics.width, initialMetrics.height, initialMetrics.screenWidth);
  win.innerHTML = buildMirrorMarkup(serial);

  const canvas = win.querySelector('.mirror-win-canvas') as HTMLCanvasElement;
  const viewport = win.querySelector('.mirror-device-viewport') as HTMLElement;

  // 关闭按钮
  win.querySelector('.mirror-win-close')?.addEventListener('click', e => {
    e.stopPropagation();
    stopMirrorBySerial(serial);
  });

  // 点击窗口 → 焦点 + 置顶
  win.addEventListener('mousedown', () => {
    bringToFront(serial);
  });

  // 拖拽引擎（仅从 header 触发）
  bindDragEngine(win);

  return { win, canvas, viewport };
}

// ─── 拖拽引擎 ─────────────────────────────────────────

function bindDragEngine(win: HTMLElement) {
  const header = win.querySelector('.mirror-win-header') as HTMLElement;
  if (!header) return;

  let isDragging = false;
  let offsetX = 0;
  let offsetY = 0;

  header.addEventListener('mousedown', (e: MouseEvent) => {
    // 忽略按钮点击
    if ((e.target as HTMLElement).closest('button')) return;
    isDragging = true;
    offsetX = e.clientX - win.offsetLeft;
    offsetY = e.clientY - win.offsetTop;
    e.preventDefault();
  });

  // 事件绑到 document 以确保拖拽到窗口外也能正常追踪
  document.addEventListener('mousemove', (e: MouseEvent) => {
    if (!isDragging) return;
    const nextX = e.clientX - offsetX;
    const nextY = e.clientY - offsetY;
    const { left: newX, top: newY } = clampMirrorWindowPosition(win, nextX, nextY);
    win.style.left = `${newX}px`;
    win.style.top = `${newY}px`;
  });

  document.addEventListener('mouseup', () => {
    isDragging = false;
  });
}

function clampMirrorWindowPosition(win: HTMLElement, left: number, top: number) {
  const visibleGripWidth = 96;
  const visibleGripHeight = 88;
  const minLeft = Math.min(24, window.innerWidth - visibleGripWidth);
  const maxLeft = Math.max(minLeft, window.innerWidth - visibleGripWidth);
  const safeLeft = clamp(left, minLeft - win.offsetWidth, maxLeft);
  const safeTop = clamp(top, 12, Math.max(12, window.innerHeight - visibleGripHeight));

  return { left: safeLeft, top: safeTop };
}

function keepMirrorWindowReachable(win: HTMLElement) {
  const currentLeft = Number.parseFloat(win.style.left || '0');
  const currentTop = Number.parseFloat(win.style.top || '0');
  const { left, top } = clampMirrorWindowPosition(win, currentLeft, currentTop);
  win.style.left = `${left}px`;
  win.style.top = `${top}px`;
}

function applyMirrorMetrics(instance: MirrorInstance, width: number, height: number) {
  instance.width = width;
  instance.height = height;
  instance.aspectRatio = height / width;

  const nextScreenWidth = clamp(
    instance.screenWidth || getRecommendedScreenWidth(width, height),
    MIN_SCREEN_WIDTH,
    getMaxScreenWidth(),
  );

  instance.screenWidth = nextScreenWidth;
  setMirrorWindowVars(instance.win, width, height, nextScreenWidth, instance.aspectRatio);

  const statusEl = instance.win.querySelector('.mirror-win-status');
  if (statusEl) statusEl.textContent = '在线';

  const resEl = instance.win.querySelector('.mirror-win-res');
  if (resEl) resEl.textContent = `${width}×${height}`;

  keepMirrorWindowReachable(instance.win);
}

function bindResizeHandle(instance: MirrorInstance) {
  const handle = instance.win.querySelector('.mirror-resize-handle') as HTMLElement | null;
  if (!handle) return;

  let isResizing = false;
  let startX = 0;
  let startWidth = 0;

  handle.addEventListener('mousedown', (e: MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    bringToFront(instance.serial);
    isResizing = true;
    startX = e.clientX;
    startWidth = instance.screenWidth;
    document.body.classList.add('mirror-resizing');
  });

  document.addEventListener('mousemove', (e: MouseEvent) => {
    if (!isResizing) return;
    const deltaX = e.clientX - startX;
    const nextWidth = clamp(startWidth + deltaX, MIN_SCREEN_WIDTH, getMaxScreenWidth());
    instance.screenWidth = nextWidth;
    applyMirrorMetrics(instance, instance.width, instance.height);
  });

  document.addEventListener('mouseup', () => {
    if (!isResizing) return;
    isResizing = false;
    document.body.classList.remove('mirror-resizing');
  });
}

// ─── 焦点 & 层级管理 ──────────────────────────────────

function bringToFront(serial: string) {
  if (focusedSerial === serial) return;
  focusedSerial = serial;

  mirrors.forEach((inst, s) => {
    if (s === serial) {
      inst.win.classList.add('focused');
      inst.win.style.zIndex = String(++zIndexCounter);
    } else {
      inst.win.classList.remove('focused');
    }
  });

  const textInput = $('#mirror-text-input') as HTMLInputElement;
  if (textInput) textInput.focus();
}

// ─── 触控事件绑定 ─────────────────────────────────────

function bindTouchEvents(canvas: HTMLCanvasElement, serial: string) {
  let isDown = false;

  const getCoords = (e: MouseEvent): { x: number; y: number } | null => {
    const inst = mirrors.get(serial);
    if (!inst) return null;
    const rect = canvas.getBoundingClientRect();
    return {
      x: Math.round((e.clientX - rect.left) * (inst.width / rect.width)),
      y: Math.round((e.clientY - rect.top) * (inst.height / rect.height)),
    };
  };

  canvas.addEventListener('mousedown', e => {
    e.preventDefault();
    bringToFront(serial);
    const textInput = $('#mirror-text-input') as HTMLInputElement;
    if (textInput) textInput.focus();
    isDown = true;
    const c = getCoords(e);
    if (c)
      invoke('scrcpy_inject_touch', { serial, action: TOUCH_ACTION.DOWN, x: c.x, y: c.y }).catch(
        console.warn,
      );
  });

  let pendingMove: { x: number; y: number } | null = null;
  let rafId = 0;

  canvas.addEventListener('mousemove', e => {
    if (!isDown) return;
    const c = getCoords(e);
    if (!c || !mirrors.has(serial)) return;
    pendingMove = c;
    if (!rafId) {
      rafId = requestAnimationFrame(() => {
        rafId = 0;
        if (pendingMove && mirrors.has(serial)) {
          invoke('scrcpy_inject_touch', {
            serial,
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
    const c = getCoords(e);
    if (c && mirrors.has(serial))
      invoke('scrcpy_inject_touch', { serial, action: TOUCH_ACTION.UP, x: c.x, y: c.y }).catch(
        console.warn,
      );
  });

  canvas.addEventListener('mouseleave', () => {
    if (isDown && mirrors.has(serial)) {
      isDown = false;
      if (rafId) {
        cancelAnimationFrame(rafId);
        rafId = 0;
        pendingMove = null;
      }
      invoke('scrcpy_inject_touch', { serial, action: TOUCH_ACTION.UP, x: 0, y: 0 }).catch(
        console.warn,
      );
    }
  });
}

// ─── 公开 API ─────────────────────────────────────────

export async function startMirror(serial: string) {
  // 已在投屏 → 仅切焦点 + 置顶
  if (mirrors.has(serial)) {
    bringToFront(serial);
    return;
  }

  if (mirrors.size >= MAX_MIRRORS) {
    showToast(`最多支持 ${MAX_MIRRORS} 台设备同时投屏`, 'error');
    return;
  }

  const container = $('#mirror-windows-container') as HTMLElement;
  if (!container) return;

  const windowIndex = mirrors.size;
  const initialMetrics = getInitialMirrorMetrics(serial);
  const { win, canvas, viewport } = createMirrorWindow(serial, windowIndex, initialMetrics);
  container.appendChild(win);

  try {
    const { gl, texture } = initWebGL(canvas);
    let hasRenderedFirstFrame = false;

    const decoder = new VideoDecoder({
      output: (frame: VideoFrame) => {
        if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
          canvas.width = frame.displayWidth;
          canvas.height = frame.displayHeight;
          gl.viewport(0, 0, canvas.width, canvas.height);
        }
        gl.bindTexture(gl.TEXTURE_2D, texture);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, frame);
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
        frame.close();
        if (!hasRenderedFirstFrame) {
          hasRenderedFirstFrame = true;
          requestAnimationFrame(() => {
            win.classList.add('mirror-stream-ready');
          });
        }
      },
      error: (e: DOMException) => {
        console.error(`[mirror:${serial}] 解码错误:`, e);
      },
    });

    let lastTs = 0;
    let configData: Uint8Array | null = null;
    let concatBuf: Uint8Array | null = null;
    let isKeyFrameNeeded = true;

    const onFrame = new Channel<ArrayBuffer>();
    onFrame.onmessage = (raw: ArrayBuffer) => {
      if (raw.byteLength < 9) return;
      const bytes = new Uint8Array(raw);
      const view = new DataView(raw);
      const is_config = view.getUint8(0) === 1;
      const ts = Number(view.getBigUint64(1));
      const data = bytes.subarray(9);

      if (!is_config && ts < lastTs) return;
      lastTs = ts;
      if (data.length === 0) return;

      if (is_config) {
        configData = new Uint8Array(data);
        const codecStr = parseCodecFromSPS(configData);
        try {
          decoder.configure({ codec: codecStr, optimizeForLatency: true });
          isKeyFrameNeeded = true;
        } catch (e) {
          console.error(`[mirror:${serial}] 解码器配置失败:`, e);
        }
        return;
      }

      if (decoder.state !== 'configured') return;

      const nalType = data[NAL_HEADER_OFFSET] & 0x1f;
      const isKey = nalType === NAL_TYPE_IDR;
      if (isKeyFrameNeeded && !isKey) return;
      isKeyFrameNeeded = false;

      if (!isKey && decoder.decodeQueueSize > MAX_DECODE_QUEUE_SIZE) {
        isKeyFrameNeeded = true;
        return;
      }

      let frameData: Uint8Array = data;
      if (isKey && configData) {
        const needed = configData.length + data.length;
        if (!concatBuf || concatBuf.length < needed) {
          concatBuf = new Uint8Array(needed + 1024);
        }
        concatBuf.set(configData, 0);
        concatBuf.set(data, configData.length);
        frameData = concatBuf.subarray(0, needed);
      }

      decoder.decode(
        new EncodedVideoChunk({
          type: isKey ? 'key' : 'delta',
          timestamp: ts * 1000,
          data: frameData,
        }),
      );
    };

    const result: MirrorStartedPayload = await invoke('scrcpy_start_mirror', { serial, onFrame });

    const unlistenStopped = await listen<string>('scrcpy-stopped', event => {
      if (event.payload === serial && mirrors.has(serial)) {
        cleanupInstance(serial);
        showToast(`${serial} 投屏已断开`, 'info');
      }
    });

    const instance: MirrorInstance = {
      serial,
      decoder,
      unlistenStopped,
      width: result.width,
      height: result.height,
      aspectRatio: result.height / result.width,
      screenWidth: initialMetrics.screenWidth,
      canvas,
      viewport,
      gl,
      texture,
      win,
    };

    applyMirrorMetrics(instance, result.width, result.height);
    canvas.width = result.width;
    canvas.height = result.height;
    gl.viewport(0, 0, result.width, result.height);

    mirrors.set(serial, instance);
    bindResizeHandle(instance);
    bindTouchEvents(canvas, serial);
    bringToFront(serial);
  } catch (e) {
    const errMsg = String(e);
    console.error('[mirror] 启动失败:', errMsg);
    win.remove();

    // 如果后端报"已在投屏中"→ 强制释放后端并自动重试一次
    if (errMsg.includes('已在投屏')) {
      console.warn(`[mirror] 检测到残留会话，强制释放 ${serial}...`);
      try {
        await invoke('scrcpy_stop_mirror', { serial });
      } catch {
        /* ignore */
      }
      // 短暂等待后端完全释放
      await new Promise(r => setTimeout(r, 300));
      showToast('正在重新连接…', 'info');
      // 递归重试（仅一次，因为此时后端已释放）
      return startMirror(serial);
    }

    showToast(`投屏连接失败: ${e}`, 'error');
  }
}

export async function stopMirrorBySerial(serial: string) {
  const inst = mirrors.get(serial);
  if (!inst) {
    // 前端已清理但后端可能还在 → 强制通知后端释放
    try {
      await invoke('scrcpy_stop_mirror', { serial });
    } catch {
      /* ignore */
    }
    return;
  }

  // 关键：先通知后端释放，再清理前端
  try {
    await invoke('scrcpy_stop_mirror', { serial });
  } catch (e) {
    console.warn('[mirror] 后端停止失败:', e);
  }

  // 后端已释放，安全清理前端
  cleanupInstance(serial);
}

export async function stopMirror() {
  if (focusedSerial) await stopMirrorBySerial(focusedSerial);
}

export async function stopAllMirrors() {
  const serials = [...mirrors.keys()];
  // 先全部通知后端停止
  await Promise.allSettled(
    serials.map(s => invoke('scrcpy_stop_mirror', { serial: s }).catch(() => {})),
  );
  // 再统一清理前端
  for (const s of serials) cleanupInstance(s);
}

// ─── 内部清理 ─────────────────────────────────────────

function cleanupInstance(serial: string) {
  const inst = mirrors.get(serial);
  if (!inst) return;
  mirrors.delete(serial);
  inst.unlistenStopped();
  try {
    inst.decoder.close();
  } catch {
    /* ignore */
  }
  inst.win.remove();

  if (focusedSerial === serial) {
    const next = mirrors.keys().next().value as string | undefined;
    if (next) {
      bringToFront(next);
    } else {
      focusedSerial = null;
    }
  }
}

// ─── 初始化（键盘事件路由） ─────────────────────────────

export function initMirror() {
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
    AudioVolumeUp: 24,
    AudioVolumeDown: 25,
  };

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

  const MODIFIER_KEYS = new Set(['Control', 'Shift', 'Alt', 'Meta', 'CapsLock']);

  function charToKeycode(key: string): number {
    const c = key.toUpperCase();
    if (c >= 'A' && c <= 'Z') return c.charCodeAt(0) - 65 + 29;
    if (c >= '0' && c <= '9') return c.charCodeAt(0) - 48 + 7;
    return 0;
  }

  const textInput = $('#mirror-text-input') as HTMLInputElement;
  if (!textInput) return;

  let isComposing = false;

  textInput.addEventListener('keydown', e => {
    if (!focusedSerial) return;
    if (MODIFIER_KEYS.has(e.key)) return;

    if (KEY_MAP[e.key] && !isComposing) {
      e.preventDefault();
      invoke('scrcpy_inject_key', {
        serial: focusedSerial,
        keycode: KEY_MAP[e.key],
        metaState: getMetaState(e),
      }).catch(err => console.error('[mirror] inject_key 失败:', err));
      return;
    }

    if ((e.ctrlKey || e.metaKey) && e.key.length === 1) {
      if (e.key === 'v' || e.key === 'V') return;
      e.preventDefault();
      const keycode = charToKeycode(e.key);
      if (keycode === 0) return;
      invoke('scrcpy_inject_key', {
        serial: focusedSerial,
        keycode,
        metaState: getMetaState(e),
      }).catch(err => console.error('[mirror] inject_key 失败:', err));
    }
  });

  textInput.addEventListener('compositionstart', () => {
    isComposing = true;
  });

  textInput.addEventListener('compositionend', () => {
    isComposing = false;
    const text = textInput.value;
    if (text && focusedSerial) {
      invoke('scrcpy_inject_text', { serial: focusedSerial, text }).catch(console.error);
    }
    textInput.value = '';
  });

  textInput.addEventListener('input', () => {
    if (isComposing) return;
    const text = textInput.value;
    if (text && focusedSerial) {
      invoke('scrcpy_inject_text', { serial: focusedSerial, text }).catch(console.error);
    }
    textInput.value = '';
  });

  textInput.addEventListener('blur', () => {
    setTimeout(() => {
      if (focusedSerial && mirrors.size > 0) textInput.focus();
    }, 50);
  });
}

// ─── 工具函数 ─────────────────────────────────────────

function parseCodecFromSPS(configData: Uint8Array): string {
  for (let i = 0; i < configData.length - 4; i++) {
    let nalStart = -1;
    if (configData[i] === 0 && configData[i + 1] === 0) {
      if (configData[i + 2] === 0 && configData[i + 3] === 1) nalStart = i + 4;
      else if (configData[i + 2] === 1) nalStart = i + 3;
    }
    if (nalStart >= 0 && nalStart < configData.length) {
      const nalType = configData[nalStart] & 0x1f;
      if (nalType === NAL_TYPE_SPS && nalStart + 3 < configData.length) {
        const profile = configData[nalStart + 1];
        const constraints = configData[nalStart + 2];
        const level = configData[nalStart + 3];
        const codec = `avc1.${hex(profile)}${hex(constraints)}${hex(level)}`;
        console.log(`[mirror] 解析 codec: ${codec}`);
        return codec;
      }
    }
  }
  console.warn('[mirror] 未找到 SPS，使用默认 codec');
  return 'avc1.42001f';
}

function hex(n: number): string {
  return n.toString(16).padStart(2, '0');
}
