#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const target = process.argv[2] || detectDefaultTarget();

const adbName = target.includes('windows') ? `${target}.exe` : target;
const adbPath = path.join(rootDir, 'backends', 'binaries', `adb-${adbName}`);
const scrcpyServerPath = path.join(rootDir, 'backends', 'resources', 'scrcpy-server');
const winApiPath = path.join(rootDir, 'backends', 'resources', 'windows', 'AdbWinApi.dll');
const winUsbPath = path.join(rootDir, 'backends', 'resources', 'windows', 'AdbWinUsbApi.dll');

function detectDefaultTarget() {
  if (process.platform === 'darwin') {
    return process.arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin';
  }
  if (process.platform === 'win32') {
    return 'x86_64-pc-windows-msvc';
  }
  return `${process.arch}-${process.platform}`;
}

function fail(message) {
  console.error(`[package:doctor] ${message}`);
  process.exitCode = 1;
}

function info(message) {
  console.log(`[package:doctor] ${message}`);
}

function checkFile(filePath, label, { required = true, minSize = 1 } = {}) {
  if (!fs.existsSync(filePath)) {
    if (required) {
      fail(`${label} 缺失: ${filePath}`);
    } else {
      info(`${label} 未提供: ${filePath}`);
    }
    return;
  }

  const stat = fs.statSync(filePath);
  if (stat.size < minSize) {
    fail(`${label} 文件异常，大小仅 ${stat.size} bytes: ${filePath}`);
    return;
  }
  info(`${label} OK (${Math.round(stat.size / 1024)} KB)`);
}

info(`检查目标平台资源: ${target}`);
checkFile(adbPath, 'ADB sidecar', { minSize: 100_000 });
checkFile(scrcpyServerPath, 'scrcpy-server', { minSize: 50_000 });

if (target.includes('windows')) {
  checkFile(winApiPath, 'AdbWinApi.dll', { required: false, minSize: 10_000 });
  checkFile(winUsbPath, 'AdbWinUsbApi.dll', { required: false, minSize: 10_000 });
  if (!fs.existsSync(winApiPath) || !fs.existsSync(winUsbPath)) {
    info('提示: Windows 便携包建议补齐 AdbWinApi.dll 与 AdbWinUsbApi.dll');
  }
}

if (process.exitCode && process.exitCode !== 0) {
  process.exit(process.exitCode);
}

info('资源检查通过');
