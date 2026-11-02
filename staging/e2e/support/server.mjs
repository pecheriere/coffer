// One throwaway server: its own copy of a template, on its own port.
import { execFile as execFileCallback } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { promisify } from 'node:util';

import { Api, PageSession } from '../../seed/client.mjs';

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
    this.seeded = JSON.parse(
      readFileSync(new URL(`staging/.state/templates/${day}/fixture.json`, root), 'utf8'),
    );
    this.admin = new Api(this.base, this.seeded.adminToken);
  }

  static async start(port, day) {
    await execFile(STAGING, ['start', String(port), day]);
    return new Server(port, day);
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
    });
    return stdout + stderr;
  }

  stop() {
    return execFile(STAGING, ['stop', String(this.port)]);
  }
}
