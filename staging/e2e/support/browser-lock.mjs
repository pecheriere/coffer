// The one browser the UI tests run: an archive whose hash is committed in browser.lock.json,
// checked by install-browser.mjs before it is unpacked.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export const LOCK_PATH = fileURLToPath(new URL('../browser.lock.json', import.meta.url));
export const BROWSER_DIR = fileURLToPath(new URL('../.browser/', import.meta.url));

export const readLock = () => JSON.parse(readFileSync(LOCK_PATH, 'utf8'));

export const platform = () => `${process.platform}-${process.arch}`;

// Where this machine's browser is once installed.
export function browserExecutable() {
  const { sha256, url } = readLock().archives[platform()];
  const folder = url.split('/').pop().replace(/\.zip$/, '');
  return `${BROWSER_DIR}${sha256}/${folder}/chrome-headless-shell`;
}
