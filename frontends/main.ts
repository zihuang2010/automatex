/**
 * AutomateX — Application Entry Point
 */
import { invoke } from '@tauri-apps/api/core';
import { type UnlistenFn, listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { platform } from '@tauri-apps/plugin-os';

import { DeviceState } from './constants';
import { refreshDevices, setDeviceCallbacks, updateCardSelection } from './devices';
import {
  hideAddDeviceDialog,
  removeSelectedDevice,
  showAddDeviceDialog,
  showDeviceInfo,
  submitAddDevice,
  unflagSelectedDevice,
} from './dialogs';
import { initMirror, startMirror, stopAllMirrors } from './mirror';
import { loadChainForDevice } from './queue';
import { initSettings, updateMqttStatusUI } from './settings';
import { activeTask, globalQueue, selectedDevice, setActiveCityIdx, setActiveTask } from './state';
import { initEngine, registerTaskActions, setRefreshCallbacks } from './task-engine';
import {
  loadTasksForDevice,
  registerViewActions,
  renderTaskView,
  setTaskViewCallbacks,
} from './task-view';
import {
  getSyncedPhones,
  isPhoneBindFlowVisible,
  needsTransition,
  showPhoneBindFlow,
  showTransition,
} from './transition';
import { $, showToast } from './utils';

let appBootstrapped = false;
let accountPanelInitialized = false;
const appUnlisteners: UnlistenFn[] = [];

// 窗口关闭时优雅停止所有投屏
window.addEventListener('beforeunload', () => {
  stopAllMirrors();
  while (appUnlisteners.length > 0) {
    try {
      appUnlisteners.pop()?.();
    } catch {
      /* ignore */
    }
  }
});

/* ===== Theme Toggle ===== */

function initTheme() {
  // 先从 localStorage 快速应用（避免开屏期间白屏闪烁）
  const saved = localStorage.getItem('theme');
  if (saved !== 'light') {
    document.documentElement.classList.add('dark');
  }

  // 异步从数据库读取真实主题设置并同步（同时预加载账号计数避免显示延迟）
  invoke<Record<string, string>>('get_settings')
    .then(settings => {
      const dbTheme = settings.theme || 'dark';
      const currentIsDark = document.documentElement.classList.contains('dark');
      const dbIsDark = dbTheme === 'dark';
      if (currentIsDark !== dbIsDark) {
        if (dbIsDark) {
          document.documentElement.classList.add('dark');
        } else {
          document.documentElement.classList.remove('dark');
        }
        localStorage.setItem('theme', dbTheme);
      }
    })
    .catch(() => {});
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
  // 退场 → 检查是否需要过渡页
  setTimeout(async () => {
    el.classList.add('out');
    // 开屏退场后同步窗口背景色
    try {
      getCurrentWindow()
        .setBackgroundColor('#00000000')
        .catch(() => {});
    } catch {
      /* 非 Tauri 环境忽略 */
    }
    setTimeout(() => el.remove(), 800);

    // 检查是否需要任务同步过渡页
    const showTrans = await needsTransition();
    if (showTrans) {
      // 显示过渡页，等用户完成后再进入主应用
      await showTransition();
    }
    // 进入主应用
    app.classList.add('show');
  }, 3200);
}

/* ===== 统一刷新 ===== */

async function fullRefresh() {
  await refreshDevices();
  await renderTaskView();
  loadChainForDevice(selectedDevice ?? '');
}

/* ===== Account Panel ===== */

/** 手机号掩码: 138****8888 */
function maskPhone(phone: string): string {
  if (phone.length >= 7) {
    return phone.slice(0, 3) + '****' + phone.slice(-4);
  }
  return phone;
}

/** 渲染账号列表 */
function renderAccountList(phones: string[]) {
  const list = document.getElementById('account-list');
  const label = document.getElementById('account-sync-label');
  const count = document.getElementById('account-active-count');
  if (!list) return;

  if (label) label.textContent = `已同步 ${phones.length} 个账号`;
  if (count) count.textContent = `${phones.length} 个活跃账号`;

  if (phones.length === 0) {
    list.innerHTML = '<div class="text-s400 px-4 py-6 text-center text-xs">暂无已同步账号</div>';
    return;
  }

  list.innerHTML = phones
    .map(
      phone => `
    <div class="border-s100 flex items-center justify-between border-b px-4 py-2.5 transition-colors last:border-b-0 hover:bg-s50">
      <div>
        <div class="text-s800 text-[13px] font-semibold tracking-wide" style="font-feature-settings:'tnum'">${maskPhone(phone)}</div>
        <div class="mt-0.5 flex items-center gap-1 text-[10px] font-semibold text-emerald-500">
          <span class="inline-block h-[5px] w-[5px] rounded-full bg-emerald-500"></span>
          已就绪
        </div>
      </div>
      <button title="移除" data-phone="${phone}"
        class="text-s400 hover:text-red-500 hover:bg-red-50 flex h-7 w-7 items-center justify-center rounded-md border-none bg-transparent cursor-pointer transition-all">
        <span class="material-symbols-outlined icon-sm text-[16px]">delete</span>
      </button>
    </div>
  `,
    )
    .join('');
}

/** 从后端刷新账号列表 */
async function refreshAccountList(): Promise<string[]> {
  try {
    const settings = await invoke<Record<string, string>>('get_settings');
    const phones: string[] = JSON.parse(settings.synced_phones || '[]');
    renderAccountList(phones);
    return phones;
  } catch {
    renderAccountList([]);
    return [];
  }
}

async function openPhoneBindPage(mode: 'startup' | 'rebind' | 'empty-tasks' = 'rebind') {
  if (isPhoneBindFlowVisible()) return;

  const phones = await getSyncedPhones();
  const hasBoundPhones = phones.length > 0;

  const config =
    mode === 'startup'
      ? {
          title: '任务同步过渡',
          subtitle: '正在进入多端任务检索流程',
          submitLabel: '开始同步',
          forceSync: false,
          prefillPhones: phones,
        }
      : mode === 'empty-tasks'
        ? {
            title: '重新绑定手机号',
            subtitle: '当前绑定手机号暂无可用任务，请修改后重新同步',
            helperText: '如果原手机号的任务已被删除或迁移，请在这里更新新的任务手机号。',
            submitLabel: '重新绑定并同步',
            forceSync: true,
            prefillPhones: phones,
            emptyTasksMessage: '当前绑定手机号仍然没有任务，请修改手机号后重新同步。',
          }
        : {
            title: hasBoundPhones ? '修改同步手机号' : '绑定同步手机号',
            subtitle: hasBoundPhones
              ? '当前绑定已失效或被挤下线，请重新绑定后继续使用'
              : '请先绑定手机号后开始使用',
            helperText: '支持直接修改现有手机号并重新同步，系统会自动清理失效任务。',
            submitLabel: hasBoundPhones ? '重新绑定并同步' : '绑定并同步',
            forceSync: hasBoundPhones,
            prefillPhones: phones,
            emptyTasksMessage: '当前绑定手机号暂无任务，请修改手机号后重新同步。',
          };

  await showPhoneBindFlow(config);
  await Promise.all([refreshAccountList(), fullRefresh()]);
}

function initAccountPanel() {
  if (accountPanelInitialized) return;
  accountPanelInitialized = true;
  const btn = document.getElementById('btn-account-sync');
  const panel = document.getElementById('account-panel');
  const chevron = document.getElementById('account-chevron');
  const manageBtn = document.getElementById('btn-manage-accounts');
  const accountList = document.getElementById('account-list');

  if (!btn || !panel) return;

  // Toggle — 每次打开面板时重新获取最新数据
  btn.addEventListener('click', e => {
    e.stopPropagation();
    const isOpen = panel.classList.contains('open');
    if (isOpen) {
      panel.classList.remove('open');
      panel.classList.add('hidden');
      chevron?.classList.remove('rotated');
    } else {
      panel.classList.remove('hidden');
      panel.classList.add('open');
      chevron?.classList.add('rotated');
      // 每次展开时刷新列表，确保数据最新
      refreshAccountList();
    }
  });

  // Outside click
  document.addEventListener('click', e => {
    const wrap = document.getElementById('account-panel-wrap');
    if (wrap && !wrap.contains(e.target as Node)) {
      panel.classList.remove('open');
      panel.classList.add('hidden');
      chevron?.classList.remove('rotated');
    }
  });

  // ── 删除账号逻辑（事件委托） ──

  /** 显示自定义确认弹窗，返回 Promise<boolean>（支持键盘 Escape/Enter） */
  function confirmRemoveAccount(maskedPhone: string): Promise<boolean> {
    return new Promise(resolve => {
      const modal = document.getElementById('confirm-remove-modal') as HTMLElement;
      const phoneLabel = document.getElementById('confirm-remove-phone');
      const cancelBtn = document.getElementById('confirm-remove-cancel');
      const okBtn = document.getElementById('confirm-remove-ok');
      if (!modal) {
        resolve(false);
        return;
      }

      // 填入手机号
      if (phoneLabel) phoneLabel.textContent = maskedPhone;
      modal.style.display = 'flex';

      // 统一清理所有事件监听
      const cleanup = () => {
        modal.style.display = 'none';
        cancelBtn?.removeEventListener('click', onCancel);
        okBtn?.removeEventListener('click', onConfirm);
        modal.removeEventListener('click', onMask);
        document.removeEventListener('keydown', onKey);
      };
      const onCancel = () => {
        cleanup();
        resolve(false);
      };
      const onConfirm = () => {
        cleanup();
        resolve(true);
      };
      const onMask = (e: MouseEvent) => {
        if (e.target === e.currentTarget) {
          cleanup();
          resolve(false);
        }
      };
      // P3: 键盘支持 — Escape 取消，Enter 确认
      const onKey = (e: KeyboardEvent) => {
        if (e.key === 'Escape') onCancel();
        if (e.key === 'Enter') onConfirm();
      };
      cancelBtn?.addEventListener('click', onCancel);
      okBtn?.addEventListener('click', onConfirm);
      modal.addEventListener('click', onMask);
      document.addEventListener('keydown', onKey);
    });
  }

  accountList?.addEventListener('click', async e => {
    const target = (e.target as HTMLElement).closest('button[data-phone]') as HTMLElement | null;
    if (!target) return;

    const phoneToRemove = target.dataset.phone;
    if (!phoneToRemove) return;

    const masked = maskPhone(phoneToRemove);

    // 使用自定义确认弹窗
    const confirmed = await confirmRemoveAccount(masked);
    if (!confirmed) return;

    // 禁用按钮防止重复操作
    target.setAttribute('disabled', 'true');
    target.classList.add('opacity-50');

    try {
      // P1 优化：从 DOM 读取当前列表，避免多余的 get_settings 调用
      const allPhoneBtns = accountList!.querySelectorAll('button[data-phone]');
      const remaining: string[] = [];
      allPhoneBtns.forEach(btn => {
        const p = (btn as HTMLElement).dataset.phone;
        if (p && p !== phoneToRemove) remaining.push(p);
      });

      if (remaining.length === 0) {
        await invoke('sync_tasks_by_phones', { phones: [], force: true });
        showToast(`已移除账号 ${masked}`, 'info');
        await Promise.all([refreshAccountList(), fullRefresh()]);
        openPhoneBindPage('startup').catch(console.error);
        return;
      }

      // 用剩余号码重新同步（后端会自动清理被移除的号码，空列表也能正确处理）
      const syncResult = await invoke<{ tasks?: number }>('sync_tasks_by_phones', {
        phones: remaining,
        force: true,
      });

      showToast(`已移除账号 ${masked}`, 'info');
      // P4 优化：并行刷新账号列表和任务视图
      await Promise.all([refreshAccountList(), fullRefresh()]);

      if ((syncResult.tasks ?? 0) === 0) {
        openPhoneBindPage('empty-tasks').catch(console.error);
      }
    } catch (err) {
      showToast(`移除失败: ${err}`, 'error');
      target.removeAttribute('disabled');
      target.classList.remove('opacity-50');
    }
  });

  // ── 管理/添加按钮 → 打开添加账号弹窗（回显已有手机号） ──
  manageBtn?.addEventListener('click', async () => {
    panel.classList.remove('open');
    panel.classList.add('hidden');
    chevron?.classList.remove('rotated');
    await openPhoneBindPage('rebind');
  });

  // ── 添加账号弹窗交互 ──
  const accountModal = document.getElementById('add-account-modal') as HTMLElement;
  if (accountModal) {
    // 关闭按钮
    document.getElementById('add-account-close')?.addEventListener('click', () => {
      accountModal.style.display = 'none';
    });
    // 点击遮罩关闭
    accountModal.addEventListener('click', e => {
      if (e.target === e.currentTarget) accountModal.style.display = 'none';
    });

    // 提交同步
    document.getElementById('add-account-submit')?.addEventListener('click', async () => {
      const textarea = document.getElementById('add-account-phones') as HTMLTextAreaElement;
      const submitBtn = document.getElementById('add-account-submit') as HTMLButtonElement;
      const raw = textarea?.value?.trim();

      if (!raw) {
        showToast('请输入至少一个手机号', 'error');
        return;
      }

      const phones = raw
        .split(/[\n,;，；]+/)
        .map(s => s.trim())
        .filter(Boolean);

      // P2: 校验手机号格式（中国大陆 11 位手机号）
      const validPhoneRe = /^1\d{10}$/;
      const invalidPhones = phones.filter(p => !validPhoneRe.test(p));
      if (invalidPhones.length > 0) {
        showToast(
          `以下号码格式无效：${invalidPhones.slice(0, 3).join('、')}${invalidPhones.length > 3 ? '…' : ''}`,
          'error',
        );
        return;
      }

      // 去重
      const uniquePhones = [...new Set(phones)];

      if (uniquePhones.length === 0) {
        showToast('请输入有效的手机号', 'error');
        return;
      }

      // Loading 状态
      submitBtn.setAttribute('disabled', 'true');
      submitBtn.innerHTML = `
        <span class="material-symbols-outlined text-lg sync-icon-spin">sync</span>
        <span class="font-mono">同步中...</span>
      `;

      try {
        const result = await invoke<{
          status: string;
          phones?: number;
          tasks?: number;
          conflicts?: unknown[];
        }>('sync_tasks_by_phones', { phones: uniquePhones, force: true });

        accountModal.style.display = 'none';
        showToast(
          `同步完成：${result.phones ?? uniquePhones.length} 个账号，${result.tasks ?? 0} 个任务`,
          'info',
        );

        // P4 优化：并行刷新账号列表和任务视图
        await Promise.all([refreshAccountList(), fullRefresh()]);
      } catch (err) {
        showToast(`同步失败: ${err}`, 'error');
      } finally {
        // 恢复按钮
        submitBtn.removeAttribute('disabled');
        submitBtn.innerHTML = `
          <span class="material-symbols-outlined text-lg">sync_alt</span>
          <span class="font-mono">开始同步</span>
        `;
      }
    });
  }
}

/* ===== Init ===== */

window.addEventListener('DOMContentLoaded', () => {
  if (appBootstrapped) return;
  appBootstrapped = true;
  // ── Step 0: 初始化主题 ──
  initTheme();

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
    document.documentElement.style.clipPath = 'none';
    document.body.style.borderRadius = '0';
    // 清除 splash 页圆角（内联 style="border-radius: 10px"）
    const splashEl = document.getElementById('splash');
    if (splashEl) {
      splashEl.style.borderRadius = '0';
    }
    // 清除 #app-root 的 Tailwind 圆角 class
    const appRoot = document.getElementById('app-root');
    if (appRoot) {
      appRoot.classList.remove('rounded-[10px]');
      appRoot.style.borderRadius = '0';
    }

    $('#btn-win-minimize')?.addEventListener('click', () => appWindow.minimize());
    $('#btn-win-maximize')?.addEventListener('click', () => appWindow.toggleMaximize());
    $('#btn-win-close')?.addEventListener('click', () => appWindow.close());
  }

  // ── Step 1: 注册跨模块回调（打破循环依赖）──
  setRefreshCallbacks(() => fullRefresh());
  setDeviceCallbacks(
    (serial: string) => loadTasksForDevice(serial),
    (serial: string) => showDeviceInfo(serial),
    (serial: string) => startMirror(serial),
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
      if (globalQueue.length > 0 && !activeTask) {
        setActiveTask(globalQueue[0]);
        setActiveCityIdx(0);
      }
      renderTaskView();
      loadChainForDevice('');
      // 启动时从数据库加载已同步的账号（修复重启后显示 0 个）
      refreshAccountList().then(phones => {
        if (tasks.length === 0 && phones.length > 0) {
          openPhoneBindPage('empty-tasks').catch(console.error);
        }
      });
    })
    .catch(e => {
      console.error('[AutomateX] 引擎初始化失败:', e);
    });

  // ── Step 5: UI 事件绑定 ──

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

  // ── Account Panel ──
  initAccountPanel();

  // ── Mirror ──
  initMirror();

  // ── Step 6: 监听后台设备事件 ──
  let devicesChangedTimer: ReturnType<typeof setTimeout> | null = null;
  void (async () => {
    appUnlisteners.push(
      await listen('devices-changed', () => {
        if (devicesChangedTimer) clearTimeout(devicesChangedTimer);
        devicesChangedTimer = setTimeout(async () => {
          devicesChangedTimer = null;
          const devs = await refreshDevices();
          // 设备离线处理现在由后端引擎负责
          // 只需通知引擎当前在线设备列表
          const onlineSerials = devs.filter(d => d.state === DeviceState.DEVICE).map(d => d.serial);
          invoke('engine_release_offline', { onlineSerials }).catch(() => {});
        }, 300);
      }),
    );

    // ── 监听账号同步变更事件（唯一更新路径）──
    appUnlisteners.push(
      await listen<{ phones: string[] }>('account://sync-changed', event => {
        renderAccountList(event.payload.phones);
      }),
    );

    appUnlisteners.push(
      await listen<{ tasks: Array<{ id: string }> }>('task://update', async event => {
        if (event.payload.tasks.length > 0 || isPhoneBindFlowVisible()) return;
        const phones = await getSyncedPhones();
        if (phones.length > 0) {
          openPhoneBindPage('empty-tasks').catch(console.error);
        }
      }),
    );

    appUnlisteners.push(
      await listen<{ reason?: string; message?: string }>('require-phone-bind', event => {
        const reason = event.payload?.reason;
        if (reason === 'no_phones') {
          openPhoneBindPage('startup').catch(console.error);
          return;
        }
        if (reason === 'all_expired') {
          openPhoneBindPage('rebind').catch(console.error);
          return;
        }
        openPhoneBindPage('rebind').catch(console.error);
      }),
    );

    // 监听风控触发事件
    appUnlisteners.push(
      await listen<{ task_id: string; device_serial: string; message: string }>(
        'risk-control',
        event => {
          const { device_serial, message } = event.payload;
          showToast(`⚠ ${device_serial}: ${message}`, 'error');
        },
      ),
    );

    appUnlisteners.push(
      await listen<string>('mqtt-status', event => {
        updateMqttStatusUI(event.payload);
      }),
    );
  })();

  invoke<string>('mqtt_status')
    .then(status => updateMqttStatusUI(status))
    .catch(() => {});

  // ── Step 7: 网络状态检测 ──
  function updateNetworkStatus() {
    const icon = document.getElementById('net-status-icon');
    const btn = document.getElementById('topbar-status');
    if (!icon || !btn) return;
    if (navigator.onLine) {
      icon.textContent = 'language';
      btn.classList.remove('text-red-500');
      btn.classList.add('text-green-500');
      btn.title = '网络已连接';
    } else {
      icon.textContent = 'language';
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
