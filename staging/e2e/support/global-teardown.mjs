import { execFileSync } from 'node:child_process';

import { STAGING } from './server.mjs';

// A test that was killed can't stop its server. The stack itself stays up for the next run.
export default function globalTeardown() {
  execFileSync(STAGING, ['sweep'], { stdio: 'inherit', timeout: 60_000 });
}
