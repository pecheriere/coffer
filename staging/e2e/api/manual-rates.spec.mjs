// Exchange rates entered by hand, through the built image: saving, listing and removing one
// moves net worth by a known amount. The seed's 181,490.30 EUR is 127,415.44 EUR, £6,400.00
// at 0.85 and $51,200.00 at 1.10, and only the pounds move here.
import { CheckedApi } from '../support/api.mjs';
import { expect, test } from '../support/fixtures.mjs';
import { addDays } from '../../seed/household.mjs';

const NOT_GBP = 12_741_544 + 4_654_545;
const SEEDED = NOT_GBP + 752_941;

async function netWorthOn(api, day) {
  const { body } = await api.get(`/reports/net-worth?from=${day}&to=${day}&interval=day`);
  return { report: body, total: body.points[0].total.minor_units };
}

test('a saved rate moves net worth, says so, and removing it moves it back', async ({
  api,
  server,
}) => {
  const { day } = server;
  expect((await netWorthOn(api, day)).total).toBe(SEEDED);

  const saved = await api.put(`/fx/manual-rates/GBP/${day}`, { rate: '0.8' });
  expect(saved.status).toBe(200);
  expect(saved.body).toMatchObject({ currency: 'GBP', on: day, rate: '0.8' });

  const { body: list } = await api.get('/fx/manual-rates');
  expect(list.truncated).toBe(false);
  expect(list.items).toEqual([saved.body]);

  const { report, total } = await netWorthOn(api, day);
  expect(total).toBe(NOT_GBP + 800_000);
  expect(report.status).toBe('firm');
  expect(report.points[0].manual_rate).toBe(true);
  expect(report.manual_rates).toEqual([{ currency: 'GBP', first_on: day, last_on: day }]);

  expect((await api.call('DELETE', `/fx/manual-rates/GBP/${day}`)).status).toBe(204);

  expect((await api.get('/fx/manual-rates')).body.items).toEqual([]);
  const after = await netWorthOn(api, day);
  expect(after.total).toBe(SEEDED);
  expect(after.report.manual_rates).toEqual([]);
});

test('a rate far from the published one needs confirming', async ({ api, server }) => {
  const { day } = server;

  const { status, body } = await api.put(`/fx/manual-rates/GBP/${day}`, { rate: '2' });

  expect(status).toBe(422);
  expect(body.code).toBe('unusual_rate');
  expect(body.detail).toContain('0.85');
  expect(body.detail).toContain('2');
  expect((await api.get('/fx/manual-rates')).body.items).toEqual([]);
  expect((await netWorthOn(api, day)).total).toBe(SEEDED);

  const confirmed = await api.put(`/fx/manual-rates/GBP/${day}`, {
    rate: '2',
    confirm_unusual: true,
  });

  expect(confirmed.status).toBe(200);
  expect((await netWorthOn(api, day)).total).toBe(NOT_GBP + 320_000);
});

test('a rate entered the wrong way round needs confirming', async ({ api, server }) => {
  // 1 / 0.85 is about 1.176: under 1.5 times the published rate, and still wrong.
  const { status, body } = await api.put(`/fx/manual-rates/GBP/${server.day}`, { rate: '1.18' });

  expect(status).toBe(422);
  expect(body.code).toBe('unusual_rate');
});

test('a currency nobody publishes converts once it has a rate', async ({ api, server }) => {
  const { day } = server;
  const { body: account } = await api.post('/accounts', {
    name: 'In kroner',
    kind: 'savings',
    currency: 'DKK',
    opening_balance: { on: day, minor_units: 100_000 },
  });
  expect((await netWorthOn(api, day)).report.points[0].unconverted).toEqual([account.id]);

  // With no published rate to compare it with, nothing is unusual.
  expect((await api.put(`/fx/manual-rates/DKK/${day}`, { rate: '7.46' })).status).toBe(200);

  const { report, total } = await netWorthOn(api, day);
  expect(report.points[0].unconverted).toEqual([]);
  expect(total).toBe(SEEDED + 13_405);
});

test('what cannot be saved or removed is refused', async ({ api, server }) => {
  const { day } = server;
  const put = (currency, on, body) => api.put(`/fx/manual-rates/${currency}/${on}`, body);

  for (const rate of ['0', '-1', 'abc', 0.8]) {
    expect((await put('GBP', day, { rate })).status).toBe(400);
  }
  expect((await put('XXQ', day, { rate: '1' })).status).toBe(400);
  // The euro, which is also the display currency here, and a day to come.
  expect((await put('EUR', day, { rate: '1' })).status).toBe(422);
  expect((await put('GBP', addDays(day, 2), { rate: '0.8' })).status).toBe(422);
  expect((await api.call('DELETE', `/fx/manual-rates/GBP/${day}`)).status).toBe(404);
  expect((await api.get('/fx/manual-rates')).body.items).toEqual([]);
});

test('only an admin token saves a rate', async ({ server }) => {
  const rate = { rate: '0.8' };
  const path = `/fx/manual-rates/GBP/${server.day}`;
  const alex = await server.signIn('alex');
  const sam = await server.signIn('sam');

  for (const secret of [
    await alex.createToken('reads', 'read'),
    await alex.createToken('writes', 'write'),
    await sam.createToken('a member', 'write'),
  ]) {
    const api = new CheckedApi(server.base, secret);
    expect((await api.get('/fx/manual-rates')).status).toBe(200);
    expect((await api.put(path, rate)).status).toBe(403);
  }
});
