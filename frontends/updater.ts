/**
 * 自动更新前端模块
 *
 * 后端在启动后延迟检查 → emit `updater://available` 时弹出模态框。
 * 用户可选「立即更新 / 稍后提醒 / 跳过此版本」。
 */
import { invoke } from '@tauri-apps/api/core';
import { type UnlistenFn, listen } from '@tauri-apps/api/event';

import { showToast } from './utils';

interface UpdateInfo {
  version: string;
  current_version: string;
  notes?: string | null;
  pub_date?: string | null;
}

interface UpdateProgress {
  downloaded: number;
  total?: number | null;
}

const SKIPPED_VERSIONS_KEY = 'automatex.updater.skippedVersions';

let activeInfo: UpdateInfo | null = null;
let installing = false;

function getSkippedVersions(): string[] {
  try {
    const raw = localStorage.getItem(SKIPPED_VERSIONS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === 'string') : [];
  } catch {
    return [];
  }
}

function addSkippedVersion(version: string) {
  const list = getSkippedVersions();
  if (!list.includes(version)) {
    list.push(version);
    localStorage.setItem(SKIPPED_VERSIONS_KEY, JSON.stringify(list));
  }
}

function showUpdateModal(info: UpdateInfo) {
  const modal = document.getElementById('update-modal') as HTMLElement | null;
  if (!modal) {
    console.warn('[updater] #update-modal 不存在，无法显示更新弹窗');
    return;
  }

  activeInfo = info;
  installing = false;

  const versionEl = document.getElementById('update-modal-version');
  const currentEl = document.getElementById('update-modal-current');
  const notesEl = document.getElementById('update-modal-notes');
  const progressWrap = document.getElementById('update-modal-progress');
  const progressBar = document.getElementById('update-modal-progress-bar');
  const progressText = document.getElementById('update-modal-progress-text');
  const installBtn = document.getElementById('update-modal-install') as HTMLButtonElement | null;
  const laterBtn = document.getElementById('update-modal-later') as HTMLButtonElement | null;
  const skipBtn = document.getElementById('update-modal-skip') as HTMLButtonElement | null;

  if (versionEl) versionEl.textContent = info.version;
  if (currentEl) currentEl.textContent = info.current_version;
  if (notesEl) {
    const md = info.notes?.trim();
    notesEl.innerHTML = md
      ? renderNotesMarkdown(md)
      : '<p class="text-s500">本次更新没有提供说明。</p>';
  }
  if (progressWrap) progressWrap.style.display = 'none';
  if (progressBar) progressBar.style.width = '0%';
  if (progressText) progressText.textContent = '';
  if (installBtn) {
    installBtn.disabled = false;
    installBtn.textContent = '立即更新';
  }
  if (laterBtn) laterBtn.disabled = false;
  if (skipBtn) skipBtn.disabled = false;

  modal.style.display = 'flex';
  installBtn?.focus({ preventScroll: true });
}

function hideUpdateModal() {
  const modal = document.getElementById('update-modal') as HTMLElement | null;
  if (modal) modal.style.display = 'none';
  activeInfo = null;
  installing = false;
}

function setProgress(progress: UpdateProgress) {
  const progressWrap = document.getElementById('update-modal-progress');
  const progressBar = document.getElementById('update-modal-progress-bar');
  const progressText = document.getElementById('update-modal-progress-text');
  if (!progressWrap || !progressBar || !progressText) return;

  progressWrap.style.display = 'block';
  if (progress.total && progress.total > 0) {
    const pct = Math.min(100, Math.round((progress.downloaded / progress.total) * 100));
    progressBar.style.width = `${pct}%`;
    progressText.textContent = `${formatBytes(progress.downloaded)} / ${formatBytes(progress.total)} (${pct}%)`;
  } else {
    progressBar.style.width = '100%';
    progressText.textContent = `${formatBytes(progress.downloaded)} 已下载`;
  }
}

/**
 * 极简 Markdown 渲染：覆盖 CHANGELOG 里实际用到的语法
 * - `### header` → 小标题
 * - `- item` → bullet 列表
 * - `` `code` `` → inline code
 * - 其他行 → 段落
 *
 * 安全性：先 HTML escape 再做替换，避免 XSS。
 * 内容来源是 OSS 上由我们控制的 CHANGELOG，非用户输入；escape 是兜底。
 */
