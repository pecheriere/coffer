// What the tests lean on, and what only the built image can show.
import { execFile as execFileCallback } from 'node:child_process';
import { promisify } from 'node:util';

import { CheckedApi } from '../support/api.mjs';
import { expect, test } from '../support/fixtures.mjs';
import { openapi } from '../support/openapi.mjs';
import { Server, STAGING } from '../support/server.mjs';

const execFile = promisify(execFileCallback);

const BESIDE_PORT = 18099;

test('a server does not see what another wrote', async ({ api, server }) => {
  const account = server.seeded.accounts.current;
  const beside = await Server.start(BESIDE_PORT, server.day);
  try {
    const other = new CheckedApi(beside.base, beside.seeded.adminToken);

    const written = await api.put(`/accounts/${account}/balances/${server.day}`, { minor_units: 1 });
    expect(written.status).toBe(200);

    const { body } = await other.get(`/accounts/${account}/balances?limit=1`);
    expect(body.items[0].balance.minor_units).toBe(845_000);
  } finally {
    await beside.stop();
  }
});

test('the image serves the API document in this checkout', async ({ api }) => {
  const { status, body } = await api.get('/openapi.json');

  expect(status).toBe(200);
  expect(body).toEqual(openapi);
});

test('the seed cannot start away from localhost', async ({ server }) => {
  const refused = await execFile(STAGING, ['elsewhere', server.day]).then(
    () => undefined,
    (failure) => failure,
  );

  expect(refused?.code).toBe(1);
  expect(refused?.stdout).toContain('COFFER_PUBLIC_URL on localhost');
});
