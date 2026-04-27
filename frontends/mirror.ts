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

import { getCachedDevice, getCachedDeviceResolution } from './devices';
import { $, esc, showToast } from './utils';

// ─── 类型 ─────────────────────────────────────────────

interface MirrorStartedPayload {
  serial: string;
  width: number;
  height: number;
}

interface ScrcpyClipboardPayload {
  serial: string;
  text: string;
}

type MirrorStreamState =
  | 'starting'
  | 'streaming'
  | 'degraded'
  | 'recovering'
  | 'stalled'
  | 'stopped';

interface ScrcpySessionStatePayload {
  serial: string;
  phase: MirrorStreamState;
  width: number;
  height: number;
  recover_reason?: string | null;
  last_frame_at_ms?: number | null;
}

interface ScrcpyTextRoutePayload {
  serial: string;
  route: 'ascii_direct' | 'adb_ime_text' | 'clipboard_fallback';
}

interface MirrorInstance {
  serial: string;
  decoder: VideoDecoder;
  frameChannel: Channel<ArrayBuffer>;
  unlistenStopped: UnlistenFn;
  width: number;
  height: number;
  aspectRatio: number;
  screenWidth: number;
  canvas: HTMLCanvasElement;
  viewport: HTMLElement;
  gl: WebGL2RenderingContext;
  program: WebGLProgram;
  vertexBuffer: WebGLBuffer;
  texture: WebGLTexture;
  win: HTMLElement; // .mirror-window DOM
  streamState: MirrorStreamState;
  textRoute: ScrcpyTextRoutePayload['route'] | null;
  lastFrameAtMs: number | null;
  resetTimerId: number | null;
  cleanupFns: Array<() => void>;
  scheduleVideoReset: (reason: string) => void;
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
const WHEEL_SCROLL_MAX = 16;
const WHEEL_DELTA_DIVISOR = 54;
const DEFAULT_CONCAT_BUFFER_BYTES = 128 * 1024;
const MAX_CONCAT_BUFFER_RETAIN_BYTES = 512 * 1024;

// ─── 状态 ─────────────────────────────────────────────

const mirrors = new Map<string, MirrorInstance>();
let focusedSerial: string | null = null;
let zIndexCounter = 1000;
let clipboardListenBound = false;
let sessionStateListenBound = false;
let textRouteListenBound = false;
let inputBindingsBound = false;

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

  gl.deleteShader(vs);
  gl.deleteShader(fs);

