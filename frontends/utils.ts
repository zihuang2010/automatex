import { DeviceRow } from './types';

/* ===== Utility Functions ===== */

/** DOM 选择器简写 */
export const $ = (s: string) => document.querySelector(s) as HTMLElement | null;

/** HTML 转义，防止 XSS */
export function esc(t: string): string {
    return t
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
}

/** 等待浏览器完成一帧渲染（双 rAF 保证 paint 完成） */
export function nextFrame(): Promise<void> {
    return new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r())));
}

/** 将 unix 时间戳（秒）转为中文相对时间 */
export function timeAgo(unixSec: number): string {
    const diff = Math.floor(Date.now() / 1000) - unixSec;
    if (diff < 60) return '刚刚';
    if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
    if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
    return `${Math.floor(diff / 86400)} 天前`;
}

/** 将 Unix 时间戳（秒）格式化为友好的执行时间 */
export function formatRunTime(ts: number): { time: string; date: string } {
    const d = new Date(ts * 1000);
    const hh = String(d.getHours()).padStart(2, '0');
    const mm = String(d.getMinutes()).padStart(2, '0');
    const time = `${hh}:${mm}`;
    const mo = d.getMonth() + 1;
    const dd = d.getDate();
    const date = `${mo}月${dd}日`;
    return { time, date };
}

/** 获取设备显示名称 */
export function getDeviceName(d: DeviceRow): string {
    if (d.brand !== 'unknown' && d.model !== 'unknown') return `${d.brand} ${d.model}`;
    if (d.model !== 'unknown') return d.model;
    if (d.name && d.name !== d.serial) return d.name;
    return d.serial;
}

/** UI 内 Toast 提示（替代 alert，3 秒自动消失） */
export function showToast(msg: string, type: 'info' | 'warning' | 'error' = 'warning') {
    const colorMap = {
        info: 'bg-blue-50/80 backdrop-blur-md text-blue-700 border-white/50 shadow-md shadow-blue-900/5',
        warning:
            'bg-amber-50/80 backdrop-blur-md text-amber-800 border-white/50 shadow-md shadow-amber-900/5',
        error: 'bg-red-50/80 backdrop-blur-md text-red-700 border-white/50 shadow-md shadow-red-900/5',
    };

    const iconMap = { info: 'info', warning: 'warning', error: 'error' };

    const iconColorMap = {
        info: 'text-blue-500',
        warning: 'text-amber-600',
        error: 'text-red-500',
    };

    // 移除上一个未消失的 toast，防止堆叠
    document.querySelector('.toast-auto')?.remove();

    const toast = document.createElement('div');
    toast.className = `toast-auto fixed top-6 left-1/2 -translate-x-1/2 z-[9999] px-6 py-3 rounded-full border ${colorMap[type]} text-xs font-medium shadow-md transition-all duration-300 opacity-0 -translate-y-4 flex items-center gap-2.5`;
    toast.innerHTML = `<span class="material-symbols-outlined text-base ${iconColorMap[type]}">${iconMap[type]}</span><span>${esc(msg)}</span>`;
    document.body.appendChild(toast);

    requestAnimationFrame(() => {
        requestAnimationFrame(() => {
            toast.classList.remove('opacity-0', '-translate-y-4');
            toast.classList.add('opacity-100', 'translate-y-0');
        });
    });

    setTimeout(() => {
        toast.classList.remove('opacity-100', 'translate-y-0');
        toast.classList.add('opacity-0', '-translate-y-4');
        setTimeout(() => toast.remove(), 300);
    }, 3000);
}
