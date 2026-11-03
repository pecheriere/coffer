// `server` is a new copy of the template for each test, and `api` its seeded admin's
// token. Tests run side by side and can't see each other's writes.
import { test as base, expect } from '@playwright/test';

import { CheckedApi } from './api.mjs';
import { Server } from './server.mjs';

// One each for the workers. 18080 is the served copy, and BESIDE_PORT a test's second server.
const FIRST_PORT = 18081;
const LAST_PORT = 18098;
export const BESIDE_PORT = 18099;

export const test = base.extend({
  seedDay: [process.env.STAGING_TODAY, { option: true }],

  server: async ({ seedDay }, use, testInfo) => {
    const port = FIRST_PORT + testInfo.parallelIndex;
    if (port > LAST_PORT) throw new Error(`no port left for worker ${testInfo.parallelIndex}`);
    const server = await Server.start(port, seedDay);
    let logs;
    try {
      await use(server);
      logs = await server.logs();
    } finally {
      await server.stop();
    }
    // Whatever a test was about, the server must not have panicked or logged a
    // credential on the way.
    expect(logs.includes('coffer panicked'), 'the server panicked').toBe(false);
    expect(logs.includes('made-up-staging-password'), 'a credential reached the log').toBe(false);
  },

  api: async ({ server }, use) => {
    await use(new CheckedApi(server.base, server.seeded.adminToken));
  },
});

export { expect };

// Asks until `check` answers something other than undefined.
export async function eventually(what, check, { seconds = 90 } = {}) {
  const deadline = Date.now() + seconds * 1000;
  while (Date.now() < deadline) {
    const answer = await check();
    if (answer !== undefined) return answer;
    await new Promise((resolve) => {
      setTimeout(resolve, 500);
    });
  }
  throw new Error(`gave up waiting for ${what}`);
}
