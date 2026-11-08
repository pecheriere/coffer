// The seeded household, in a browser: its passkeys survive the copy, and an account
// waiting to be bound can be.
import { expect, signInSeeded, test } from '../support/browser.mjs';

test('a seeded member signs in with the key in the fixture', async ({
  page,
  server,
  authenticator,
}) => {
  await signInSeeded(page, server, authenticator, 'sam');
  await expect(page).toHaveURL(`${server.base}/`);
  await expect(page.getByRole('heading', { name: 'Home', level: 1 })).toBeVisible();

  await page.goto(`${server.base}/account`);
  await expect(page.getByRole('heading', { name: 'Create a token' })).toBeVisible();
  await page.goto(`${server.base}/more`);
  await expect(page.getByRole('link', { name: 'Cash forecast' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Members' })).toHaveCount(0);
});

test('an admin binds the unbound account, and net worth counts it', async ({
  page,
  server,
  authenticator,
}) => {
  await signInSeeded(page, server, authenticator, 'alex');
  await page.goto(`${server.base}/forecast`);
  await expect(
    page.getByRole('listitem').filter({ hasText: 'Bramblewood Travel Saver' }),
  ).toBeVisible();

  await page.goto(`${server.base}/connections`);
  // An account waiting for this opens by itself.
  const form = page.locator('form', {
    has: page.locator('input[name="name"][value="Bramblewood Travel Saver"]'),
  });
  await form.locator('select[name="kind"]').selectOption('savings');
  await form.getByRole('button', { name: 'Create account and bind' }).click();
  await expect(form).toHaveCount(0);

  // The seed's answer and $2,500.00 at 1.10.
  await page.goto(`${server.base}/net-worth`);
  await expect(page.locator('h2.answer-label:has-text("Net worth on") + *')).toHaveText(
    '183,763.03 EUR',
  );
});
