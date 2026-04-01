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

/* ===== 手机号验证 ===== */

/** 验证单个手机号格式（中国大陆 11 位） */
function isValidPhone(phone: string): boolean {
  return /^1[3-9]\d{9}$/.test(phone.trim());
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
    const errorTextEl = errorEl.querySelector('span:last-child') as HTMLElement | null;
    const counterEl = el.querySelector('#phone-counter') as HTMLElement;
    const titleEl = el.querySelector('#transition-title') as HTMLElement | null;
    const subtitleEl = el.querySelector('#transition-subtitle') as HTMLElement | null;
    const helperEl = el.querySelector('#transition-helper') as HTMLElement | null;
    const submitTextEl = el.querySelector('#transition-submit-text') as HTMLElement | null;
    const resetBtn = el.querySelector('#btn-sync-reset') as HTMLButtonElement | null;
    const submitBtn = el.querySelector('#btn-sync-start') as HTMLButtonElement | null;
    const initialPhones = options.prefillPhones ?? [];
    const forceSync = options.forceSync ?? false;
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
    function updateCounter() {
      const phones = parsePhones(textarea.value);
      const valid = phones.filter(isValidPhone);
      const invalid = phones.length - valid.length;
      if (phones.length === 0) {
        counterEl.textContent = '建议一次不超过 50 个';
        counterEl.className =
          'absolute bottom-3 right-3 text-[10px] text-s400 font-mono bg-white/80 dark:bg-s800/80 px-2 py-0.5 rounded border border-s100 dark:border-s700';
      } else if (invalid > 0) {
        counterEl.textContent = `${valid.length} 个有效 / ${invalid} 个无效`;
        counterEl.className =
          'absolute bottom-3 right-3 text-[10px] text-orange-500 font-mono bg-white/80 dark:bg-s800/80 px-2 py-0.5 rounded border border-orange-200 dark:border-orange-800';
      } else {
        counterEl.textContent = `${valid.length} 个号码`;
        counterEl.className =
          'absolute bottom-3 right-3 text-[10px] text-green-600 font-mono bg-white/80 dark:bg-s800/80 px-2 py-0.5 rounded border border-green-200 dark:border-green-800';
      }
    }

    function showError(msg: string) {
      if (errorTextEl) errorTextEl.textContent = msg;
      errorEl.style.display = 'flex';
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

      try {
        const result = await invoke<{
          status: string;
          phones?: number;
          tasks?: number;
          taskItems?: string[];
          conflicts?: Array<{ mobile?: string; phone?: string; clientId: string }>;
        }>('sync_tasks_by_phones', {
          phones: valid,
          force: forceSync,
        });

        if (result.status === 'conflicts') {
          const message =
            result.conflicts
              ?.map(item => `${item.mobile ?? item.phone ?? ''}（占用端：${item.clientId}）`)
              .join('、') || '';
          showError(`以下号码已在其他客户端绑定：${message}`);
          const confirmed = window.confirm(
            `以下号码已在其他客户端绑定：\n${message}\n\n是否强制绑定并继续同步？`,
          );
          if (confirmed) {
            const forcedResult = await invoke<{ tasks?: number }>('sync_tasks_by_phones', {
              phones: valid,
              force: true,
            });
            if ((forcedResult.tasks ?? 0) === 0) {
              showError(emptyTasksMessage);
              return;
            }
            exit();
          }
          return;
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
    updateCounter();
    textarea.focus();
  });

  return activePhoneBindPromise;
}
