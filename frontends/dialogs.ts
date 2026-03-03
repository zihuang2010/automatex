import { invoke } from '@tauri-apps/api/core';
import { DeviceRow } from './types';
import { selectedDevice, setSelectedDevice } from './state';
import { $, esc, showToast } from './utils';
import { refreshDevices, updateCardSelection } from './devices';

/* ===== Add Device Dialog ===== */

export function showAddDeviceDialog() {
  const modal = $('#add-device-modal')!;
  const addrInput = $('#add-device-addr') as HTMLInputElement;
  const nameInput = $('#add-device-name') as HTMLInputElement;
  const errorDiv = $('#add-device-error')!;
  modal.style.display = 'flex';
  addrInput.value = '';
  nameInput.value = '';
  errorDiv.style.display = 'none';
  addrInput.focus();
}

export function hideAddDeviceDialog() {
  ($('#add-device-modal') as HTMLElement).style.display = 'none';
}

export async function submitAddDevice() {
  const addrInput = $('#add-device-addr') as HTMLInputElement;
  const nameInput = $('#add-device-name') as HTMLInputElement;
  const errorDiv = $('#add-device-error')!;
  const submitBtn = $('#add-device-submit') as HTMLButtonElement;

  const addr = addrInput.value.trim();
  if (!addr) {
    const textSpan = errorDiv.querySelector('span:last-child');
    if (textSpan) textSpan.textContent = '请输入设备地址';
    else errorDiv.textContent = '请输入设备地址';
    errorDiv.style.display = 'flex';
    return;
  }

  submitBtn.disabled = true;
  submitBtn.innerHTML = '<span class="spinner-sm"></span> 连接中…';
  errorDiv.style.display = 'none';

  try {
    await invoke('add_device', { address: addr, name: nameInput.value.trim() });
    hideAddDeviceDialog();
    showToast('设备添加成功', 'info');
    await refreshDevices();
  } catch (e) {
    const textSpan = errorDiv.querySelector('span:last-child');
    if (textSpan) textSpan.textContent = String(e);
    else errorDiv.textContent = String(e);
    errorDiv.style.display = 'flex';
  } finally {
    submitBtn.disabled = false;
    submitBtn.innerHTML =
      '<span class="flex items-center gap-1.5"><span class="material-symbols-outlined text-[14px]">link</span>连接</span>';
  }
}

/* ===== Remove Device ===== */

export async function removeSelectedDevice() {
  if (!selectedDevice) return;
  try {
    await invoke('remove_device', { serial: selectedDevice });
    setSelectedDevice(null);
    updateCardSelection();
    showToast('设备已移除', 'info');
    await refreshDevices();
  } catch (e) {
    showToast(`移除失败: ${e}`, 'error');
  }
}

/* ===== Unflag Device ===== */

export async function unflagSelectedDevice() {
  if (!selectedDevice) return;
  try {
    await invoke('unflag_device', { serial: selectedDevice });
    showToast('设备风控标记已解除', 'info');
    setSelectedDevice(null);
    const unflagBtn = $('#btn-unflag-device') as HTMLButtonElement;
    const removeBtn = $('#btn-remove-selected') as HTMLButtonElement;
    if (unflagBtn) unflagBtn.disabled = true;
    if (removeBtn) removeBtn.disabled = true;
    await refreshDevices();
  } catch (e) {
    showToast(`解除失败: ${e}`, 'error');
  }
}

/* ===== Device Info Modal ===== */

