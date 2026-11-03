// Syncs for real, through the egress client, against the mock bridge.
import { randomUUID } from 'node:crypto';

import { eventually, expect, test } from '../support/fixtures.mjs';

const setupToken = (scenario, name = randomUUID()) =>
  Buffer.from(`https://bridge.simplefin.org/claim/${scenario}/${name}`).toString('base64');

const connect = (api, label, setup_token) =>
  api.post('/connections/simplefin', { label, setup_token });

// The connection, once its first sync has ended.
async function afterFirstSync(api, id) {
  const finished = ['succeeded', 'partial', 'failed'];
  await eventually('the first sync to end', async () => {
    const { body } = await api.get(`/connections/${id}/sync-runs`);
    return finished.includes(body.items[0]?.status) ? true : undefined;
  });
  const { body } = await api.get('/connections');
  return body.items.find((connection) => connection.id === id);
}

test('a seeded connection brought its card and left an account to bind', async ({ api, server }) => {
  const { body: transactions } = await api.get(
    `/transactions?account=${server.seeded.accounts.card}`,
  );
  expect(transactions.items.length).toBeGreaterThan(0);

  const { body: providers } = await api.get('/provider-accounts');
  const unbound = providers.items.filter(({ account }) => account === null);
  expect(unbound.map(({ name }) => name)).toEqual(['Bramblewood Travel Saver']);
});

test('binding the unbound account brings its balance and clears the caution', async ({ api }) => {
  const { body: providers } = await api.get('/provider-accounts');
  const travel = providers.items.find(({ name }) => name === 'Bramblewood Travel Saver');

  const { status, body } = await api.post(`/provider-accounts/${travel.id}/create-account`, {
    kind: 'savings',
    include_in_forecast: false,
  });
  expect(status).toBe(201);

  const { body: balances } = await api.get(`/accounts/${body.account.id}/balances`);
  expect(balances.items[0].balance).toEqual({ minor_units: 250_000, currency: 'USD' });
  const { body: forecast } = await api.get('/forecast');
  expect(forecast.cautions.map(({ kind }) => kind)).not.toContain('unbound_provider_account');
  expect(forecast.status).toBe('firm');
});

test('a sync finds the accounts and records of a new connection', async ({ api }) => {
  const { status, body } = await connect(api, 'A second login', setupToken('healthy'));
  expect(status).toBe(201);

  const connection = await afterFirstSync(api, body.id);

  expect(connection.status).toBe('active');
  expect(connection.last_error).toBeNull();
  expect(connection.accounts.map(({ name }) => name).sort()).toEqual([
    'Bramblewood Card',
    'Bramblewood Travel Saver',
  ]);
});

test('a setup token works once', async ({ api }) => {
  const token = setupToken('healthy');
  expect((await connect(api, 'First', token)).status).toBe(201);

  const { status, body } = await connect(api, 'Again', token);

  expect(status).toBe(422);
  expect(body.code).toBe('setup_token_invalid');
});

for (const [scenario, state, message] of [
  ['outage', 'error', 'SimpleFin answered with status 503'],
  ['cut-off', 'error', 'SimpleFin could not be reached: connection failed'],
  ['bad-json', 'error', 'SimpleFin sent an answer coffer could not read'],
  ['revoked', 'reauth_required', 'the provider refused the credentials'],
  ['rate-limited', 'error', 'the provider is rate limiting requests'],
]) {
  test(`a bridge that answers with ${scenario} is reported and stores nothing`, async ({ api }) => {
    const before = await api.get('/reports/net-worth?account_points=false');
    const { body } = await connect(api, scenario, setupToken(scenario));

    const connection = await afterFirstSync(api, body.id);

    expect(connection.status).toBe(state);
    expect(connection.last_error).toBe(message);
    expect(connection.accounts).toEqual([]);
    const after = await api.get('/reports/net-worth?account_points=false');
    expect(after.body.points.at(-1).total).toEqual(before.body.points.at(-1).total);
  });
}
