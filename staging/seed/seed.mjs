// Fills an empty coffer with the made-up household, through its public pages and
// API, and prints what the tests need to find their way around it.
//
// Reads SEED_BASE (where coffer answers), SEED_ORIGIN (its COFFER_PUBLIC_URL),
// SEED_LINK (its setup link) and SEED_TODAY (the day to date everything from).
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';

import { Api, PageSession, until } from './client.mjs';
import { household } from './household.mjs';

const { SEED_BASE: base, SEED_ORIGIN: origin, SEED_LINK: setupLink, SEED_TODAY: today } = process.env;

async function seedCategories(api, data) {
  const ids = {};
  for (const { group, categories } of data.categories) {
    const created = await api.post('/category-groups', { name: group });
    for (const [name, kind] of categories) {
      const category = await api.post('/categories', { group: created.id, name, kind });
      ids[name] = category.id;
    }
  }
  return ids;
}

async function seedAccounts(api, data) {
  const ids = {};
  for (const account of data.accounts) {
    const [first, ...rest] = account.history;
    const created = await api.post('/accounts', {
      name: account.name,
      kind: account.kind,
      currency: account.currency,
      ...(account.beneficiary ? { beneficiary: account.beneficiary } : {}),
      ...(account.includeInForecast === false ? { include_in_forecast: false } : {}),
      opening_balance: { on: first.on, minor_units: first.minor },
    });
    ids[account.key] = created.id;
    for (const { on, minor } of rest) {
      await api.put(`/accounts/${created.id}/balances/${on}`, { minor_units: minor });
    }
  }
  return ids;
}

async function closeAccounts(api, data, accounts) {
  for (const account of data.accounts.filter(({ closed }) => closed)) {
    await api.patch(`/accounts/${accounts[account.key]}`, { closed: true });
  }
}

async function seedTransactions(api, data, account, categories) {
  for (const { on, minor, description, category } of data.transactions) {
    const created = await api.post('/transactions', {
      account,
      posted_on: on,
      minor_units: minor,
      description,
    });
    await api.patch(`/transactions/${created.id}`, {
      splits: [{ category: categories[category], minor_units: minor }],
    });
  }
}

async function seedPlan(api, data, categories) {
  for (const { amount, currency, category, ...item } of data.planItems) {
    await api.post('/plan-items', {
      ...item,
      category: categories[category],
      amount: { minor_units: amount, currency },
    });
  }
}

async function seedConnection(api, data) {
  const connection = await api.post('/connections/simplefin', {
    label: 'Bramblewood Bank',
    setup_token: Buffer.from(`${data.simplefinClaim}/${randomUUID()}`).toString('base64'),
  });
  const { items } = await until('the first sync', async () => {
    const list = await api.get('/provider-accounts');
    return list.items.length > 0 ? list : undefined;
  });
  const { providerName, kind, name } = data.boundProviderAccount;
  const bound = items.find((item) => item.name === providerName);
  const { account } = await api.post(`/provider-accounts/${bound.id}/create-account`, {
    kind,
    name,
  });
  return { connection: connection.id, card: account.id };
}

// Rates arrive from the job that looks every minute, so this can take that long.
async function ratesFetched(api, data) {
  const needed = new Set(data.accounts.map(({ currency }) => currency));
  needed.delete('EUR');
  await until('the exchange rates', async () => {
    const { items } = await api.get('/fx/coverage');
    const ready = items.filter(({ status }) => status === 'ready').map(({ currency }) => currency);
    return [...needed].every((currency) => ready.includes(currency));
  });
}

async function seed() {
  const data = household(today);
  const fixture = JSON.parse(
    readFileSync(new URL('../fixtures/passkeys.json', import.meta.url), 'utf8'),
  );
  const [admin, member] = fixture.members;

  const adminPages = new PageSession(base, origin);
  await adminPages.enroll(setupLink, admin);
  const memberLink = await adminPages.createInvite(member.role, member.displayName);
  await new PageSession(base, origin).enroll(memberLink, member);
  const adminToken = await adminPages.createToken('staging seed', 'admin');

  const api = new Api(base, adminToken);
  await api.patch('/household/settings', data.settings);
  const categories = await seedCategories(api, data);
  const accounts = await seedAccounts(api, data);
  await seedTransactions(api, data, accounts.current, categories);
  await seedPlan(api, data, categories);
  const { connection, card } = await seedConnection(api, data);
  await ratesFetched(api, data);
  await closeAccounts(api, data, accounts);

  process.stdout.write(
    `${JSON.stringify({
      today,
      adminToken,
      connection,
      accounts: { ...accounts, card },
      categories,
    })}\n`,
  );
}

await seed();
