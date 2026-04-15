/**
 * AutomateX — Transition Page Module
 *
 * 任务同步过渡页：首次启动或无本地手机号时，
 * 在开屏动画结束后显示，让用户输入手机号并开始同步任务。
 */
import { invoke } from '@tauri-apps/api/core';

import { $ } from './utils';

export interface PhoneBindFlowOptions {
  title?: string;
  subtitle?: string;
  helperText?: string;
  submitLabel?: string;
  prefillPhones?: string[];
  forceSync?: boolean;
  emptyTasksMessage?: string;
  conflictDetails?: Array<{ mobile?: string; phone?: string; clientId: string }>;
}

let activePhoneBindPromise: Promise<void> | null = null;
let activePhoneBindCleanup: (() => void) | null = null;

export function updateStartupStatusUI(status: string): void {
  // 更新启动状态 UI（splash 页面的状态文字）
  // status 格式: 'booting:prepare' | 'booting:sync' | 'connected' | 等
  const statusEl = document.getElementById('splash-status-text');
  if (!statusEl) return;
  const labelMap: Record<string, string> = {
    'booting:prepare': '初始化中...',
    'booting:sync': '同步任务数据...',
    connected: '已连接',
  };
  statusEl.textContent = labelMap[status] ?? status;
}

/* ===== 手机号验证 ===== */

/** 验证单个手机号格式（中国大陆 11 位） */
function isValidPhone(phone: string): boolean {
  return /^1[3-9]\d{9}$/.test(phone.trim());
}

/** 手机号掩码: 138****8888 */
function maskMobile(phone: string): string {
  if (phone.length >= 7) {
    return phone.slice(0, 3) + '****' + phone.slice(-4);
  }
  return phone;
}

/**
 * 弹出「是否强制绑定」确认弹窗。
 * 返回 Promise<boolean>：true 表示用户确认强制，false 表示取消。
 */
function confirmForceBind(
  conflicts: Array<{ mobile?: string; phone?: string; clientId: string }>,
): Promise<boolean> {
  return new Promise(resolve => {
    const modal = document.getElementById('force-bind-confirm-modal') as HTMLElement | null;
    const list = document.getElementById('force-bind-conflicts-list') as HTMLElement | null;
    const cancelBtn = document.getElementById('force-bind-cancel') as HTMLElement | null;
    const okBtn = document.getElementById('force-bind-ok') as HTMLElement | null;
    if (!modal || !list || !cancelBtn || !okBtn) {
      resolve(false);
      return;
    }

    // 渲染冲突手机号列表
    list.innerHTML = conflicts
      .map(item => {
        const phone = item.mobile ?? item.phone ?? '';
        const masked = maskMobile(phone);
        const clientId = item.clientId || '未知客户端';
        return `
          <div class="border-s100 bg-s50/60 flex items-center gap-3 rounded-xl border px-3 py-2.5">
            <div class="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-amber-100 bg-amber-50">
              <span class="material-symbols-outlined text-base text-amber-500">smartphone</span>
            </div>
            <div class="min-w-0 flex-1">
              <div class="text-s800 text-[13px] font-bold tracking-wide" style="font-feature-settings:'tnum'">${masked}</div>
              <div class="text-s400 mt-0.5 truncate text-[10px]">占用端：${clientId}</div>
            </div>
          </div>
        `;
      })
      .join('');

    modal.style.display = 'flex';

    const cleanup = () => {
      modal.style.display = 'none';
      cancelBtn.removeEventListener('click', onCancel);
      okBtn.removeEventListener('click', onConfirm);
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
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onCancel();
      if (e.key === 'Enter') onConfirm();
    };

    cancelBtn.addEventListener('click', onCancel);
    okBtn.addEventListener('click', onConfirm);
    modal.addEventListener('click', onMask);
    document.addEventListener('keydown', onKey);
  });
}

/** 从 textarea 内容解析有效手机号列表 */
function parsePhones(raw: string): string[] {
  return [
    ...new Set(
      raw
        .split(/[\n,;，；\s]+/)
        .map(s => s.trim())
        .filter(s => s.length > 0),
    ),
  ];
}

/* ===== 核心逻辑 ===== */

