// Installs the browser in browser.lock.json, refusing an archive whose hash differs. With
// --update, after Playwright has moved, it writes that file again for the browser the new
// Playwright names: review the versions, and commit it with the bump.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, createWriteStream, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { fileURLToPath } from 'node:url';

import { BROWSER_DIR, LOCK_PATH, platform, readLock } from './browser-lock.mjs';

const E2E = fileURLToPath(new URL('../', import.meta.url));
const MIN_AGE_DAYS = 7;
const DAY_MS = 86_400_000;

// Playwright's name for each platform, to ask it where each one's browser is.
const PLATFORMS = {
  'darwin-arm64': 'mac15-arm64',
  'darwin-x64': 'mac15',
  'linux-x64': 'ubuntu24.04-x64',
  'linux-arm64': 'ubuntu24.04-arm64',
};

const playwrightVersion = () =>
  JSON.parse(readFileSync(`${E2E}node_modules/playwright-core/package.json`, 'utf8')).version;

async function download(url, path) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} answered ${response.status}`);
  await pipeline(Readable.fromWeb(response.body), createWriteStream(path));
  return response.headers.get('last-modified');
}

async function sha256Of(path) {
  const hash = createHash('sha256');
  await pipeline(createReadStream(path), hash);
  return hash.digest('hex');
}

async function install() {
  const lock = readLock();
  const installed = playwrightVersion();
  if (lock.playwright !== installed) {
    throw new Error(
      `browser.lock.json is for Playwright ${lock.playwright}, and ${installed} is installed: ` +
        'run node support/install-browser.mjs --update in staging/e2e, review it and commit it',
    );
  }
  const entry = lock.archives[platform()];
  if (!entry) throw new Error(`browser.lock.json has no browser for ${platform()}`);
  mkdirSync(BROWSER_DIR, { recursive: true });
  const archive = `${BROWSER_DIR}${entry.sha256}.zip`;
  if (!existsSync(archive)) await download(entry.url, archive);
  const found = await sha256Of(archive);
  if (found !== entry.sha256) {
    rmSync(archive);
    throw new Error(`the browser archive's hash is ${found}, not the one in browser.lock.json`);
  }
  const unpacked = `${BROWSER_DIR}${entry.sha256}`;
  if (!existsSync(unpacked)) {
    execFileSync('unzip', ['-q', archive, '-d', unpacked]);
  }
  for (const name of readdirSync(BROWSER_DIR)) {
    if (!name.startsWith(entry.sha256)) rmSync(`${BROWSER_DIR}${name}`, { recursive: true });
  }
}

function urlFor(hostPlatform) {
  const output = execFileSync(
    'npx',
    ['playwright', 'install', '--dry-run', '--only-shell', 'chromium'],
    { cwd: E2E, encoding: 'utf8', env: { ...process.env, PLAYWRIGHT_HOST_PLATFORM_OVERRIDE: hostPlatform } },
  );
  const [, url] = /Download url:\s+(\S+)/.exec(output);
  return url;
}

async function update() {
  mkdirSync(BROWSER_DIR, { recursive: true });
  const archives = {};
  for (const [key, hostPlatform] of Object.entries(PLATFORMS)) {
    const url = urlFor(hostPlatform);
    const path = `${BROWSER_DIR}update.tmp`;
    const modified = await download(url, path);
    const age = (Date.now() - Date.parse(modified)) / DAY_MS;
    const sha256 = await sha256Of(path);
    rmSync(path);
    if (!(age >= MIN_AGE_DAYS)) {
      throw new Error(`${url} is ${age} days old: it must be at least ${MIN_AGE_DAYS}`);
    }
    archives[key] = { url, sha256, published: new Date(modified).toISOString().slice(0, 10) };
  }
  const lock = { playwright: playwrightVersion(), archives };
  writeFileSync(LOCK_PATH, `${JSON.stringify(lock, null, 2)}\n`);
}

await (process.argv.includes('--update') ? update() : install());
