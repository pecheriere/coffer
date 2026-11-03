// The MCP endpoint of the built image, over real HTTP with hand-written JSON-RPC: the protocol,
// who gets in, and that a tool answers what its API operation answers.
import { CheckedApi } from '../support/api.mjs';
import { expect, test } from '../support/fixtures.mjs';
import { CURRENT_REVISION, Mcp, OLDER_REVISION, problemsAgainst } from '../support/mcp.mjs';

const readTokenOf = async (server) => (await server.signIn('alex')).createToken('read only', 'read');

test('a client connects, lists and calls as the protocol says', async ({ server }) => {
  const mcp = new Mcp(server.base, server.seeded.adminToken);

  const initialized = await mcp.request('initialize', {
    protocolVersion: OLDER_REVISION,
    capabilities: {},
    clientInfo: { name: 'a client', version: '1' },
  });
  expect(initialized.status).toBe(200);
  expect(initialized.headers.get('mcp-session-id')).toBe(null);
  expect(initialized.body.result.protocolVersion).toBe(OLDER_REVISION);
  expect(initialized.body.result.serverInfo.name).toBe('coffer');
  expect(initialized.body.result.capabilities.tools).toBeDefined();

  const noted = await mcp.post({ jsonrpc: '2.0', method: 'notifications/initialized' });
  expect(noted.status).toBe(202);

  const tools = await mcp.tools();
  expect(tools.map(({ name }) => name)).toContain('get_forecast');

  const answer = await mcp.call('whoami');
  expect(answer.isError).toBe(false);
  expect(answer.structuredContent.member.username).toBeDefined();
});

test('a client at the current revision needs no handshake', async ({ server }) => {
  const mcp = new Mcp(server.base, server.seeded.adminToken, {
    'MCP-Protocol-Version': CURRENT_REVISION,
    'Mcp-Method': 'tools/call',
    'Mcp-Name': 'whoami',
  });

  const { status, body } = await mcp.request('tools/call', {
    name: 'whoami',
    arguments: {},
    _meta: {
      'io.modelcontextprotocol/protocolVersion': CURRENT_REVISION,
      'io.modelcontextprotocol/clientInfo': { name: 'a client', version: '1' },
      'io.modelcontextprotocol/clientCapabilities': {},
    },
  });

  expect(status).toBe(200);
  expect(body.result.structuredContent.agent).toBeDefined();
});

test('each read tool answers what its operation answers, as its schema says', async ({
  server,
  api,
}) => {
  const mcp = new Mcp(server.base, server.seeded.adminToken);
  const account = server.seeded.accounts.current;
  const tools = new Map((await mcp.tools()).map((tool) => [tool.name, tool]));
  const { body: page } = await api.get('/transactions?limit=1');
  const transaction = page.items[0].id;
  const { body: plan } = await api.get('/plan-items');
  const item = plan.items[0].id;
  const month = server.day.slice(0, 7);
  const rates = 'currency=CAD&from=2026-01-01&to=2026-01-31';
  const cases = [
    ['whoami', {}, '/me'],
    ['get_household_settings', {}, '/household/settings'],
    ['list_members', {}, '/members'],
    ['list_accounts', {}, '/accounts'],
    ['get_account', { account_id: account }, `/accounts/${account}`],
    ['list_account_balances', { account_id: account, limit: 5 }, `/accounts/${account}/balances?limit=5`],
    ['list_categories', {}, '/categories'],
    ['list_transactions', { limit: 5 }, '/transactions?limit=5'],
    ['list_review_queue', {}, '/transactions?review=unreviewed'],
    ['get_transaction', { transaction_id: transaction }, `/transactions/${transaction}`],
    ['list_plan_items', {}, '/plan-items'],
    ['get_plan_item', { item_id: item }, `/plan-items/${item}`],
    ['get_forecast', {}, '/forecast'],
    ['get_plan_vs_actual', { from: month }, `/reports/plan-vs-actual?from=${month}`],
    ['get_net_worth', {}, '/reports/net-worth'],
    ['get_fx_coverage', {}, '/fx/coverage'],
    ['list_fx_rates', { currency: 'CAD', from: '2026-01-01', to: '2026-01-31' }, `/fx/rates?${rates}`],
  ];

  for (const [name, args, path] of cases) {
    const rest = await api.get(path);
    expect(rest.status, name).toBe(200);
    const answer = await mcp.call(name, args);
    expect(answer.structuredContent, name).toEqual(rest.body);
    expect(JSON.parse(answer.content[0].text), name).toEqual(rest.body);
    expect(problemsAgainst(tools.get(name).outputSchema, answer.structuredContent), name).toEqual([]);
  }

  const what = { horizon_months: 3 };
  const rest = await api.post('/forecast/what-if', what);
  const answer = await mcp.call('what_if_forecast', what);
  expect(answer.structuredContent).toEqual(rest.body);
  expect(
    problemsAgainst(tools.get('what_if_forecast').outputSchema, answer.structuredContent),
  ).toEqual([]);
});

