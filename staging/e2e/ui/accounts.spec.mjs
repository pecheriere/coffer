// The accounts page: a manual account created with an opening balance, a balance recorded on it,
// and net worth at the seed's known answer plus what the account holds, until it is said to be
// someone else's money and moves to a bucket.
import { expect, signInSeeded, test } from '../support/browser.mjs';

const figure = (page) => page.locator('h2.answer-label:has-text("Net worth on") + *');

test('a manual account counts in net worth until it is someone else\'s', async ({
  page,
  server,
  authenticator,
}) => {
  await signInSeeded(page, server, authenticator, 'alex');

  await page.goto(`${server.base}/accounts`);
  await expect(page.getByRole('heading', { name: 'Accounts', level: 1 })).toBeVisible();
  const create = page.locator('#new-account form');
  await create.getByLabel('Name').fill('Coin jar');
  await create.getByLabel('Kind').selectOption('savings');
  await create.getByLabel('Currency').selectOption('EUR');
  await create.getByLabel('Balance', { exact: true }).fill('1000.00');
  await create.locator('input[name="opening_on"]').fill(server.day);
  await create.getByRole('button', { name: 'Create account' }).click();

  await expect(page.getByRole('heading', { name: 'Coin jar', level: 1 })).toBeVisible();
  await expect(page.locator('#balances')).toContainText('1,000.00 EUR');
  await page.goto(`${server.base}/net-worth`);
  await expect(figure(page)).toHaveText('182,490.30 EUR');

  await page.goto(`${server.base}/accounts`);
  await page.getByRole('link', { name: 'Coin jar' }).click();
  const record = page.locator('#record-balance form');
  await record.locator('input[name="on"]').fill(server.day);
  await record.getByLabel('Balance in EUR').fill('250.50');
  await record.getByRole('button', { name: 'Save balance' }).click();
  await expect(page.locator('#balances')).toContainText('250.50 EUR');
  await page.goto(`${server.base}/net-worth`);
  await expect(figure(page)).toHaveText('181,740.80 EUR');

  await page.goto(`${server.base}/accounts`);
  await page.getByRole('link', { name: 'Coin jar' }).click();
  const change = page.locator('#change-account form');
  await change.getByLabel('Whose money is it?').selectOption('someone_else');
  await change.getByLabel('Held for (optional)').fill('Child A');
  await change.getByRole('button', { name: 'Save changes' }).click();
  await expect(page.getByRole('cell', { name: 'Held for Child A' })).toBeVisible();
  await page.goto(`${server.base}/net-worth`);
  await expect(figure(page)).toHaveText('181,490.30 EUR');
});
