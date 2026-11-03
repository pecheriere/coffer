// Rate fetches for real against the mock ECB, on the job's own time: within a minute of a
// currency being needed.
import { addMonths } from '../../seed/household.mjs';
import { eventually, expect, test } from '../support/fixtures.mjs';

test('a failing source is recorded per currency and blocks only what needs it', async ({
  api,
  server,
}) => {
  const added = [];
  for (const currency of ['NOK', 'SEK', 'DKK']) {
    const { status, body } = await api.post('/accounts', {
      name: `In ${currency}`,
      kind: 'savings',
      currency,
      opening_balance: { on: server.day, minor_units: 100_000 },
    });
    expect(status).toBe(201);
    added.push(body.id);
  }

  const coverage = await eventually('the job to try all three', async () => {
    const { body } = await api.get('/fx/coverage');
    const byCurrency = Object.fromEntries(body.items.map((row) => [row.currency, row]));
    return ['NOK', 'SEK', 'DKK'].every((currency) => byCurrency[currency]) ? byCurrency : undefined;
  });
  expect(coverage.NOK).toMatchObject({ status: 'failing', last_error: 'unavailable' });
  expect(coverage.SEK).toMatchObject({ status: 'failing', last_error: 'malformed' });
  expect(coverage.DKK).toMatchObject({ status: 'not_published' });
  expect(coverage.GBP).toMatchObject({ status: 'ready', last_error: null });

  const { body } = await api.get(`/reports/net-worth?from=${server.day}&to=${server.day}`);
  expect(body.status).not.toBe('firm');
  expect(body.points[0].unconverted.sort()).toEqual(added.sort());
});

test('the rates the seed fetched are the source\'s, with none at a weekend', async ({
  api,
  server,
}) => {
  const from = addMonths(server.day, -12);

  const { body } = await api.get(`/fx/rates?currency=GBP&from=${from}&to=${server.day}`);

  expect(body.items.length).toBeGreaterThan(200);
  expect(new Set(body.items.map(({ rate }) => rate))).toEqual(new Set(['0.85']));
  const weekdays = body.items.map(({ on }) => new Date(on).getUTCDay());
  expect(weekdays).not.toContain(0);
  expect(weekdays).not.toContain(6);
});
