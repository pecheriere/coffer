// One throwaway server: its own copy of a template, on its own port.
import { execFile as execFileCallback } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { promisify } from 'node:util';

import { PageSession } from '../../seed/client.mjs';

const execFile = promisify(execFileCallback);
const root = new URL('../../../', import.meta.url);

export const STAGING = new URL('scripts/staging.sh', root).pathname;

const FIXTURE = JSON.parse(
  readFileSync(new URL('staging/fixtures/passkeys.json', root), 'utf8'),
);

export const member = (username) => FIXTURE.members.find((one) => one.username === username);

export class Server {
  constructor(port, day) {
    this.port = port;
    this.day = day;
    this.base = `http://localhost:${port}`;
    this.seeded =
      day &&
      JSON.parse(
        readFileSync(new URL(`staging/.state/templates/${day}/fixture.json`, root), 'utf8'),
      );
  }

  // With no day, the server holds nothing: no template, and no admin yet.
  static async start(port, day) {
    const command = day ? ['start', String(port), day] : ['start-empty', String(port)];
    await execFile(STAGING, command, { timeout: 60_000 });
    return new Server(port, day);
  }

  // The first admin's setup link, as an operator gets it.
  async setupLink() {
    const { stdout } = await execFile('docker', [
      'exec',
      `coffer-staging-${this.port}`,
      '/coffer',
      'bootstrap-link',
    ], { timeout: 30_000 });
    return stdout.trim();
  }

  // A session signed in with a member's passkey.
  async signIn(username) {
    const pages = new PageSession(this.base, this.base);
    await pages.login(member(username));
    return pages;
  }

  async logs() {
    const { stdout, stderr } = await execFile('docker', ['logs', `coffer-staging-${this.port}`], {
      maxBuffer: 16 * 1024 * 1024,
      timeout: 30_000,
    });
    return stdout + stderr;
  }

  stop() {
    return execFile(STAGING, ['stop', String(this.port)], { timeout: 60_000 });
  }
}
