// What a token may do, through the built image: scope, grants, revoking and a member's
// ceiling. Passkeys sign in as a browser would, since only a session can make tokens.
import { CheckedApi } from '../support/api.mjs';
import { expect, test } from '../support/fixtures.mjs';

const NEW_ACCOUNT = { name: 'New account', kind: 'savings', currency: 'EUR' };

test('without a good token nothing answers', async ({ server }) => {
  for (const token of [undefined, 'not-a-token']) {
    const { status } = await new CheckedApi(server.base, token).get('/accounts');
    expect(status).toBe(401);
  }
});

test('a read token reads and cannot write', async ({ server }) => {
  const pages = await server.signIn('alex');
  const api = new CheckedApi(server.base, await pages.createToken('read only', 'read'));

  expect((await api.get('/accounts')).status).toBe(200);
  expect((await api.post('/accounts', NEW_ACCOUNT)).status).toBe(403);
});

test('a token with a grant on one account sees only that account', async ({ server, api }) => {
  const pages = await server.signIn('alex');
  const account = server.seeded.accounts.current;
  const secret = await pages.createToken('one account', 'read', {
    all: 'none',
    [`account_${account}`]: 'balances',
  });
  const limited = new CheckedApi(server.base, secret);

  const listed = await limited.get('/accounts');
  expect(listed.body.items.map(({ id }) => id)).toEqual([account]);
  expect((await limited.get(`/accounts/${server.seeded.accounts.saver}`)).status).toBe(404);
  // Its answer covers that account alone, not the household's total.
  const worth = await limited.get('/reports/net-worth?account_points=false');
  expect(worth.body.points.at(-1).total.minor_units).toBe(845_000);
  // The admin token next to it still sees all of them.
  expect((await api.get('/accounts')).body.items.length).toBeGreaterThan(1);
});

test('a balances grant shows balances and not transactions', async ({ server }) => {
  const pages = await server.signIn('alex');
  const account = server.seeded.accounts.current;
  const secret = await pages.createToken('balances only', 'read', {
    all: 'none',
    [`account_${account}`]: 'balances',
  });
  const limited = new CheckedApi(server.base, secret);

  expect((await limited.get(`/accounts/${account}/balances?limit=1`)).status).toBe(200);
  const { status } = await limited.get(`/transactions?account=${account}`);
  expect(status).toBe(403);
});

test('a transactions grant shows that account\'s transactions', async ({ server }) => {
  const pages = await server.signIn('alex');
  const account = server.seeded.accounts.current;
  const secret = await pages.createToken('transactions', 'read', {
    all: 'none',
    [`account_${account}`]: 'transactions',
  });
  const limited = new CheckedApi(server.base, secret);

  const { status, body } = await limited.get(`/transactions?account=${account}&limit=5`);
  expect(status).toBe(200);
  expect(body.items.length).toBe(5);
});

test('a revoked token stops working at once', async ({ server }) => {
  const pages = await server.signIn('alex');
  const api = new CheckedApi(server.base, await pages.createToken('short lived', 'read'));
  const { body } = await api.get('/me');
  expect(body.agent.name).toBe('short lived');

  await pages.submit('/account', `/account/tokens/${body.agent.id}/revoke`, {});

  expect((await api.get('/me')).status).toBe(401);
});

test("a member's token stops at the member's rights", async ({ server }) => {
  const pages = await server.signIn('sam');
  const api = new CheckedApi(server.base, await pages.createToken('sam writes', 'write'));

  expect((await api.get('/me')).body.member.username).toBe('sam');
  expect((await api.patch('/household/settings', { default_horizon_months: 6 })).status).toBe(403);
  // It does write what Sam may: a token that could write nothing would stop here.
  expect((await api.post('/accounts', NEW_ACCOUNT)).status).toBe(201);
});