  return { gl, program, vertexBuffer: vbo, texture };
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

function buildMirrorMarkup(serial: string, displayId: string) {
  return `
    <div class="mirror-showcase-unit">
      <div class="mirror-info-panel mirror-win-header">
        <div class="mirror-info-main">
          <div class="mirror-info-icon">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
              <rect x="7" y="2.5" width="10" height="19" rx="2.6" stroke="currentColor" stroke-width="1.7"></rect>
              <circle cx="12" cy="18" r="0.9" fill="currentColor"></circle>
            </svg>
          </div>
          <div class="mirror-info-id-block">
            <span class="mirror-info-kicker">Device ID</span>
            <span class="mirror-info-text" title="${esc(displayId)}" data-serial="${esc(serial)}">${esc(displayId)}</span>
          </div>
        </div>
        <div class="mirror-info-actions">
          <div class="mirror-win-res">-- <span class="opacity-30">×</span> --</div>
          <button type="button" class="mirror-win-close" title="关闭投屏" aria-label="关闭投屏">
            <svg fill="none" width="16" height="16" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <line x1="18" y1="6" x2="6" y2="18"></line>
              <line x1="6" y1="6" x2="18" y2="18"></line>
            </svg>
          </button>
        </div>
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

function scheduleMirrorVideoReset(serial: string, reason: string) {
  const inst = mirrors.get(serial);
  if (!inst) return;
  inst.scheduleVideoReset(reason);
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
  const displayId = getCachedDevice(serial)?.hw_serial?.trim() || serial;
  win.innerHTML = buildMirrorMarkup(serial, displayId);

  const canvas = win.querySelector('.mirror-win-canvas') as HTMLCanvasElement;
  const viewport = win.querySelector('.mirror-device-viewport') as HTMLElement;
  const mirrorHost = win as HTMLElement & {
    __mirrorOnWindowMouseDown?: (event: MouseEvent) => void;
    __mirrorOnCloseClick?: (event: MouseEvent) => void;
  };

  // 关闭按钮
  const closeBtn = win.querySelector('.mirror-win-close') as HTMLElement | null;
  const handleClose = (e: Event) => {
    e.stopPropagation();
    e.preventDefault();
    stopMirrorBySerial(serial);
  };
  mirrorHost.__mirrorOnCloseClick = handleClose as (event: MouseEvent) => void;
  closeBtn?.addEventListener('click', handleClose);
  closeBtn?.addEventListener('pointerdown', handleClose);

  // 点击窗口 → 焦点 + 置顶
  mirrorHost.__mirrorOnWindowMouseDown = () => {
    bringToFront(serial);
  };
  win.addEventListener('mousedown', mirrorHost.__mirrorOnWindowMouseDown);

  return { win, canvas, viewport };
}

// ─── 拖拽引擎 ─────────────────────────────────────────

function bindDragEngine(instance: MirrorInstance) {
  const win = instance.win;
  const header = win.querySelector('.mirror-win-header') as HTMLElement;
  if (!header) return;

  let isDragging = false;
  let offsetX = 0;
  let offsetY = 0;

  const onHeaderMouseDown = (e: MouseEvent) => {
    // 忽略按钮点击
    if ((e.target as HTMLElement).closest('button')) return;
    isDragging = true;
    offsetX = e.clientX - win.offsetLeft;
    offsetY = e.clientY - win.offsetTop;
    e.preventDefault();
  };

  // 事件绑到 document 以确保拖拽到窗口外也能正常追踪
  const onDocumentMouseMove = (e: MouseEvent) => {
    if (!isDragging) return;
    const nextX = e.clientX - offsetX;
    const nextY = e.clientY - offsetY;
    const { left: newX, top: newY } = clampMirrorWindowPosition(win, nextX, nextY);
    win.style.left = `${newX}px`;
    win.style.top = `${newY}px`;
  };

  const onDocumentMouseUp = () => {
    isDragging = false;
  };

  header.addEventListener('mousedown', onHeaderMouseDown);
  document.addEventListener('mousemove', onDocumentMouseMove);
  document.addEventListener('mouseup', onDocumentMouseUp);
  instance.cleanupFns.push(() => {
    header.removeEventListener('mousedown', onHeaderMouseDown);
    document.removeEventListener('mousemove', onDocumentMouseMove);
    document.removeEventListener('mouseup', onDocumentMouseUp);
  });
}

function clampMirrorWindowPosition(win: HTMLElement, left: number, top: number) {
  const winW = win.offsetWidth || 280;
  const winH = win.offsetHeight || 400;
  const pad = 4;
  const safeLeft = clamp(left, pad, Math.max(pad, window.innerWidth - winW - pad));
  const safeTop = clamp(top, pad, Math.max(pad, window.innerHeight - winH - pad));

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

  const resEl = instance.win.querySelector('.mirror-win-res');
  if (resEl) resEl.textContent = `${width}×${height}`;

  keepMirrorWindowReachable(instance.win);
}

function getStreamStateMeta(phase: MirrorStreamState, reason?: string | null) {
  switch (phase) {
    case 'starting':
      return {
        title: '正在建立投屏链路',
        subtitle: '正在等待设备首帧回传，连接完成后将平滑切入实时画面',
      };
    case 'streaming':
      return {
        title: '实时画面已连接',
        subtitle: '当前视频链路稳定，可直接进行触控、键盘和滚轮操作',
      };
    case 'recovering':
      return {
        title: '正在恢复视频流',
        subtitle: reason ? `恢复原因：${reason}` : '输入法或视频链路变化后，正在请求新的关键帧',
      };
    case 'stalled':
      return {
        title: '画面暂时停滞',
        subtitle: reason ? `检测到异常：${reason}` : '等待自动恢复或重新请求视频流',
      };
    case 'degraded':
      return {
        title: '当前处于降级运行',
        subtitle: '已进入保守恢复模式，尽量维持会话可用',
      };
    case 'stopped':
      return {
        title: '投屏已停止',
        subtitle: '当前会话已关闭，可重新打开投屏窗口',
      };
  }
}

function setMirrorStreamState(
  instance: MirrorInstance,
  phase: MirrorStreamState,
  reason?: string | null,
  lastFrameAtMs?: number | null,
) {
  instance.streamState = phase;
  if (lastFrameAtMs !== undefined) {
    instance.lastFrameAtMs = lastFrameAtMs ?? null;
  }

  instance.win.dataset.streamState = phase;
  const meta = getStreamStateMeta(phase, reason);
  // DEAD-1 修复：移除无效的 statusEl.className 赋值（meta.className 全为 ''）

  const titleEl = instance.win.querySelector('.mirror-stream-placeholder-title');
  if (titleEl) titleEl.textContent = meta.title;
  const subtitleEl = instance.win.querySelector('.mirror-stream-placeholder-subtitle');
  if (subtitleEl) subtitleEl.textContent = meta.subtitle;
}

function setMirrorTextRoute(
  instance: MirrorInstance,
  route: ScrcpyTextRoutePayload['route'] | null,
) {
  instance.textRoute = route;
  const idEl = instance.win.querySelector('.mirror-info-text') as HTMLElement | null;
  if (!idEl) return;

  const routeTitleMap: Record<NonNullable<MirrorInstance['textRoute']>, string> = {
    ascii_direct: '当前短文本通过直发通道输入',
    adb_ime_text: '当前中文/长文本通过 ADB 输入法桥接',
    clipboard_fallback: '当前文本走剪贴板兜底路径',
  };

  const rawTitle = idEl.dataset.rawTitle || idEl.title || idEl.textContent || '';
  idEl.dataset.rawTitle = rawTitle;
  idEl.title = route ? `${rawTitle}\n${routeTitleMap[route]}` : rawTitle;
}

function bindResizeHandle(instance: MirrorInstance) {
  const handle = instance.win.querySelector('.mirror-resize-handle') as HTMLElement | null;
  if (!handle) return;

  let isResizing = false;
  let startX = 0;
  let startWidth = 0;

  const onHandleMouseDown = (e: MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    bringToFront(instance.serial);
    isResizing = true;
    startX = e.clientX;
    startWidth = instance.screenWidth;
    document.body.classList.add('mirror-resizing');
  };

  const onDocumentMouseMove = (e: MouseEvent) => {
    if (!isResizing) return;
    const deltaX = e.clientX - startX;
    const nextWidth = clamp(startWidth + deltaX, MIN_SCREEN_WIDTH, getMaxScreenWidth());
    instance.screenWidth = nextWidth;
    applyMirrorMetrics(instance, instance.width, instance.height);
  };

  const onDocumentMouseUp = () => {
    if (!isResizing) return;
    isResizing = false;
    document.body.classList.remove('mirror-resizing');
  };

  handle.addEventListener('mousedown', onHandleMouseDown);
  document.addEventListener('mousemove', onDocumentMouseMove);
  document.addEventListener('mouseup', onDocumentMouseUp);
  instance.cleanupFns.push(() => {
    handle.removeEventListener('mousedown', onHandleMouseDown);
    document.removeEventListener('mousemove', onDocumentMouseMove);
    document.removeEventListener('mouseup', onDocumentMouseUp);
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

function bindTouchEvents(instance: MirrorInstance) {
  const { canvas, serial } = instance;
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

  const normalizeWheelDelta = (delta: number, mode: number) => {
    const factor =
      mode === WheelEvent.DOM_DELTA_LINE ? 16 : mode === WheelEvent.DOM_DELTA_PAGE ? 120 : 1;
    return clamp((delta / WHEEL_DELTA_DIVISOR) * factor, -WHEEL_SCROLL_MAX, WHEEL_SCROLL_MAX);
  };

  const onCanvasMouseDown = (e: MouseEvent) => {
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
  };

  let pendingMove: { x: number; y: number } | null = null;
  let rafId = 0;

  const onCanvasMouseMove = (e: MouseEvent) => {
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
  };

  const onCanvasMouseUp = (e: MouseEvent) => {
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
  };

  const onCanvasMouseLeave = () => {
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
  };

  let pendingWheel: {
    x: number;
    y: number;
    hScroll: number;
    vScroll: number;
  } | null = null;
  let wheelRaf = 0;

  const onCanvasWheel = (e: WheelEvent) => {
    if (!mirrors.has(serial)) return;
    e.preventDefault();
    bringToFront(serial);
    const textInput = $('#mirror-text-input') as HTMLInputElement;
    if (textInput) textInput.focus();
    const c = getCoords(e);
    if (!c) return;

    const hDelta = normalizeWheelDelta(e.deltaX, e.deltaMode);
    const vDelta = normalizeWheelDelta(e.deltaY, e.deltaMode);
    if (hDelta === 0 && vDelta === 0) return;

    pendingWheel = {
      x: c.x,
      y: c.y,
      hScroll: clamp((pendingWheel?.hScroll ?? 0) + hDelta, -WHEEL_SCROLL_MAX, WHEEL_SCROLL_MAX),
      vScroll: clamp((pendingWheel?.vScroll ?? 0) + vDelta, -WHEEL_SCROLL_MAX, WHEEL_SCROLL_MAX),
    };

    if (wheelRaf) return;
    wheelRaf = requestAnimationFrame(() => {
      wheelRaf = 0;
      const next = pendingWheel;
      pendingWheel = null;
      if (!next || !mirrors.has(serial)) return;
      invoke('scrcpy_inject_scroll', {
        serial,
        x: next.x,
        y: next.y,
        hScroll: next.hScroll,
        vScroll: next.vScroll,
        buttons: 0,
      }).catch(console.warn);
    });
  };

  canvas.addEventListener('mousedown', onCanvasMouseDown);
  canvas.addEventListener('mousemove', onCanvasMouseMove);
  canvas.addEventListener('mouseup', onCanvasMouseUp);
  canvas.addEventListener('mouseleave', onCanvasMouseLeave);
  canvas.addEventListener('wheel', onCanvasWheel, { passive: false });
  instance.cleanupFns.push(() => {
    canvas.removeEventListener('mousedown', onCanvasMouseDown);
    canvas.removeEventListener('mousemove', onCanvasMouseMove);
    canvas.removeEventListener('mouseup', onCanvasMouseUp);
    canvas.removeEventListener('mouseleave', onCanvasMouseLeave);
    canvas.removeEventListener('wheel', onCanvasWheel);
    if (rafId) {
      cancelAnimationFrame(rafId);
      rafId = 0;
    }
    if (wheelRaf) {
      cancelAnimationFrame(wheelRaf);
      wheelRaf = 0;
    }
    pendingMove = null;
    pendingWheel = null;
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
    const { gl, program, vertexBuffer, texture } = initWebGL(canvas);
    let hasRenderedFirstFrame = false;
    // resetTimer 局部变量已移除 — timer 统一由 instance.resetTimerId 管理（见 MEM-L1 修复）

    const scheduleVideoReset = (reason: string) => {
      const live = mirrors.get(serial);
      if (!live) return;

      // MEM-L1 修复：统一用 live.resetTimerId 单个字段管理 timer，
      // 原来同时维护局部 resetTimer 和实例 resetTimerId 两个变量，
      // clearTimeout 时只清了局部变量，实例字段的 timer 仍在运行，
      // 两者可能并发触发同一 serial 的 reset，造成重复 invoke 和僵尸 timer。
      setMirrorStreamState(live, 'recovering', reason);
      if (live.resetTimerId) {
        window.clearTimeout(live.resetTimerId);
        live.resetTimerId = null;
      }
      live.resetTimerId = window.setTimeout(() => {
        live.resetTimerId = null;
        if (!mirrors.has(serial)) return;
        console.warn(`[mirror:${serial}] 请求重置视频流: ${reason}`);
        invoke('scrcpy_reset_video', { serial, reason }).catch(err =>
          console.error(`[mirror:${serial}] reset_video 失败:`, err),
        );
      }, 120);
    };

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
          const live = mirrors.get(serial);
          if (live) {
            setMirrorStreamState(live, 'streaming');
          }
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
        concatBuf = null;
        const codecStr = parseCodecFromSPS(configData);
        try {
          if (decoder.state !== 'closed') {
            decoder.reset();
          }
          decoder.configure({ codec: codecStr, optimizeForLatency: true });
          isKeyFrameNeeded = true;
        } catch (e) {
          console.error(`[mirror:${serial}] 解码器配置失败:`, e);
          scheduleVideoReset('decoder-config-failed');
        }
        return;
      }

      if (decoder.state !== 'configured') return;

      const nalType = data[NAL_HEADER_OFFSET] & 0x1f;
      const isKey = nalType === NAL_TYPE_IDR;
      if (isKeyFrameNeeded && !isKey) return;
      isKeyFrameNeeded = false;

      if (!isKey && decoder.decodeQueueSize > MAX_DECODE_QUEUE_SIZE) {
        try {
          decoder.reset();
          if (configData) {
            decoder.configure({ codec: parseCodecFromSPS(configData), optimizeForLatency: true });
          }
        } catch (e) {
          console.warn(`[mirror:${serial}] 解码器压力重置失败:`, e);
        }
        isKeyFrameNeeded = true;
        scheduleVideoReset('decode-queue-overflow');
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

      try {
        decoder.decode(
          new EncodedVideoChunk({
            type: isKey ? 'key' : 'delta',
            timestamp: ts * 1000,
            data: frameData,
          }),
        );
        if (concatBuf && concatBuf.length > MAX_CONCAT_BUFFER_RETAIN_BYTES) {
          const retainSize = Math.max(DEFAULT_CONCAT_BUFFER_BYTES, frameData.byteLength + 1024);
          if (retainSize < concatBuf.length) {
            concatBuf = new Uint8Array(retainSize);
          }
        }
      } catch (e) {
        console.error(`[mirror:${serial}] decode 调用失败:`, e);
        isKeyFrameNeeded = true;
        scheduleVideoReset('decode-call-failed');
      }
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
      frameChannel: onFrame,
      unlistenStopped,
      width: result.width,
      height: result.height,
      aspectRatio: result.height / result.width,
      screenWidth: initialMetrics.screenWidth,
      canvas,
      viewport,
      gl,
      program,
      vertexBuffer,
      texture,
      win,
      streamState: 'starting',
      textRoute: null,
      lastFrameAtMs: null,
      resetTimerId: null,
      cleanupFns: [],
      scheduleVideoReset,
    };

    const mirrorHost = win as HTMLElement & {
      __mirrorOnWindowMouseDown?: (event: MouseEvent) => void;
      __mirrorOnCloseClick?: (event: MouseEvent) => void;
    };
    const closeBtn = win.querySelector('.mirror-win-close') as HTMLElement | null;
    if (mirrorHost.__mirrorOnWindowMouseDown) {
      instance.cleanupFns.push(() => {
        win.removeEventListener('mousedown', mirrorHost.__mirrorOnWindowMouseDown!);
        delete mirrorHost.__mirrorOnWindowMouseDown;
      });
    }
    if (closeBtn && mirrorHost.__mirrorOnCloseClick) {
      instance.cleanupFns.push(() => {
        closeBtn.removeEventListener('click', mirrorHost.__mirrorOnCloseClick!);
        closeBtn.removeEventListener('pointerdown', mirrorHost.__mirrorOnCloseClick!);
        delete mirrorHost.__mirrorOnCloseClick;
      });
    }

    applyMirrorMetrics(instance, result.width, result.height);
    setMirrorStreamState(instance, 'starting');
    setMirrorTextRoute(instance, null);
    canvas.width = result.width;
    canvas.height = result.height;
    gl.viewport(0, 0, result.width, result.height);

    mirrors.set(serial, instance);
    bindDragEngine(instance);
    bindResizeHandle(instance);
    bindTouchEvents(instance);
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
  if (inst.resetTimerId) {
    window.clearTimeout(inst.resetTimerId);
    inst.resetTimerId = null;
  }
  for (const cleanup of inst.cleanupFns.splice(0)) {
    try {
      cleanup();
    } catch {
      /* ignore */
    }
  }
  try {
    inst.frameChannel.onmessage = () => {};
  } catch {
    /* ignore */
  }
  try {
    inst.decoder.close();
  } catch {
    /* ignore */
  }
  try {
    inst.gl.deleteProgram(inst.program);
    inst.gl.deleteBuffer(inst.vertexBuffer);
    inst.gl.deleteTexture(inst.texture);
    const loseContext = inst.gl.getExtension('WEBGL_lose_context');
    loseContext?.loseContext();
  } catch {
    /* ignore */
  }
  inst.canvas.width = 0;
  inst.canvas.height = 0;
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
  if (inputBindingsBound) return;
  inputBindingsBound = true;

  let isComposing = false;

  async function forwardClipboardText(serial: string, providedText?: string) {
    let text = providedText ?? '';
    if (!text) {
      try {
        text = await navigator.clipboard.readText();
      } catch (error) {
        console.warn('[mirror] 读取系统剪贴板失败:', error);
      }
    }
    if (!text) return;
    invoke('scrcpy_inject_text', { serial, text })
      .catch(err => {
        console.error('[mirror] inject_text 失败:', err);
        showToast(`粘贴到设备失败：${String(err)}`, 'error');
      })
      .finally(() => {
        scheduleMirrorVideoReset(serial, 'clipboard-paste');
      });
  }

  async function writeSystemClipboard(text: string) {
    if (!text) return;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
        return;
      }
    } catch (error) {
      console.warn('[mirror] 写入系统剪贴板失败，尝试降级方案:', error);
    }

    const textarea = document.createElement('textarea');
    textarea.value = text;
    textarea.setAttribute('readonly', 'true');
    textarea.style.position = 'fixed';
    textarea.style.opacity = '0';
    textarea.style.pointerEvents = 'none';
    document.body.appendChild(textarea);
    textarea.select();
    try {
      document.execCommand('copy');
    } finally {
      textarea.remove();
    }
  }

  if (!clipboardListenBound) {
    clipboardListenBound = true;
    listen<ScrcpyClipboardPayload>('scrcpy-clipboard', event => {
      const payload = event.payload;
      if (!payload?.serial || !mirrors.has(payload.serial)) return;
      void writeSystemClipboard(payload.text);
    }).catch(err => console.error('[mirror] 监听 scrcpy-clipboard 失败:', err));
  }

  if (!sessionStateListenBound) {
    sessionStateListenBound = true;
    listen<ScrcpySessionStatePayload>('scrcpy-session-state', event => {
      const payload = event.payload;
      const inst = mirrors.get(payload?.serial ?? '');
      if (!inst) return;

      if (payload.width > 0 && payload.height > 0) {
        applyMirrorMetrics(inst, payload.width, payload.height);
      }
      setMirrorStreamState(
        inst,
        payload.phase,
        payload.recover_reason ?? null,
        payload.last_frame_at_ms ?? null,
      );
    }).catch(err => console.error('[mirror] 监听 scrcpy-session-state 失败:', err));
  }

  if (!textRouteListenBound) {
    textRouteListenBound = true;
    listen<ScrcpyTextRoutePayload>('scrcpy-text-route', event => {
      const payload = event.payload;
      const inst = mirrors.get(payload?.serial ?? '');
      if (!inst) return;
      setMirrorTextRoute(inst, payload.route);
    }).catch(err => console.error('[mirror] 监听 scrcpy-text-route 失败:', err));
  }

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
      if (e.key === 'v' || e.key === 'V') {
        e.preventDefault();
        void forwardClipboardText(focusedSerial);
        return;
      }
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
      const serial = focusedSerial;
      invoke('scrcpy_inject_text', { serial, text })
        .catch(console.error)
        .finally(() => scheduleMirrorVideoReset(serial, 'compositionend-input'));
    }
    textInput.value = '';
  });

  textInput.addEventListener('input', () => {
    if (isComposing) return;
    const text = textInput.value;
    if (text && focusedSerial) {
      const serial = focusedSerial;
      invoke('scrcpy_inject_text', { serial, text })
        .catch(console.error)
        .finally(() => scheduleMirrorVideoReset(serial, 'input-event'));
    }
    textInput.value = '';
  });

  textInput.addEventListener('paste', e => {
    if (!focusedSerial) return;
    const text = e.clipboardData?.getData('text/plain') ?? '';
    if (!text) return;
    e.preventDefault();
    void forwardClipboardText(focusedSerial, text);
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
        return `avc1.${hex(profile)}${hex(constraints)}${hex(level)}`;
      }
    }
  }
  console.warn('[mirror] 未找到 SPS，使用默认 codec');
  return 'avc1.42001f';
}

function hex(n: number): string {
  return n.toString(16).padStart(2, '0');
}
