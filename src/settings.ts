import { invoke } from '@tauri-apps/api/core';
import { $, showToast } from './utils';

/* ===== Settings Panel ===== */

export function updateMqttStatusUI(status: string) {
    const dot = $('#mqtt-dot');
    const text = $('#mqtt-status-text');
    const btnConn = $('#btn-mqtt-connect') as HTMLButtonElement;
    const btnDisc = $('#btn-mqtt-disconnect') as HTMLButtonElement;

    if (dot) {
        dot.className = 'mqtt-dot';
        if (status === 'connected') dot.classList.add('connected');
        else if (status.startsWith('error')) dot.classList.add('error');
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

    $('#btn-settings')?.addEventListener('click', async () => {
        try {
            const settings = await invoke<Record<string, string>>('get_settings');
            ($('#set-mqtt-host') as HTMLInputElement).value = settings.mqtt_host || '';
            ($('#set-mqtt-port') as HTMLInputElement).value = settings.mqtt_port || '1883';
            ($('#set-mqtt-client-id') as HTMLInputElement).value = settings.mqtt_client_id || '';
            ($('#set-mqtt-username') as HTMLInputElement).value = settings.mqtt_username || '';
            ($('#set-mqtt-password') as HTMLInputElement).value = settings.mqtt_password || '';
        } catch {
            /* ignore */
        }
        try {
            const s = await invoke<string>('mqtt_status');
            updateMqttStatusUI(s);
        } catch {
            /* ignore */
        }
        settingsOverlay.style.display = 'flex';
    });

    $('#settings-close')?.addEventListener('click', () => (settingsOverlay.style.display = 'none'));
    $('#settings-cancel')?.addEventListener(
        'click',
        () => (settingsOverlay.style.display = 'none'),
    );
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
            mqtt_port: portStr || '1883',
            mqtt_client_id: ($('#set-mqtt-client-id') as HTMLInputElement).value,
            mqtt_username: ($('#set-mqtt-username') as HTMLInputElement).value,
            mqtt_password: ($('#set-mqtt-password') as HTMLInputElement).value,
        };
        try {
            await invoke('save_settings', { settings });
            settingsOverlay.style.display = 'none';
        } catch (e) {
            console.error('Save settings failed:', e);
        }
    });

    $('#btn-mqtt-connect')?.addEventListener('click', async () => {
        try {
            await invoke('mqtt_connect');
            updateMqttStatusUI('connecting');
        } catch (e) {
            updateMqttStatusUI(`error:${e}`);
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
}
