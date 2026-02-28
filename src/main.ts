/**
 * AutomateX — Application Entry Point
 */

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Task } from './types';
import { DeviceState } from './constants';
import {
    setGlobalQueue,
    setActiveTask,
    setActiveCityIdx,
    globalQueue,
    activeTask,
    selectedDevice,
} from './state';
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
import {
    registerTaskActions,
    releaseTasksForOfflineDevices,
    setRefreshCallbacks,
    cleanupAllTimers,
} from './task-engine';
import {
    showAddDeviceDialog,
    hideAddDeviceDialog,
    submitAddDevice,
    removeSelectedDevice,
    showDeviceInfo,
} from './dialogs';
import { initSettings, updateMqttStatusUI } from './settings';

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

    // ── Step 4: 初始化全局任务队列（从后端加载）──
    invoke<Task[]>('list_tasks')
        .then(tasks => {
            console.log('[AutomateX] 加载任务成功:', tasks.length, '个');
            setGlobalQueue(tasks);
            if (globalQueue.length > 0 && !activeTask) {
                setActiveTask(globalQueue[0]);
                setActiveCityIdx(0);
                renderTaskView();
            }
            loadChainForDevice('');
        })
        .catch(e => {
            console.error('[AutomateX] 加载任务失败:', e);
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

    // ── Step 6: 监听后台事件（带 debounce 防抖）──
    let devicesChangedTimer: ReturnType<typeof setTimeout> | null = null;
    listen('devices-changed', () => {
        if (devicesChangedTimer) clearTimeout(devicesChangedTimer);
        devicesChangedTimer = setTimeout(async () => {
            devicesChangedTimer = null;
            const devs = await refreshDevices();
            const onlineSerials = new Set(
                devs.filter(d => d.state !== DeviceState.OFFLINE).map(d => d.serial),
            );
            const released = releaseTasksForOfflineDevices(onlineSerials);
            if (released > 0) {
                renderTaskView();
                loadChainForDevice(selectedDevice ?? '');
            }
        }, 300);
    });

    listen<string>('mqtt-status', event => {
        updateMqttStatusUI(event.payload);
    });

    // Auto-refresh after splash
    setTimeout(refreshDevices, 2400);

    // ── Step 7: 页面卸载时清理定时器 ──
    window.addEventListener('beforeunload', cleanupAllTimers);
});
