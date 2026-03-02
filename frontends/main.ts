/**
 * AutomateX — Application Entry Point
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { platform } from '@tauri-apps/plugin-os';
import { DeviceState } from './constants';
import { setActiveTask, setActiveCityIdx, globalQueue, activeTask, selectedDevice } from './state';
import { $, showToast } from './utils';
import {
    refreshDevices,
    filterDeviceCards,
    setDeviceCallbacks,
    updateCardSelection,
} from './devices';
import {
    renderTaskView,
    registerViewActions,
    loadTasksForDevice,
    setTaskViewCallbacks,
} from './task-view';
import { loadChainForDevice } from './queue';
import { registerTaskActions, setRefreshCallbacks, initEngine } from './task-engine';
import {
    showAddDeviceDialog,
    hideAddDeviceDialog,
    submitAddDevice,
    removeSelectedDevice,
    unflagSelectedDevice,
    showDeviceInfo,
} from './dialogs';
import { initSettings, updateMqttStatusUI } from './settings';

/* ===== Theme Toggle ===== */

/** 同步 Tauri 窗口背景色与当前主题 */
function syncWindowBg() {
    try {
        const isDark = document.documentElement.classList.contains('dark');
        const color = isDark ? '#0f1117' : '#f8fafc';
        // 使用 Tauri webview 的 setBackgroundColor API（如果可用）
        const win = getCurrentWindow();
        // Tauri v2 支持 RGBA 格式
        win.setBackgroundColor(color).catch(() => {});
    } catch {
        // 非 Tauri 环境忽略
    }
}

function initTheme() {
    const saved = localStorage.getItem('theme');
    // 默认暗色，仅用户明确选择 light 时才用亮色
    if (saved !== 'light') {
        document.documentElement.classList.add('dark');
    }
    updateThemeIcon();
    // 注意：此处不调用 syncWindowBg()，开屏期间保持 tauri.conf.json 中的 backgroundColor
    // 等 splash 退场后再同步，避免底色闪变
}

function toggleTheme() {
    const isDark = document.documentElement.classList.toggle('dark');
    localStorage.setItem('theme', isDark ? 'dark' : 'light');
    updateThemeIcon();
    syncWindowBg();
}

function updateThemeIcon() {
    const icon = document.getElementById('theme-icon');
    if (icon) {
        icon.textContent = document.documentElement.classList.contains('dark')
            ? 'light_mode'
            : 'dark_mode';
    }
}

/* ===== Splash Screen ===== */

function splash() {
    const el = $('#splash')!;
    const app = $('#app')!;
    const timeEl = el.querySelector('.splash-time');
    if (timeEl) {
        const now = new Date();
        const hh = String(now.getHours()).padStart(2, '0');
        const mm = String(now.getMinutes()).padStart(2, '0');
        timeEl.textContent = `${hh}:${mm}`;
    }
    // 入场动画
    setTimeout(() => el.classList.add('loaded'), 300);
    // 退场 + 显示主应用
    setTimeout(() => {
        el.classList.add('out');
        app.classList.add('show');
        // 开屏退场后再同步窗口背景色（避免底色闪变）
        syncWindowBg();
        setTimeout(() => el.remove(), 800);
    }, 3200);
}

/* ===== 统一刷新 ===== */

async function fullRefresh() {
    await refreshDevices();
    await renderTaskView();
    loadChainForDevice(selectedDevice ?? '');
}

/* ===== Init ===== */