/** 检查是否需要显示过渡页（无已同步手机号） */
export async function needsTransition(): Promise<boolean> {
  const phones = await getSyncedPhones();
  return phones.length === 0;
}

export async function getSyncedPhones(): Promise<string[]> {
  try {
    const settings = await invoke<Record<string, string>>('get_settings');
    const phonesStr = settings.synced_phones || '[]';
    return JSON.parse(phonesStr);
  } catch (e) {
    console.warn('[transition] 读取设置失败，返回空手机号列表:', e);
    return [];
  }
}

/** 显示过渡页，返回 Promise 在用户完成操作后 resolve */
export function showTransition(): Promise<void> {
  return showPhoneBindFlow();
}

export function isPhoneBindFlowVisible(): boolean {
  const el = $('#transition');
  return Boolean(el && el.style.display !== 'none');
}

export function showPhoneBindFlow(options: PhoneBindFlowOptions = {}): Promise<void> {
  if (activePhoneBindPromise && isPhoneBindFlowVisible()) {
    return activePhoneBindPromise;
  }

  activePhoneBindPromise = new Promise(resolve => {
    activePhoneBindCleanup?.();
    const el = $('#transition')!;
    const app = $('#app')!;
    const textarea = el.querySelector('#phone-numbers') as HTMLTextAreaElement;
    const errorEl = el.querySelector('#transition-error') as HTMLElement;
    const errorTextEl = el.querySelector('#transition-error-text') as HTMLElement | null;
    const errorTitleEl = el.querySelector('#transition-error-title') as HTMLElement | null;
    const errorCloseBtn = el.querySelector('#transition-error-close') as HTMLButtonElement | null;
    const counterEl = el.querySelector('#phone-counter') as HTMLElement;
    const titleEl = el.querySelector('#transition-title') as HTMLElement | null;
    const subtitleEl = el.querySelector('#transition-subtitle') as HTMLElement | null;
    const helperEl = el.querySelector('#transition-helper') as HTMLElement | null;
    const submitTextEl = el.querySelector('#transition-submit-text') as HTMLElement | null;
    const resetBtn = el.querySelector('#btn-sync-reset') as HTMLButtonElement | null;
    const submitBtn = el.querySelector('#btn-sync-start') as HTMLButtonElement | null;
    const initialPhones = options.prefillPhones ?? [];
    // 注：options.forceSync 字段保留以便后续兼容，但不再在首次提交时透传给后端，
    // 否则后端会跳过冲突检测，前端弹窗永远不会触发（详见 sync.rs 的 force 分支）
    const conflictDetails = options.conflictDetails ?? [];
    const emptyTasksMessage =
      options.emptyTasksMessage || '当前绑定手机号暂无任务，请修改手机号后重新同步。';

    if (titleEl) titleEl.textContent = options.title || '任务同步过渡';
    if (subtitleEl) subtitleEl.textContent = options.subtitle || '正在进入多端任务检索流程';
    if (helperEl)
      helperEl.textContent =
        options.helperText ||
        '系统将根据填写的号码自动匹配当前任务队列，请确保设备均已登录并处于在线状态。';
    if (submitTextEl) submitTextEl.textContent = options.submitLabel || '开始同步';
    textarea.value = initialPhones.join('\n');
    hideError();
    if (conflictDetails.length > 0) {
      showError(
        `检测到冲突手机号：${conflictDetails
          .map(item => `${item.mobile ?? item.phone ?? ''}（占用端：${item.clientId}）`)
          .join('、')}。你可以直接确认是否强制绑定。`,
      );
    }

    // 显示过渡页
    el.style.display = 'flex';
    el.classList.remove('out');
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        el.classList.add('show');
      });
    });

    // 更新手机号计数
    // 注意：背景/边框用 s* 语义 token（随主题自动切换），
    //      着色态用 500/10 半透明叠层，在浅/深色底都有良好对比度
    function updateCounter() {
      const phones = parsePhones(textarea.value);
      const valid = phones.filter(isValidPhone);
      const invalid = phones.length - valid.length;
      const base =
        'absolute bottom-3 right-3 text-[10px] font-mono px-2 py-0.5 rounded border backdrop-blur-sm';
      if (phones.length === 0) {
        counterEl.textContent = '建议一次不超过 50 个';
        counterEl.className = `${base} text-s500 bg-s100/80 border-s200`;
      } else if (invalid > 0) {
        counterEl.textContent = `${valid.length} 个有效 / ${invalid} 个无效`;
        counterEl.className = `${base} text-orange-500 bg-orange-500/10 border-orange-400/30`;
      } else {
        counterEl.textContent = `${valid.length} 个号码`;
        counterEl.className = `${base} text-emerald-600 bg-emerald-500/10 border-emerald-400/30`;
      }
    }

    function showError(msg: string, title?: string) {
      if (errorTextEl) errorTextEl.textContent = msg;
      if (errorTitleEl && title) errorTitleEl.textContent = title;
      errorEl.style.display = 'block';
      // 无障碍：宣告错误
      errorEl.setAttribute('role', 'alert');
    }

    function hideError() {
      errorEl.style.display = 'none';
    }

    // 退出过渡页 → 进入主应用
    function exit() {
      el.classList.remove('show');
      el.classList.add('out');
      app.classList.add('show');
      setTimeout(() => {
        el.style.display = 'none';
        el.classList.remove('out');
      }, 500);
      cleanup();
      activePhoneBindCleanup = null;
      activePhoneBindPromise = null;
      resolve();
    }

    function cleanup() {
      textarea.removeEventListener('input', onInput);
      submitBtn?.removeEventListener('click', onSubmit);
      resetBtn?.removeEventListener('click', onReset);
      errorCloseBtn?.removeEventListener('click', hideError);
    }

    activePhoneBindCleanup = cleanup;

    const onInput = () => {
      hideError();
      updateCounter();
    };

    const onReset = () => {
      textarea.value = initialPhones.join('\n');
      hideError();
      updateCounter();
      textarea.focus();
    };

    const onSubmit = async () => {
      const phones = parsePhones(textarea.value);
      if (phones.length === 0) {
        showError('请至少输入一个手机号');
        return;
      }
      const valid = phones.filter(isValidPhone);
      const invalid = phones.filter(p => !isValidPhone(p));
      if (invalid.length > 0) {
        showError(
          `以下号码格式不正确：${invalid.slice(0, 3).join('、')}${invalid.length > 3 ? '…' : ''}`,
        );
        return;
      }
      if (valid.length > 50) {
        showError('单次同步建议不超过 50 个号码');
        return;
      }

      // 调用同步接口；conflicts 时弹出确认弹窗，确认后用 force=true 再调一次
      const callSync = (force: boolean) =>
        invoke<{
          status: string;
          phones?: number;
          tasks?: number;
          conflicts?: Array<{ mobile?: string; phone?: string; clientId: string }>;
        }>('sync_tasks_by_phones', { phones: valid, force });

      try {
        // 先以 force=false 调用，让后端返回 conflicts；由弹窗收集用户授权后再以 force=true 重调
        let result = await callSync(false);

        if (result.status === 'conflicts' && (result.conflicts?.length ?? 0) > 0) {
          // 弹出确认弹窗：是否强制绑定（forceBind = true）
          const confirmed = await confirmForceBind(result.conflicts ?? []);
          if (!confirmed) {
            // 用户取消：在错误条上提示一下，留在当前页面
            const message =
              result.conflicts
                ?.map(item => `${item.mobile ?? item.phone ?? ''}（占用端：${item.clientId}）`)
                .join('、') || '';
            showError(`以下号码已在其他客户端绑定：${message}。已取消强制绑定。`);
            return;
          }
          // 用户确认强制绑定 → 重新调用接口，force=true
          result = await callSync(true);
        }

        if ((result.tasks ?? 0) === 0) {
          showError(emptyTasksMessage);
          return;
        }

        exit();
      } catch (e) {
        showError(`同步失败: ${e}`);
      }
    };

    textarea.addEventListener('input', onInput);
    submitBtn?.addEventListener('click', onSubmit);
    resetBtn?.addEventListener('click', onReset);
    errorCloseBtn?.addEventListener('click', hideError);
    updateCounter();
    textarea.focus();
  });

  return activePhoneBindPromise;
}
