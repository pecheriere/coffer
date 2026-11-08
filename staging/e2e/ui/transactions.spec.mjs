// The transactions pages: a member enters a transaction by hand, files and reviews it from its
// page, finds it in the list, and deletes it.
import { expect, signInSeeded, test } from '../support/browser.mjs';

test('a member enters, files, reviews and deletes a transaction', async ({
  page,
  server,
  authenticator,
}) => {
  await signInSeeded(page, server, authenticator, 'sam');
  await page.goto(`${server.base}/transactions`);
  await expect(page.getByRole('heading', { name: 'Transactions', level: 1 })).toBeVisible();

  const enter = page.locator('form', { has: page.getByRole('button', { name: 'Enter it' }) });
  await enter.locator('select[name="account"]').selectOption({ label: 'Everyday Current, EUR' });
  await enter.locator('input[name="posted_on"]').fill(server.day);
  await enter.locator('input[name="amount"]').fill('4.20');
  await enter.locator('input[name="description"]').fill('Corner kiosk');
  await enter.getByRole('button', { name: 'Enter it' }).click();

  await expect(page.getByRole('heading', { name: /Corner kiosk/, level: 1 })).toBeVisible();
  await expect(page.getByRole('row').filter({ hasText: 'Amount' })).toContainText('-4.20 EUR');
  const filing = page.locator('form', { has: page.getByRole('button', { name: 'Save category' }) });
  await filing.locator('select[name="category"]').selectOption({ label: 'Daily life: Eating out' });
  await filing.getByRole('button', { name: 'Save category' }).click();
  await expect(page.getByText(/^Reviewed by /)).toBeVisible();

  await page.goto(`${server.base}/transactions?q=kiosk`);
  const row = page.getByRole('row').filter({ hasText: 'Corner kiosk' });
  await expect(row).toContainText('Daily life: Eating out');
  await row.getByRole('link', { name: 'Corner kiosk' }).click();
  await page.getByRole('button', { name: 'Delete this transaction' }).click();

  await expect(page.getByRole('heading', { name: 'Transactions', level: 1 })).toBeVisible();
  await page.goto(`${server.base}/transactions?q=kiosk`);
  await expect(page.getByText('No transactions match.')).toBeVisible();
});
