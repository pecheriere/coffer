// Two transactions said to be one movement of money. The seed's surplus is 42,270.84 EUR with
// 85.40 of the 600.00 groceries budget spent, so 500.00 more filed as groceries lifts it by
// 500.00, until a link says that money only moved.
import { CheckedApi } from '../support/api.mjs';
import { expect, test } from '../support/fixtures.mjs';

const SURPLUS = 4_227_084;
const EUR = (minor_units) => ({ minor_units, currency: 'EUR' });

// A manual transaction, filed if given a category.
async function transaction(api, account, on, minor_units, category) {
  const { status, body } = await api.post('/transactions', {
    account,
    posted_on: on,
    minor_units,
    description: 'Moving money',
  });
  expect(status).toBe(201);
  if (category) {
    const filed = await api.patch(`/transactions/${body.id}`, {
      splits: [{ category, minor_units }],
    });
    expect(filed.status).toBe(200);
  }
  return body.id;
}

const surplus = async (api, day) =>
  (await api.get(`/forecast?as_of=${day}`)).body.surplus.minor_units;

const netWorth = async (api, day) =>
  (await api.get(`/reports/net-worth?from=${day}&to=${day}&interval=day`)).body.points[0].total;

// Out of the current account as groceries, and into the saver.
async function bothLegs(api, server, arriving = 50_000) {
  const { today } = server.seeded;
  const { current, saver } = server.seeded.accounts;
  return {
    outflow: await transaction(api, current, today, -50_000, server.seeded.categories.Groceries),
    inflow: await transaction(api, saver, today, arriving),
  };
}

test('a link between counted accounts is one movement, until it ends', async ({ api, server }) => {
  const { day } = server;
  const worth = await netWorth(api, day);
  expect(await surplus(api, day)).toBe(SURPLUS);
  const legs = await bothLegs(api, server);
  expect(await surplus(api, day)).toBe(SURPLUS + 50_000);

  const { body: candidates } = await api.get(`/transactions/${legs.outflow}/transfer-candidates`);
  expect(candidates.items.map(({ id }) => id)).toEqual([legs.inflow]);

  const { status, body: link } = await api.post('/transfer-links', legs);
  expect(status).toBe(201);
  expect(link.outflow.transaction).toBe(legs.outflow);
  expect(link.inflow.transaction).toBe(legs.inflow);
  expect(link.difference).toEqual(EUR(0));
  expect(link.one_movement).toBe(true);
  expect(link.one_movement_in_reports).toBe(true);
  expect(await surplus(api, day)).toBe(SURPLUS);

  expect((await api.get(`/transfer-links/${link.id}`)).body).toEqual(link);
  const { body: leg } = await api.get(`/transactions/${legs.outflow}`);
  expect(leg.transfer_link).toEqual({ id: link.id, other_transaction: legs.inflow });
  const { body: none } = await api.get(`/transactions/${legs.outflow}/transfer-candidates`);
  expect(none.items).toEqual([]);

  expect((await api.call('DELETE', `/transfer-links/${link.id}`)).status).toBe(204);

  expect((await api.get(`/transfer-links/${link.id}`)).status).toBe(404);
  expect(await surplus(api, day)).toBe(SURPLUS + 50_000);
  // Balances say what net worth is, so a link never changed it.
  expect(await netWorth(api, day)).toEqual(worth);
});

test('a fee stays what its leg is filed as', async ({ api, server }) => {
  const legs = await bothLegs(api, server, 49_000);

  const { body: link } = await api.post('/transfer-links', legs);

  expect(link.difference).toEqual(EUR(-1_000));
  expect(await surplus(api, server.day)).toBe(SURPLUS + 1_000);
});

test('a link out of counted cash, in another currency, changes no number', async ({
  api,
  server,
}) => {
  const { today, accounts, categories } = server.seeded;
  const outflow = await transaction(api, accounts.current, today, -50_000, categories.Groceries);
  const inflow = await transaction(api, accounts.brokerage, today, 55_000);

  const { status, body: link } = await api.post('/transfer-links', { outflow, inflow });

  expect(status).toBe(201);
  expect(link.difference).toBeNull();
  expect(link.one_movement).toBe(false);
  expect(await surplus(api, server.day)).toBe(SURPLUS + 50_000);
});

test('what a link cannot join is refused', async ({ api, server }) => {
  const { today, accounts, categories } = server.seeded;
  const legs = await bothLegs(api, server);
  const sameAccount = await transaction(api, accounts.current, today, 50_000);
  expect((await api.post('/transfer-links', { ...legs, inflow: sameAccount })).status).toBe(422);

  const split = await transaction(api, accounts.current, today, -30_000);
  await api.patch(`/transactions/${split}`, {
    splits: [
      { category: categories.Groceries, minor_units: -10_000 },
      { category: categories['Eating out'], minor_units: -20_000 },
    ],
  });
  expect((await api.post('/transfer-links', { ...legs, outflow: split })).status).toBe(422);

  expect((await api.post('/transfer-links', legs)).status).toBe(201);
  const another = await transaction(api, accounts.saver, today, 50_000);
  expect((await api.post('/transfer-links', { ...legs, inflow: another })).status).toBe(409);
  // A linked transaction stays until its link ends.
  expect((await api.call('DELETE', `/transactions/${legs.outflow}`)).status).toBe(409);
});

test('a link needs write on both accounts, and a hidden one reads as missing', async ({
  api,
  server,
}) => {
  const { current, saver } = server.seeded.accounts;
  const legs = await bothLegs(api, server);
  const pages = await server.signIn('alex');
  const tokenWith = async (name, levels) =>
    new CheckedApi(server.base, await pages.createToken(name, 'write', { all: 'none', ...levels }));

  const oneSide = await tokenWith('one side', {
    [`account_${current}`]: 'write',
    [`account_${saver}`]: 'transactions',
  });
  expect((await oneSide.post('/transfer-links', legs)).status).toBe(403);

  const hidden = await tokenWith('hidden side', { [`account_${current}`]: 'write' });
  expect((await hidden.post('/transfer-links', legs)).status).toBe(404);

  const { body: link } = await api.post('/transfer-links', legs);
  expect((await oneSide.get(`/transfer-links/${link.id}`)).status).toBe(200);
  expect((await hidden.get(`/transfer-links/${link.id}`)).status).toBe(404);
  expect((await oneSide.call('DELETE', `/transfer-links/${link.id}`)).status).toBe(403);
});