export async function showDeviceInfo(serial: string) {
  const m = $('#modal')!;
  const b = $('#modal-body')!;
  m.style.display = 'flex';
  b.innerHTML = '<div class="text-center p-4.5"><span class="spinner"></span></div>';
  try {
    const i: DeviceRow = await invoke('get_device_info', { serial });
    const typeLabel = i.device_type === 'usb' ? 'USB' : 'WiFi';
    const typeIcon =
      i.device_type === 'usb'
        ? '<span class="material-symbols-outlined text-base">usb</span>'
        : '<span class="material-symbols-outlined text-base">wifi</span>';
    const batteryPct = i.battery_level ?? 0;
    const batteryColor = batteryPct < 30 ? 'text-orange-500' : 'text-green-600';
    const tempVal = i.battery_temperature ?? 0;
    const tempColor = tempVal > 40 ? 'text-orange-500' : 'text-s500';

    const flaggedBanner = i.is_flagged
      ? `<div class="flex items-center gap-2 px-3 py-2 bg-orange-50 border border-orange-200 rounded-lg mb-4">
                <span class="material-symbols-outlined text-orange-500 icon-sm text-base">warning</span>
                <span class="text-[12px] font-bold text-orange-600">设备已被标记为风控，请检查后手动解除标记。</span>
               </div>`
      : '';

    b.innerHTML = `
      ${flaggedBanner}
      <!-- Device Header -->
      <div class="flex items-center gap-3 mb-5">
        <div class="w-12 h-12 rounded-lg bg-gradient-to-br from-blue-500 to-blue-700 flex items-center justify-center shrink-0 shadow-[0_4px_12px_rgba(37,99,235,.3)]">
          <span class="material-symbols-outlined text-2xl text-white">smartphone</span>
        </div>
        <div>
          <div class="text-[15px] font-bold text-s900">${esc(i.brand)} ${esc(i.model)}</div>
          <div class="flex items-center gap-1.5 mt-0.5">
            <span class="inline-flex items-center gap-1 text-[11px] font-semibold text-s400">${typeIcon} ${typeLabel}</span>
            <span class="text-s200">·</span>
            <span class="text-[11px] font-mono text-s400">${esc(i.serial)}</span>
          </div>
        </div>
      </div>

      <!-- Stats Row -->
      <div class="flex gap-2 mb-5">
        <div class="flex-1 px-3 py-2.5 rounded-lg bg-s50 border border-s100 text-center">
          <div class="text-[10px] font-bold text-s400 uppercase tracking-wider mb-1">电量</div>
          <div class="text-[16px] font-bold ${batteryColor}">${batteryPct}%</div>
        </div>
        <div class="flex-1 px-3 py-2.5 rounded-lg bg-s50 border border-s100 text-center">
          <div class="text-[10px] font-bold text-s400 uppercase tracking-wider mb-1">温度</div>
          <div class="text-[16px] font-bold ${tempColor}">${tempVal}°C</div>
        </div>
        <div class="flex-1 px-3 py-2.5 rounded-lg bg-s50 border border-s100 text-center">
          <div class="text-[10px] font-bold text-s400 uppercase tracking-wider mb-1">Android</div>
          <div class="text-[14px] font-bold text-s700">${esc(i.android_version)}</div>
        </div>
      </div>

      <!-- Detail Rows -->
      <div class="flex flex-col gap-0 rounded-lg border border-s100 overflow-hidden">
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-white border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">序列号</span>
          <span class="text-[12px] font-mono font-medium text-s700">${esc(i.serial)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-s50 border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">品牌</span>
          <span class="text-[12px] font-medium text-s700">${esc(i.brand)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-white border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">型号</span>
          <span class="text-[12px] font-medium text-s700">${esc(i.model)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-s50 border-b border-s100">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">SDK 版本</span>
          <span class="text-[12px] font-mono font-medium text-s700">${esc(i.sdk_version)}</span>
        </div>
        <div class="flex items-center justify-between px-3.5 py-2.5 bg-white">
          <span class="text-[11px] font-semibold text-s400 uppercase tracking-wide">分辨率</span>
          <span class="text-[12px] font-mono font-medium text-s700">${esc(i.display_resolution)}</span>
        </div>
      </div>`;
  } catch (e) {
    b.innerHTML = `<p class="text-red-500 text-center py-4">获取失败: ${e}</p>`;
  }
}