function renderNotesMarkdown(md: string): string {
  const escape = (s: string) =>
    s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

  const inlineCode = (escaped: string) =>
    escaped.replace(
      /`([^`]+)`/g,
      '<code class="bg-s100 text-s700 rounded px-1 py-0.5 font-mono text-[11px]">$1</code>',
    );

  const lines = md.split('\n');
  const out: string[] = [];
  let inList = false;
  const closeList = () => {
    if (inList) {
      out.push('</ul>');
      inList = false;
    }
  };

  for (const raw of lines) {
    const line = raw.trim();
    if (!line) {
      closeList();
      continue;
    }
    // ### / ## 标题
    const headerMatch = line.match(/^(#{2,4})\s+(.+)$/);
    if (headerMatch) {
      closeList();
      const level = headerMatch[1].length;
      const text = inlineCode(escape(headerMatch[2].trim()));
      const cls =
        level === 2
          ? 'text-s800 mt-3 mb-1 text-[14px] font-bold first:mt-0'
          : 'text-s700 mt-3 mb-1 text-[12.5px] font-bold first:mt-0';
      out.push(`<h4 class="${cls}">${text}</h4>`);
      continue;
    }
    // - bullet
    if (line.startsWith('- ') || line.startsWith('* ')) {
      if (!inList) {
        out.push(
          '<ul class="text-s600 my-1 list-outside list-disc space-y-1 pl-4 leading-relaxed">',
        );
        inList = true;
      }
      out.push(`<li>${inlineCode(escape(line.slice(2).trim()))}</li>`);
      continue;
    }
    // 段落
    closeList();
    out.push(`<p class="text-s600 my-1 leading-relaxed">${inlineCode(escape(line))}</p>`);
  }
  closeList();
  return out.join('\n');
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

async function startInstall() {
  if (installing || !activeInfo) return;
  installing = true;

  const installBtn = document.getElementById('update-modal-install') as HTMLButtonElement | null;
  const laterBtn = document.getElementById('update-modal-later') as HTMLButtonElement | null;
  const skipBtn = document.getElementById('update-modal-skip') as HTMLButtonElement | null;
  if (installBtn) {
    installBtn.disabled = true;
    installBtn.textContent = '下载中...';
  }
  if (laterBtn) laterBtn.disabled = true;
  if (skipBtn) skipBtn.disabled = true;

  try {
    await invoke('install_update');
    // 成功后后端会触发 app.restart()，此处通常不会执行到
  } catch (err) {
    installing = false;
    showToast(`更新失败: ${err}`, 'error');
    if (installBtn) {
      installBtn.disabled = false;
      installBtn.textContent = '重试';
    }
    if (laterBtn) laterBtn.disabled = false;
    if (skipBtn) skipBtn.disabled = false;
  }
}

function bindModalActions() {
  const installBtn = document.getElementById('update-modal-install');
  const laterBtn = document.getElementById('update-modal-later');
  const skipBtn = document.getElementById('update-modal-skip');

  if (installBtn && !installBtn.dataset.bound) {
    installBtn.dataset.bound = '1';
    installBtn.addEventListener('click', () => {
      void startInstall();
    });
  }
  if (laterBtn && !laterBtn.dataset.bound) {
    laterBtn.dataset.bound = '1';
    laterBtn.addEventListener('click', () => {
      if (installing) return;
      hideUpdateModal();
    });
  }
  if (skipBtn && !skipBtn.dataset.bound) {
    skipBtn.dataset.bound = '1';
    skipBtn.addEventListener('click', () => {
      if (installing || !activeInfo) return;
      addSkippedVersion(activeInfo.version);
      hideUpdateModal();
    });
  }
}

/**
 * 注册更新事件监听器，返回的 unlisteners 应交由 main.ts 的 appUnlisteners 统一管理。
 */
export async function setupUpdaterListeners(): Promise<UnlistenFn[]> {
  bindModalActions();

  const unAvailable = await listen<UpdateInfo>('updater://available', event => {
    const info = event.payload;
    if (getSkippedVersions().includes(info.version)) {
      console.warn(`[updater] 跳过已忽略的版本: ${info.version}`);
      return;
    }
    showUpdateModal(info);
  });

  const unProgress = await listen<UpdateProgress>('updater://progress', event => {
    setProgress(event.payload);
  });

  const unError = await listen<string>('updater://error', event => {
    installing = false;
    showToast(`更新失败: ${event.payload}`, 'error');
  });

  return [unAvailable, unProgress, unError];
}

/**
 * 用户主动点击「检查更新」时调用。无新版本时给出 toast 提示。
 */
export async function checkForUpdateManually() {
  try {
    const result = await invoke<{ available: boolean; info: UpdateInfo | null }>(
      'check_for_update',
    );
    if (result.available && result.info) {
      showUpdateModal(result.info);
    } else {
      showToast('当前已是最新版本', 'info');
    }
  } catch (err) {
    showToast(`检查更新失败: ${err}`, 'error');
  }
}
