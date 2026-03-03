/**
 * AutomateX — Transition Page Module
 *
 * 任务同步过渡页：首次启动或无本地手机号时，
 * 在开屏动画结束后显示，让用户输入手机号并开始同步任务。
 */

import { invoke } from '@tauri-apps/api/core';
import { $ } from './utils';

/* ===== 手机号验证 ===== */

/** 验证单个手机号格式（中国大陆 11 位） */
function isValidPhone(phone: string): boolean {
  return /^1[3-9]\d{9}$/.test(phone.trim());
}

/** 从 textarea 内容解析有效手机号列表 */
function parsePhones(raw: string): string[] {
  return raw
    .split(/[\n,;，；\s]+/)
    .map(s => s.trim())
    .filter(s => s.length > 0);
}

/* ===== 核心逻辑 ===== */

/** 检查是否需要显示过渡页（无已同步手机号） */
export async function needsTransition(): Promise<boolean> {
  try {
    const settings = await invoke<Record<string, string>>('get_settings');
    const phonesStr = settings.synced_phones || '[]';
    const phones: string[] = JSON.parse(phonesStr);
    return phones.length === 0;
  } catch (e) {
    console.warn('[transition] 读取设置失败，默认显示过渡页:', e);
    return true;
  }
}

/** 显示过渡页，返回 Promise 在用户完成操作后 resolve */
export function showTransition(): Promise<void> {
  return new Promise(resolve => {
    const el = $('#transition')!;
    const app = $('#app')!;

    // 显示过渡页
    el.style.display = 'flex';
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        el.classList.add('show');
      });
    });

    const textarea = el.querySelector('#phone-numbers') as HTMLTextAreaElement;
    const errorEl = el.querySelector('#transition-error') as HTMLElement;
    const counterEl = el.querySelector('#phone-counter') as HTMLElement;

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
      errorEl.textContent = msg;
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
        el.remove();
      }, 500);
      resolve();
    }

    // ── 事件绑定 ──

    textarea.addEventListener('input', () => {
      hideError();
      updateCounter();
    });

    // 开始同步
    el.querySelector('#btn-sync-start')?.addEventListener('click', async () => {
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
          conflicts?: Array<{ phone: string; current_client: string }>;
        }>('sync_tasks_by_phones', {
          phones: valid,
          force: false,
        });

        if (result.status === 'conflicts') {
          const conflictPhones = result.conflicts?.map(c => c.phone).join('、') || '';
          showError(`以下号码已在其他客户端绑定：${conflictPhones}。如需强制抢占，请重新提交。`);
          // TODO: 可添加"强制抢占"按钮，调用 sync_tasks_by_phones(force=true)
          return;
        }

        console.log('[transition] 同步完成:', result.phones, '个手机号,', result.tasks, '个任务');
        exit();
      } catch (e) {
        showError(`同步失败: ${e}`);
      }
    });

    // 重置
    el.querySelector('#btn-sync-reset')?.addEventListener('click', () => {
      textarea.value = '';
      hideError();
      updateCounter();
      textarea.focus();
    });
  });
}
