import { invoke } from '@tauri-apps/api/core';
import { $, showToast } from './utils';

/* ===== Settings Panel ===== */

const THEME_OPT_ACTIVE = 'bg-white text-s800 shadow-sm';
const THEME_OPT_INACTIVE = 'text-s400 hover:text-s600';

function syncThemeSwitcher() {
  const isDark = document.documentElement.classList.contains('dark');
  document.querySelectorAll<HTMLButtonElement>('#theme-switcher .theme-opt').forEach(btn => {
    const theme = btn.getAttribute('data-theme');
    btn.className = `theme-opt px-3 py-1 text-[11px] font-medium rounded transition-all flex items-center gap-1 ${
      (theme === 'dark' && isDark) || (theme === 'light' && !isDark)
        ? THEME_OPT_ACTIVE
        : THEME_OPT_INACTIVE
    }`;
  });
}

function initThemeSwitcher() {
  document.querySelectorAll<HTMLButtonElement>('#theme-switcher .theme-opt').forEach(btn => {
    btn.addEventListener('click', () => {
      const theme = btn.getAttribute('data-theme');
      if (theme === 'dark') {
        document.documentElement.classList.add('dark');
      } else {
        document.documentElement.classList.remove('dark');
      }
      localStorage.setItem('theme', theme!);
      syncThemeSwitcher();
      // 同步 header 图标
      const icon = document.getElementById('theme-icon');
      if (icon) icon.textContent = theme === 'dark' ? 'light_mode' : 'dark_mode';
    });
  });
}

export function updateMqttStatusUI(status: string) {
  const dot = $('#mqtt-dot');
  const text = $('#mqtt-status-text');
  const btnConn = $('#btn-mqtt-connect') as HTMLButtonElement;
  const btnDisc = $('#btn-mqtt-disconnect') as HTMLButtonElement;

  if (dot) {
    dot.className = 'relative inline-flex rounded-full h-1.5 w-1.5';
    const ping = $('#mqtt-ping');
    if (status === 'connected') {
      dot.classList.add('bg-emerald-400');
      if (ping)
        ping.className =
          'animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-75';
    } else if (status.startsWith('error')) {
      dot.classList.add('bg-rose-400');
      if (ping)
        ping.className = 'absolute inline-flex h-full w-full rounded-full bg-rose-400 opacity-75';
    } else {
      dot.classList.add('bg-gray-300');
      if (ping)
        ping.className =
          'animate-ping absolute inline-flex h-full w-full rounded-full bg-gray-300 opacity-75';
    }
  }
  if (text) {
    if (status === 'connected') text.textContent = '已连接';
    else if (status === 'connecting') text.textContent = '连接中...';
    else if (status === 'disconnected') text.textContent = '未连接';
    else if (status.startsWith('error:')) text.textContent = `错误: ${status.slice(6)}`;
    else text.textContent = status;
  }
  if (btnConn) btnConn.disabled = status === 'connected' || status === 'connecting';
  if (btnDisc) btnDisc.disabled = status !== 'connected';
}

