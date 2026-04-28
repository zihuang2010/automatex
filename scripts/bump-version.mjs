#!/usr/bin/env node
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(__dirname, '..');

const PACKAGE_JSON = resolve(ROOT, 'package.json');
const CARGO_TOML = resolve(ROOT, 'backends/Cargo.toml');
const TAURI_CONF = resolve(ROOT, 'backends/tauri.conf.json');

const SEMVER_RE = /^\d+\.\d+\.\d+(?:-[\w.+-]+)?$/;

function fail(msg) {
  console.error(`[bump-version] ${msg}`);
  process.exit(1);
}

const next = process.argv[2];
if (!next) fail('Usage: node scripts/bump-version.mjs <version>  (e.g. 1.0.3)');
if (!SEMVER_RE.test(next)) fail(`Invalid semver: ${next}`);

const pkg = JSON.parse(readFileSync(PACKAGE_JSON, 'utf8'));
const prev = pkg.version;
pkg.version = next;
writeFileSync(PACKAGE_JSON, JSON.stringify(pkg, null, 2) + '\n', 'utf8');

const cargo = readFileSync(CARGO_TOML, 'utf8');
const cargoPatched = cargo.replace(
  /^(version\s*=\s*")[^"]+(")/m,
  `$1${next}$2`,
);
if (cargoPatched === cargo) fail('Failed to update version in backends/Cargo.toml');
writeFileSync(CARGO_TOML, cargoPatched, 'utf8');

const conf = JSON.parse(readFileSync(TAURI_CONF, 'utf8'));
conf.version = next;
writeFileSync(TAURI_CONF, JSON.stringify(conf, null, 2) + '\n', 'utf8');

console.log(`[bump-version] ${prev} -> ${next}`);
console.log(`  package.json           : ${pkg.version}`);
console.log(`  backends/Cargo.toml    : ${next}`);
console.log(`  backends/tauri.conf.json: ${conf.version}`);
