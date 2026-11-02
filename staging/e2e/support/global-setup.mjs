import { execFileSync } from 'node:child_process';

import { days } from './days.mjs';
import { STAGING } from './server.mjs';

export default function globalSetup() {
  const { today, monthStart, monthEnd } = days();
  execFileSync(STAGING, ['template', ...new Set([today, monthStart, monthEnd])], {
    stdio: 'inherit',
  });
}