test('a read token is shown no write and is refused one as the API refuses it', async ({
  server,
  api,
}) => {
  const secret = await readTokenOf(server);
  const mcp = new Mcp(server.base, secret);
  const reader = new CheckedApi(server.base, secret);
  const { body: page } = await api.get('/transactions?review=unreviewed&limit=1');
  const { id } = page.items[0];

  const names = (await mcp.tools()).map(({ name }) => name);
  expect(names).toContain('get_forecast');
  expect(names).not.toContain('review_transactions');

  const rest = await reader.post('/transactions/batch', {
    operations: [{ id, patch: { review: 'reviewed' } }],
  });
  expect(rest.status).toBe(403);
  const answer = await mcp.call('review_transactions', {
    operations: [{ id, review: 'reviewed' }],
  });
  expect(answer.isError).toBe(true);
  expect(answer.structuredContent).toBeUndefined();
  expect(JSON.parse(answer.content[0].text)).toEqual(rest.body);

  const { body: seen } = await api.get(`/transactions/${id}`);
  expect(seen.review).toBe('unreviewed');
});

test('a write token reviews, and the answer is the transaction as it now is', async ({
  server,
  api,
}) => {
  const mcp = new Mcp(server.base, server.seeded.adminToken);
  const { body: page } = await api.get('/transactions?review=unreviewed&limit=1');
  const { id } = page.items[0];

  const answer = await mcp.call('review_transactions', {
    operations: [{ id, review: 'reviewed' }],
  });

  const { body: seen } = await api.get(`/transactions/${id}`);
  expect(seen.review).toBe('reviewed');
  expect(answer.structuredContent).toEqual({ items: [seen] });
});

test('only a good token from no other origin gets in', async ({ server }) => {
  const list = { jsonrpc: '2.0', id: 1, method: 'tools/list' };
  const { adminToken } = server.seeded;

  for (const token of [undefined, 'not-a-token']) {
    const { status, body, headers } = await new Mcp(server.base, token).post(list);
    expect(status).toBe(401);
    expect(body.code).toBe('unauthenticated');
    expect(headers.get('www-authenticate')).toBe('Bearer');
  }

  const foreign = await new Mcp(server.base, adminToken, {
    Origin: 'https://elsewhere.example',
  }).post(list);
  expect(foreign.status).toBe(403);
  expect(foreign.body.code).toBe('cross_origin');

  const own = await new Mcp(server.base, adminToken, { Origin: server.base }).post(list);
  expect(own.status).toBe(200);

  const cookie = await server.signIn('alex');
  const session = await fetch(`${server.base}/mcp`, {
    method: 'POST',
    headers: {
      Cookie: cookie.cookie,
      'Content-Type': 'application/json',
      Accept: 'application/json, text/event-stream',
    },
    body: JSON.stringify(list),
  });
  expect(session.status).toBe(401);

  const opened = await fetch(`${server.base}/mcp`, {
    headers: { Authorization: `Bearer ${adminToken}`, Accept: 'text/event-stream' },
  });
  expect(opened.status).toBe(405);
  expect(opened.headers.get('allow')).toBe('POST');
});

test('nothing of a call reaches the log, at the image\'s own level', async ({ server, api }) => {
  const mcp = new Mcp(server.base, server.seeded.adminToken);
  const { body: page } = await api.get('/transactions?limit=1');

  await mcp.request('initialize', {
    protocolVersion: OLDER_REVISION,
    capabilities: {},
    clientInfo: { name: 'a client that says this of itself', version: '1' },
  });
  await mcp.call('list_transactions', { limit: 1 });

  const logs = await server.logs();
  expect(logs).toContain('coffer is serving');
  expect(logs.includes(page.items[0].description)).toBe(false);
  expect(logs.includes('a client that says this of itself')).toBe(false);
});
