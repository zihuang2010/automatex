/**
 * AutomateX — Application Entry Point
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { DeviceState } from './constants';
import { setActiveTask, setActiveCityIdx, globalQueue, activeTask, selectedDevice } from './state';
import { $ } from './utils';
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
    showDeviceInfo,
} from './dialogs';
import { initSettings, updateMqttStatusUI } from './settings';

/* ===== Theme Toggle ===== */

function initTheme() {
    const saved = localStorage.getItem('theme');
    // 默认暗色，仅用户明确选择 light 时才用亮色
    if (saved !== 'light') {
        document.documentElement.classList.add('dark');
    }
    updateThemeIcon();
}

function toggleTheme() {
    const isDark = document.documentElement.classList.toggle('dark');
    localStorage.setItem('theme', isDark ? 'dark' : 'light');
    updateThemeIcon();
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