export function initSettings() {
  const settingsOverlay = $('#settings-overlay') as HTMLElement;
  initThemeSwitcher();

  $('#btn-settings')?.addEventListener('click', async () => {
    try {
      const settings = await invoke<Record<string, string>>('get_settings');
      ($('#set-mqtt-host') as HTMLInputElement).value = settings.mqtt_host || '';
      ($('#set-mqtt-port') as HTMLInputElement).value = settings.mqtt_port || '30002';
      ($('#set-mqtt-client-id') as HTMLInputElement).value = settings.mqtt_client_id || '';
      ($('#set-mqtt-username') as HTMLInputElement).value = settings.mqtt_username || '';
      ($('#set-mqtt-password') as HTMLInputElement).value = settings.mqtt_password || '';
      ($('#set-api-base-url') as HTMLInputElement).value = settings.api_base_url || '';
    } catch {
      /* ignore */
    }
    try {
      const s = await invoke<string>('mqtt_status');
      updateMqttStatusUI(s);
    } catch {
      /* ignore */
    }
    // 同步主题按钮状态
    syncThemeSwitcher();
    settingsOverlay.style.display = 'flex';
  });

  $('#settings-close')?.addEventListener('click', () => (settingsOverlay.style.display = 'none'));
  $('#settings-cancel')?.addEventListener('click', () => (settingsOverlay.style.display = 'none'));
  settingsOverlay?.addEventListener('click', e => {
    if (e.target === e.currentTarget) settingsOverlay.style.display = 'none';
  });

  $('#settings-save')?.addEventListener('click', async () => {
    const host = ($('#set-mqtt-host') as HTMLInputElement).value.trim();
    const portStr = ($('#set-mqtt-port') as HTMLInputElement).value.trim();
    const port = parseInt(portStr, 10);

    // #13: 基础输入校验
    if (host && (isNaN(port) || port < 1 || port > 65535)) {
      showToast('MQTT 端口号必须在 1-65535 之间', 'error');
      return;
    }

    const settings = {
      mqtt_host: host,
      mqtt_port: portStr || '30002',
      mqtt_client_id: ($('#set-mqtt-client-id') as HTMLInputElement).value,
      mqtt_username: ($('#set-mqtt-username') as HTMLInputElement).value,
      mqtt_password: ($('#set-mqtt-password') as HTMLInputElement).value,
      api_base_url: ($('#set-api-base-url') as HTMLInputElement).value.trim(),
    };
    try {
      await invoke('save_settings', { settings });
      settingsOverlay.style.display = 'none';

      // 保存后自动连接 MQTT（如果配置了主机地址）
      if (host) {
        try {
          // 先断开旧连接（忽略错误）
          try {
            await invoke('mqtt_disconnect');
          } catch {
            /* ignore */
          }
          await invoke('mqtt_connect');
          updateMqttStatusUI('connecting');
          showToast('MQTT 正在连接...', 'info');
        } catch (e) {
          updateMqttStatusUI(`error:${e}`);
          showToast(`MQTT 连接失败: ${e}`, 'error');
        }
      }
    } catch (e) {
      console.error('Save settings failed:', e);
      showToast('保存失败', 'error');
    }
  });

  $('#btn-mqtt-connect')?.addEventListener('click', async () => {
    const btn = $('#btn-mqtt-connect') as HTMLButtonElement;
    btn.disabled = true;
    btn.textContent = '连接中...';
    updateMqttStatusUI('connecting');

    // 先保存当前表单值
    const settings = {
      mqtt_host: ($('#set-mqtt-host') as HTMLInputElement).value.trim(),
      mqtt_port: ($('#set-mqtt-port') as HTMLInputElement).value.trim() || '30002',
      mqtt_client_id: ($('#set-mqtt-client-id') as HTMLInputElement).value,
      mqtt_username: ($('#set-mqtt-username') as HTMLInputElement).value,
      mqtt_password: ($('#set-mqtt-password') as HTMLInputElement).value,
      api_base_url: ($('#set-api-base-url') as HTMLInputElement).value.trim(),
    };
    try {
      await invoke('save_settings', { settings });
    } catch {
      /* ignore */
    }

    // 断开旧连接再重连
    try {
      try {
        await invoke('mqtt_disconnect');
      } catch {
        /* ignore */
      }
      await invoke('mqtt_connect');
      updateMqttStatusUI('connecting');
    } catch (e) {
      updateMqttStatusUI(`error:${e}`);
      showToast(`连接失败: ${e}`, 'error');
    } finally {
      btn.disabled = false;
      btn.textContent = '测试连接';
    }
  });

  $('#btn-mqtt-disconnect')?.addEventListener('click', async () => {
    try {
      await invoke('mqtt_disconnect');
      updateMqttStatusUI('disconnected');
    } catch (e) {
      console.error('MQTT disconnect failed:', e);
    }
  });

  // ── Settings tab navigation ──
  document.querySelectorAll<HTMLButtonElement>('[data-settings-tab]').forEach(btn => {
    btn.addEventListener('click', () => {
      const tabId = btn.getAttribute('data-settings-tab')!;
      // Update nav active state
      document.querySelectorAll('.settings-nav-item').forEach(el => el.classList.remove('active'));
      btn.classList.add('active');
      // Show/hide content panels
      document.querySelectorAll('.settings-tab-content').forEach(panel => {
        const el = panel as HTMLElement;
        if (el.getAttribute('data-tab-id') === tabId) {
          el.classList.remove('hidden');
        } else {
          el.classList.add('hidden');
        }
      });
    });
  });
}
