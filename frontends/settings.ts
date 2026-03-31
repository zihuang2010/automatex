import { invoke } from '@tauri-apps/api/core';

import { $, showToast } from './utils';

/* ===== Settings Panel ===== */

const THEME_OPT_ACTIVE = 'bg-white text-s800 shadow-sm';
const THEME_OPT_INACTIVE = 'text-s400 hover:text-s600';
const DEFAULT_MQTT_HOST = '39.98.170.208';
const DEFAULT_MQTT_PORT = '30002';
let themeSwitcherBound = false;
let settingsBound = false;

function normalizeMqttFormValues() {
  const hostInput = $('#set-mqtt-host') as HTMLInputElement;
  const portInput = $('#set-mqtt-port') as HTMLInputElement;
  const clientIdInput = $('#set-mqtt-client-id') as HTMLInputElement;
  const usernameInput = $('#set-mqtt-username') as HTMLInputElement;
  const passwordInput = $('#set-mqtt-password') as HTMLInputElement;

  const normalized = {
    host: hostInput.value.trim() || DEFAULT_MQTT_HOST,
    port: portInput.value.trim() || DEFAULT_MQTT_PORT,
    client_id: clientIdInput.value.trim(),
    username: usernameInput.value.trim(),
    password: passwordInput.value,
  };

  hostInput.value = normalized.host;
  portInput.value = normalized.port;

  return normalized;
}

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
  if (themeSwitcherBound) return;
  themeSwitcherBound = true;
  document.querySelectorAll<HTMLButtonElement>('#theme-switcher .theme-opt').forEach(btn => {
    btn.addEventListener('click', () => {
      const theme = btn.getAttribute('data-theme');
      if (theme === 'dark') {
        document.documentElement.classList.add('dark');
      } else {
        document.documentElement.classList.remove('dark');
      }
      localStorage.setItem('theme', theme!);
      // 同步主题到后端数据库，确保重启后生效
      invoke('save_settings', { settings: { theme } }).catch(() => {});
      syncThemeSwitcher();
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
  if (settingsBound) return;
  settingsBound = true;
  const settingsOverlay = $('#settings-overlay') as HTMLElement;
  initThemeSwitcher();

  // 缓存打开设置面板时的 MQTT 配置快照，用于保存时对比是否有变化
  let savedMqttSnapshot = { host: '', port: '', client_id: '', username: '', password: '' };

  $('#btn-settings')?.addEventListener('click', async () => {
    try {
      const settings = await invoke<Record<string, string>>('get_settings');
      ($('#set-mqtt-host') as HTMLInputElement).value = settings.mqtt_host || DEFAULT_MQTT_HOST;
      ($('#set-mqtt-port') as HTMLInputElement).value = settings.mqtt_port || DEFAULT_MQTT_PORT;
      ($('#set-mqtt-client-id') as HTMLInputElement).value = settings.mqtt_client_id || '';
      ($('#set-mqtt-username') as HTMLInputElement).value = settings.mqtt_username || '';
      ($('#set-mqtt-password') as HTMLInputElement).value = settings.mqtt_password || '';
      ($('#set-api-base-url') as HTMLInputElement).value = settings.api_base_url || '';

      // 记录当前 MQTT 配置快照
      savedMqttSnapshot = {
        host: settings.mqtt_host || DEFAULT_MQTT_HOST,
        port: settings.mqtt_port || DEFAULT_MQTT_PORT,
        client_id: settings.mqtt_client_id || '',
        username: settings.mqtt_username || '',
        password: settings.mqtt_password || '',
      };
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
    const newMqtt = normalizeMqttFormValues();
    const host = newMqtt.host;
    const portStr = newMqtt.port;
    const port = parseInt(portStr, 10);

    // #13: 基础输入校验
    if (host && (isNaN(port) || port < 1 || port > 65535)) {
      showToast('MQTT 端口号必须在 1-65535 之间', 'error');
      return;
    }

    const settings = {
      mqtt_host: newMqtt.host,
      mqtt_port: newMqtt.port,
      mqtt_client_id: newMqtt.client_id,
      mqtt_username: newMqtt.username,
      mqtt_password: newMqtt.password,
      api_base_url: ($('#set-api-base-url') as HTMLInputElement).value.trim(),
    };
    try {
      await invoke('save_settings', { settings });
      settingsOverlay.style.display = 'none';

      // 仅当 MQTT 配置实际发生变化时才重连
      const mqttChanged =
        newMqtt.host !== savedMqttSnapshot.host ||
        newMqtt.port !== savedMqttSnapshot.port ||
        newMqtt.client_id !== savedMqttSnapshot.client_id ||
        newMqtt.username !== savedMqttSnapshot.username ||
        newMqtt.password !== savedMqttSnapshot.password;

      if (host && mqttChanged) {
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
    const newMqtt = normalizeMqttFormValues();
    const settings = {
      mqtt_host: newMqtt.host,
      mqtt_port: newMqtt.port,
      mqtt_client_id: newMqtt.client_id,
      mqtt_username: newMqtt.username,
      mqtt_password: newMqtt.password,
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