window.addEventListener('DOMContentLoaded', () => {
    // ── Step 0: 初始化主题 ──
    initTheme();
    $('#btn-theme-toggle')?.addEventListener('click', toggleTheme);

    // ── Step 0.5: 自定义窗口控件（全平台 decorations: false）──
    const currentPlatform = platform();
    const appWindow = getCurrentWindow();

    if (currentPlatform === 'macos') {
        // macOS: 显示交通灯按钮
        const macControls = document.getElementById('mac-controls');
        if (macControls) {
            macControls.classList.remove('hidden');
            macControls.classList.add('flex');
        }
        $('#btn-mac-close')?.addEventListener('click', () => appWindow.close());
        $('#btn-mac-minimize')?.addEventListener('click', () => appWindow.minimize());
        $('#btn-mac-maximize')?.addEventListener('click', () => appWindow.toggleMaximize());
    } else if (currentPlatform === 'windows') {
        // Windows: 显示方块按钮
        const winControls = document.getElementById('window-controls');
        if (winControls) {
            winControls.classList.remove('hidden');
            winControls.classList.add('flex');
        }
        // Windows 调整顶部 padding
        const header = document.querySelector('header');
        if (header) {
            header.classList.remove('pt-3');
            header.classList.add('pt-1');
        }
        // Windows 不需要圆角
        document.documentElement.style.borderRadius = '0';
        document.body.style.borderRadius = '0';

        $('#btn-win-minimize')?.addEventListener('click', () => appWindow.minimize());
        $('#btn-win-maximize')?.addEventListener('click', () => appWindow.toggleMaximize());
        $('#btn-win-close')?.addEventListener('click', () => appWindow.close());
    }

    // ── Step 1: 注册跨模块回调（打破循环依赖）──
    setRefreshCallbacks(() => fullRefresh());
    setDeviceCallbacks(
        (serial: string) => loadTasksForDevice(serial),
        (serial: string) => showDeviceInfo(serial),
    );
    setTaskViewCallbacks(
        () => updateCardSelection(),
        (serial: string) => loadChainForDevice(serial),
    );

    // ── Step 2: Splash ──
    splash();

    // ── Step 3: 注册全局 window 回调 ──
    registerViewActions();
    registerTaskActions();

    // ── Step 4: 初始化后端引擎（加载任务 + 监听事件）──
    initEngine()
        .then(tasks => {
            console.log('[AutomateX] 引擎初始化成功:', tasks.length, '个任务');
            if (globalQueue.length > 0 && !activeTask) {
                setActiveTask(globalQueue[0]);
                setActiveCityIdx(0);
                renderTaskView();
            }
            loadChainForDevice('');
        })
        .catch(e => {
            console.error('[AutomateX] 引擎初始化失败:', e);
        });

    // ── Step 5: UI 事件绑定 ──
    // Search toggle
    $('#btn-search-dev')?.addEventListener('click', () => {
        const box = $('#dev-search');
        const searchInp = $('#dev-search-input') as HTMLInputElement | null;
        if (!box || !searchInp) return;
        const visible = box.style.display !== 'none';
        box.style.display = visible ? 'none' : 'block';
        if (!visible) {
            searchInp.value = '';
            searchInp.focus();
        } else filterDeviceCards('');
    });

    $('#dev-search-input')?.addEventListener('input', e => {
        filterDeviceCards((e.target as HTMLInputElement).value);
    });

    // Add Device (WiFi)
    $('#btn-add-device')?.addEventListener('click', showAddDeviceDialog);
    $('#add-device-close')?.addEventListener('click', hideAddDeviceDialog);
    $('#add-device-cancel')?.addEventListener('click', hideAddDeviceDialog);
    $('#add-device-submit')?.addEventListener('click', submitAddDevice);
    $('#add-device-modal')?.addEventListener('click', e => {
        if (e.target === e.currentTarget) hideAddDeviceDialog();
    });
    $('#add-device-addr')?.addEventListener('keydown', e => {
        if ((e as KeyboardEvent).key === 'Enter') submitAddDevice();
    });
    $('#add-device-name')?.addEventListener('keydown', e => {
        if ((e as KeyboardEvent).key === 'Enter') submitAddDevice();
    });

    // Remove Selected
    $('#btn-remove-selected')?.addEventListener('click', removeSelectedDevice);
    // Unflag Device
    $('#btn-unflag-device')?.addEventListener('click', unflagSelectedDevice);

    // Modal
    $('#modal-close')?.addEventListener('click', () => {
        ($('#modal') as HTMLElement).style.display = 'none';
    });
    $('#modal')?.addEventListener('click', e => {
        if (e.target === e.currentTarget) (e.currentTarget as HTMLElement).style.display = 'none';
    });

    // Settings
    initSettings();

    // ── Step 6: 监听后台设备事件 ──
    let devicesChangedTimer: ReturnType<typeof setTimeout> | null = null;
    listen('devices-changed', () => {
        if (devicesChangedTimer) clearTimeout(devicesChangedTimer);
        devicesChangedTimer = setTimeout(async () => {
            devicesChangedTimer = null;
            const devs = await refreshDevices();
            // 设备离线处理现在由后端引擎负责
            // 只需通知引擎当前在线设备列表
            const onlineSerials = devs
                .filter(d => d.state === DeviceState.DEVICE)
                .map(d => d.serial);
            invoke('engine_release_offline', { onlineSerials }).catch(() => {});
        }, 300);
    });

    // 监听风控触发事件
    listen<{ task_id: string; device_serial: string; message: string }>('risk-control', event => {
        const { device_serial, message } = event.payload;
        showToast(`⚠ ${device_serial}: ${message}`, 'error');
    });

    listen<string>('mqtt-status', event => {
        updateMqttStatusUI(event.payload);
    });

    // ── Step 7: 网络状态检测 ──
    function updateNetworkStatus() {
        const icon = document.getElementById('net-status-icon');
        const btn = document.getElementById('topbar-status');
        if (!icon || !btn) return;
        if (navigator.onLine) {
            icon.textContent = 'wifi';
            btn.classList.remove('text-red-500');
            btn.classList.add('text-green-500');
            btn.title = '网络已连接';
        } else {
            icon.textContent = 'wifi_off';
            btn.classList.remove('text-green-500');
            btn.classList.add('text-red-500');
            btn.title = '网络已断开';
        }
    }
    updateNetworkStatus();
    window.addEventListener('online', updateNetworkStatus);
    window.addEventListener('offline', updateNetworkStatus);

    // Auto-refresh after splash
    setTimeout(refreshDevices, 2400);
});
