import { execFileSync } from 'node:child_process';

import { days } from './days.mjs';
import { STAGING } from './server.mjs';

export default function globalSetup() {
  const { today, monthStart, monthEnd } = days();
  execFileSync(STAGING, ['template', ...new Set([today, monthStart, monthEnd])], {
    stdio: 'inherit',
    // Seeding takes about a minute; twice that and more means something is stuck.
    timeout: 300_000,
  });
}
