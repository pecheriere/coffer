// A new household: the operator's setup link, the first passkey, and a first token.
import { CheckedApi } from '../support/api.mjs';
import { expect, signIn, test } from '../support/browser.mjs';

test.use({ seedDay: null });

test('the first admin sets up through the link, makes a token and signs in again', async ({
  page,
  server,
  authenticator,
}) => {
  expect(authenticator.authenticatorId).toBeTruthy();

  await page.goto(await server.setupLink());
  await expect(page.getByRole('heading', { name: 'Create your passkey' })).toBeVisible();
  await page.getByLabel('Username').fill('first');
  await page.getByLabel('Display name').fill('First Admin');
  await page.getByRole('button', { name: 'Create passkey' }).click();
  await page.waitForURL((url) => !url.pathname.startsWith('/setup/'));

  await page.goto(`${server.base}/account`);
  await page.getByLabel('Name', { exact: true }).fill('from the page');
  await page.getByLabel('Scope').selectOption('read');
  await page.getByRole('button', { name: 'Create token' }).click();
  const secret = await page.locator('section.once code').textContent();
  const api = new CheckedApi(server.base, secret);
  const { status, body } = await api.get('/me');
  expect(status).toBe(200);
  expect(body.member.username).toBe('first');
  expect(body.agent.name).toBe('from the page');

  await page.getByRole('button', { name: 'Sign out' }).click();
  await page.waitForURL('**/login');

  await signIn(page, server, 'first');
  await page.goto(`${server.base}/account`);
  await expect(page.getByRole('heading', { name: 'Create a token' })).toBeVisible();
});
