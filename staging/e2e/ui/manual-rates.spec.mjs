// The household page's form for an exchange rate entered by hand, with net worth at the seed's
// known answer plus what the rate moves: £6,400.00 at 0.80 instead of 0.85.
import { expect, signInSeeded, test } from '../support/browser.mjs';

const figure = (page) => page.locator('h2.answer-label:has-text("Net worth on") + *');

async function enterRate(page, server, rate) {
  await page.goto(`${server.base}/admin/household`);
  const form = page.locator('form', { has: page.getByRole('button', { name: 'Save rate' }) });
  await form.locator('select[name="currency"]').selectOption('GBP');
  await form.locator('input[name="on"]').fill(server.day);
  await form.locator('input[name="rate"]').fill(rate);
  await form.getByRole('button', { name: 'Save rate' }).click();
}

test('an admin saves a rate, net worth follows, and removing it undoes that', async ({
  page,
  server,
  authenticator,
}) => {
  await signInSeeded(page, server, authenticator, 'alex');

  await enterRate(page, server, '0.8');

  const row = page.getByRole('row').filter({ hasText: '0.8 per euro' });
  await expect(row).toBeVisible();
  await page.goto(`${server.base}/net-worth`);
  await expect(figure(page)).toHaveText('181,960.89 EUR');

  await page.goto(`${server.base}/admin/household`);
  await row.getByRole('button', { name: 'Remove' }).click();
  await expect(row).toHaveCount(0);
  await page.goto(`${server.base}/net-worth`);
  await expect(figure(page)).toHaveText('181,490.30 EUR');
});

test.describe('an unusual rate', () => {
  // The page that asks answers 422, which a browser notes on its console.
  test.use({ expectedConsole: ['status of 422'] });

  test('is kept only once confirmed', async ({ page, server, authenticator }) => {
    await signInSeeded(page, server, authenticator, 'alex');

    await enterRate(page, server, '2');

    await expect(page.getByRole('heading', { name: 'Check this rate', level: 1 })).toBeVisible();
    await page.getByRole('checkbox', { name: 'This rate is right, keep it' }).check();
    await page.getByRole('button', { name: 'Save rate' }).click();

    await expect(page.getByRole('row').filter({ hasText: '2 per euro' })).toBeVisible();
  });
});
