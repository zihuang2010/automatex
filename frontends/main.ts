/**
 * AutomateX — Application Entry Point
 */
import { getVersion } from '@tauri-apps/api/app';
import { invoke } from '@tauri-apps/api/core';
import { type UnlistenFn, listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { platform } from '@tauri-apps/plugin-os';

import { DeviceState } from './constants';
import { cleanupDevices, refreshDevices, setDeviceCallbacks, updateCardSelection } from './devices';
import {
  hideAddDeviceDialog,
  removeSelectedDevice,
  showAddDeviceDialog,
  showDeviceInfo,
  submitAddDevice,
  unflagSelectedDevice,
} from './dialogs';
import { initMirror, startMirror, stopAllMirrors } from './mirror';
import { cleanupQueue, loadChainForDevice } from './queue';
import { initSettings, updateMqttStatusUI } from './settings';
import { selectedDevice } from './state';
import { initEngine, registerTaskActions } from './task-engine';
import {
  cleanupTaskView,
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
  updateStartupStatusUI,
} from './transition';
import { setupUpdaterListeners } from './updater';
import { $, showToast } from './utils';

let appBootstrapped = false;
let accountPanelInitialized = false;
const appUnlisteners: UnlistenFn[] = [];
let activeUnbindModalCleanup: (() => void) | null = null;

/* ===== 生产环境交互拦截 =====
 * 仅在 vite 构建出的生产包中生效：
 *  - 屏蔽右键菜单、F12/DevTools 快捷键、页面刷新（F5/Ctrl+R）、查看源码
 *  - 屏蔽外部文件拖入 webview（避免意外替换资源或跳离应用）
 * 开发期保留全部调试能力。
 */
if (import.meta.env.PROD) {
  window.addEventListener('contextmenu', e => e.preventDefault());

  window.addEventListener('keydown', e => {
    const k = e.key.toLowerCase();
    // F12
    if (k === 'f12') {
      e.preventDefault();
      return;
    }
    // Ctrl/Cmd + Shift + I / J / C  → DevTools
    if ((e.ctrlKey || e.metaKey) && e.shiftKey && (k === 'i' || k === 'j' || k === 'c')) {
      e.preventDefault();
      return;
    }
    // Ctrl/Cmd + R / F5 → 页面刷新会让 Rust 端状态与 webview 脱节
    if (((e.ctrlKey || e.metaKey) && k === 'r') || k === 'f5') {
      e.preventDefault();
      return;
    }
    // Ctrl/Cmd + U → 查看源码
    if ((e.ctrlKey || e.metaKey) && k === 'u') {
      e.preventDefault();
      return;
    }
    // Ctrl/Cmd + P → 打印对话框
    if ((e.ctrlKey || e.metaKey) && k === 'p') {
      e.preventDefault();
      return;
    }
  });

  window.addEventListener('dragover', e => e.preventDefault());
  window.addEventListener('drop', e => e.preventDefault());
}

// 窗口关闭时优雅停止所有投屏并清理所有资源
window.addEventListener('beforeunload', () => {
  stopAllMirrors();
  // MEM-L3: 清除倒计时 setInterval 和 SortableJS 实例
  cleanupQueue();
  // LOGIC-2: 销毁城市拖拽 Sortable 实例
  cleanupTaskView();
  // MEM-L4: 取消挂起的 requestAnimationFrame
  cleanupDevices();
  // 清除 Tauri 事件监听器
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
  // 早期内联脚本（index.html <head>）已根据 localStorage 应用初步主题，
  // 此处仅负责以数据库为权威来源做二次校正，并清理不一致的 localStorage。
  // 数据库默认值由后端 startup.rs 写入为 'light'。
  invoke<Record<string, string>>('get_settings')
    .then(settings => {
      const dbTheme = settings.theme === 'dark' ? 'dark' : 'light';
      const dbIsDark = dbTheme === 'dark';
      const currentIsDark = document.documentElement.classList.contains('dark');
      if (currentIsDark !== dbIsDark) {
        if (dbIsDark) {
          document.documentElement.classList.add('dark');
        } else {
          document.documentElement.classList.remove('dark');
        }
      }
      // 始终把 localStorage 对齐到数据库（防止历史构建残留 'dark' 干扰）
      localStorage.setItem('theme', dbTheme);
    })
    .catch(() => {
      // 后端不可达时兜底：保持浅色，并清理可能的脏 localStorage
      document.documentElement.classList.remove('dark');
      try {
        localStorage.setItem('theme', 'light');
      } catch {
        /* ignore */
      }
    });
}

/* ===== Splash Screen ===== */

function splash() {
  const el = $('#splash')!;
  const app = $('#app')!;
  updateStartupStatusUI('booting:prepare');
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

async function openPhoneBindPage(
  mode: 'startup' | 'rebind' | 'empty-tasks' = 'rebind',
  conflictDetails: Array<{ mobile?: string; phone?: string; clientId: string }> = [],
  prefillPhones?: string[],
) {
  if (isPhoneBindFlowVisible()) return;

  const phones = prefillPhones ?? (await getSyncedPhones());
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
            conflictDetails,
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
      // Step 1: 调用 unbind_phone —— 命中后端 /mttl_tools/v1/meituanTraffic/client/unbind
      // 同时完成：HTTP 解绑 + 本地任务清理 + 更新 SYNCED_PHONES + 发 ACCOUNT_SYNC_CHANGED 事件
      const unbindResult = await invoke<{ status: string; phones: number; tasks: number }>(
        'unbind_phone',
        { phone: phoneToRemove },
      );

      showToast(`已移除账号 ${masked}`, 'info');

      // Step 2: 如果最后一个号码被解绑，直接跳转同步过渡页
      if (unbindResult.phones === 0) {
        await Promise.all([refreshAccountList(), fullRefresh()]);
        openPhoneBindPage('startup').catch(console.error);
        return;
      }

      // Step 3: 对剩余号码重新同步 —— 强制以服务端为准刷新任务状态，
      // 避免 handle_phones_unbind 因 phone 列匹配失败而漏清理，也保证剩余号码
      // 的任务数是服务端最新数据（而不是 DB 里过期的行数）
      const remaining = await getSyncedPhones();
      const syncResult = await invoke<{
        status: string;
        phones?: number;
        tasks?: number;
      }>('sync_tasks_by_phones', { phones: remaining, force: true });

      await Promise.all([refreshAccountList(), fullRefresh()]);

      if ((syncResult.tasks ?? 0) === 0) {
        // 剩余号码均无任务 → 提示用户重绑
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
      const uniquePhones = [...new Set(phones)];
      if (uniquePhones.length < phones.length) {
        showToast(`已自动去重 ${phones.length - uniquePhones.length} 个重复手机号`, 'info');
      }

      // P2: 校验手机号格式（中国大陆 11 位手机号）
      const validPhoneRe = /^1\d{10}$/;
      const invalidPhones = uniquePhones.filter(p => !validPhoneRe.test(p));
      if (invalidPhones.length > 0) {
        showToast(
          `以下号码格式无效：${invalidPhones.slice(0, 3).join('、')}${invalidPhones.length > 3 ? '…' : ''}`,
          'error',
        );
        return;
      }

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
          conflicts?: Array<{ mobile?: string; phone?: string; clientId: string }>;
        }>('sync_tasks_by_phones', { phones: uniquePhones, force: false });

        if (result.status === 'conflicts' && (result.conflicts?.length ?? 0) > 0) {
          accountModal.style.display = 'none';
          await openPhoneBindPage('rebind', result.conflicts, uniquePhones);
          return;
        }

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

/* ===== Unbind Notify Modal ===== */

function showUnbindNotifyModal(mobiles: string[], reason?: string) {
  const modal = document.getElementById('unbind-notify-modal') as HTMLElement;
  if (!modal) return;

  const reasonWrap = document.getElementById('unbind-notify-reason') as HTMLElement;
  const reasonText = document.getElementById('unbind-notify-reason-text') as HTMLElement;
  const phonesList = document.getElementById('unbind-notify-phones') as HTMLElement;
  const okBtn = document.getElementById('unbind-notify-ok') as HTMLButtonElement | null;

  // 显示原因
  if (reason && reasonWrap && reasonText) {
    reasonText.textContent = reason;
    reasonWrap.style.display = 'flex';
  } else if (reasonWrap) {
    reasonWrap.style.display = 'none';
  }

  // 渲染手机号列表
  if (phonesList) {
    phonesList.innerHTML = mobiles
      .map(
        phone => `
      <div class="flex items-center gap-3 rounded-xl border border-amber-100 bg-amber-50/50 px-4 py-2.5">
        <div class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-amber-200 bg-white">
          <span class="material-symbols-outlined text-base text-amber-400">smartphone</span>
        </div>
        <div class="text-s800 text-[13px] font-bold tracking-wide" style="font-feature-settings:'tnum'">${maskPhone(phone)}</div>
      </div>`,
      )
      .join('');
  }

  activeUnbindModalCleanup?.();
  modal.style.display = 'flex';
  okBtn?.focus({ preventScroll: true });

  // 持有状态，避免重复触发（Enter + 点击 同时发生时只执行一次）
  let acknowledging = false;

  // 点击「我知道了」：先**立即关闭弹窗**给用户即时反馈，再异步调用后端
  // 清理被解绑的手机号及任务、刷新账号列表与任务状态。
  //
  // 为什么先关再做：原实现等后端 `invoke` 返回后才 cleanup，点击到弹窗消失之间
  // 有 ~100ms+ 空白期，用户会以为「没点到」而反复点击。现在按下首击立即消失。
  //
  // 注意：不支持点击遮罩或 Esc 关闭 —— 异地登录是强制提醒，必须点按钮确认。
  const onOk = async () => {
    if (acknowledging) return;
    acknowledging = true;
    // 首击即时生效：立即隐藏弹窗并解绑监听，后端处理并行进行
    cleanup();
    try {
      const result = await invoke<{
        status: string;
        phones: number;
        tasks: number;
      }>('acknowledge_phones_unbind', { phones: mobiles });
      await Promise.all([refreshAccountList(), fullRefresh()]);
      if ((result.phones ?? 0) === 0 && (result.tasks ?? 0) === 0) {
        openPhoneBindPage('startup').catch(console.error);
        return;
      }
      if ((result.tasks ?? 0) === 0) {
        openPhoneBindPage('empty-tasks').catch(console.error);
      }
    } catch (err) {
      console.error('[unbind-notify] 确认失败:', err);
      showToast(`确认失败: ${err}`, 'error');
    }
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      void onOk();
    }
  };

  const onPress = () => {
    void onOk();
  };

  const cleanup = () => {
    modal.style.display = 'none';
    okBtn?.removeEventListener('click', onPress);
    okBtn?.removeEventListener('keydown', onKey);
    activeUnbindModalCleanup = null;
  };

  okBtn?.addEventListener('click', onPress);
  okBtn?.addEventListener('keydown', onKey);
  activeUnbindModalCleanup = cleanup;
}

/* ===== Init ===== */

window.addEventListener('DOMContentLoaded', () => {
  if (appBootstrapped) return;
  appBootstrapped = true;
  // ── Step 0: 初始化主题 ──
  initTheme();

  // ── Step 0.1: 把所有 .js-app-version 元素替换成真实版本号（从 tauri.conf.json 读）──
  // 元素自己负责前缀（v/V/Version），JS 只填裸版本号 1.0.6
  void (async () => {
    try {
      const v = await getVersion();
      document.querySelectorAll<HTMLElement>('.js-app-version').forEach(el => {
        el.textContent = v;
      });
    } catch (err) {
      console.warn('[main] 读取 app version 失败:', err);
    }
  })();

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

    // 过渡页 macOS 交通灯
    const transMacControls = document.getElementById('transition-mac-controls');
    if (transMacControls) {
      transMacControls.classList.remove('hidden');
      transMacControls.classList.add('flex');
    }
    $('#btn-transition-mac-close')?.addEventListener('click', () => appWindow.close());
    $('#btn-transition-mac-minimize')?.addEventListener('click', () => appWindow.minimize());
    $('#btn-transition-mac-maximize')?.addEventListener('click', () => appWindow.toggleMaximize());
  } else if (currentPlatform === 'windows') {
    // 给 <html> 打 os-windows 标记，便于样式层做 Windows 专属兼容/覆盖
    document.documentElement.classList.add('os-windows');
    // Windows: 显示方块按钮
    const winControls = document.getElementById('window-controls');
    if (winControls) {
      winControls.classList.remove('hidden');
      winControls.classList.add('flex');
    }

    // 过渡页 Windows 关闭按钮
    const transWinClose = document.getElementById('btn-transition-win-close');
    if (transWinClose) {
      transWinClose.classList.remove('hidden');
      transWinClose.classList.add('flex');
    }
    $('#btn-transition-win-close')?.addEventListener('click', () => appWindow.close());
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
  setDeviceCallbacks(
    (serial: string) => loadTasksForDevice(serial),
    (serial: string) => showDeviceInfo(serial),
    (serial: string) => startMirror(serial),
    async (serial: string) => {
      try {
        const msg = await invoke<string>('switch_device_to_wifi', { serial });
        showToast(msg, 'info');
      } catch (e) {
        showToast(`切换无线失败: ${e}`, 'error');
      }
    },
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
    .then(({ tasks, unlisten }) => {
      // LOGIC-9 修复：将 task://update 监听器纳入 appUnlisteners 统一管理
      appUnlisteners.push(unlisten);
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
          // MEM 修复：在 beforeunload 时 devicesChangedTimer 也应被清除
          // 通过将 clear 逻辑归入 appUnlisteners 外层 IIFE 控制
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
      await listen<string>('startup-sync-status', event => {
        updateStartupStatusUI(event.payload);
      }),
    );

    appUnlisteners.push(
      await listen<{
        reason?: string;
        message?: string;
        conflicts?: Array<{ mobile?: string; phone?: string; clientId: string }>;
      }>('require-phone-bind', event => {
        const reason = event.payload?.reason;
        if (reason === 'no_phones') {
          openPhoneBindPage('startup').catch(console.error);
          return;
        }
        if (reason === 'all_expired') {
          openPhoneBindPage('rebind').catch(console.error);
          return;
        }
        if (reason === 'conflicts') {
          openPhoneBindPage('rebind', event.payload?.conflicts ?? []).catch(console.error);
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

    // ── 监听手机号解绑通知 ──
    appUnlisteners.push(
      await listen<{ mobiles: string[]; reason?: string }>('mqtt-phones-unbind', event => {
        const { mobiles, reason } = event.payload;
        showUnbindNotifyModal(mobiles, reason);
      }),
    );

    // ── 监听广播下线 → 退出应用 ──
    appUnlisteners.push(
      await listen('mqtt-broadcast-offline', () => {
        // 后端已完成 stop_all_tasks + MQTT disconnect，前端关闭窗口释放资源
        getCurrentWindow().close();
      }),
    );

    // ── 监听应用自动更新事件 ──
    const updaterUnlisteners = await setupUpdaterListeners();
    appUnlisteners.push(...updaterUnlisteners);
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
