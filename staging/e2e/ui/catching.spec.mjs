// The check every UI test leans on: a violation of the content security policy is caught
// on the console and in the server's log. If this stopped working, the others would pass
// for the wrong reason.
import { browserReports, eventually, expect, signInSeeded, test } from '../support/browser.mjs';

test.use({ expectProblems: true });

test('a blocked inline script is caught twice', async ({
  page,
  server,
  authenticator,
  consoleProblems,
}) => {
  await signInSeeded(page, server, authenticator, 'alex');
  await page.goto(`${server.base}/forecast`);
  expect(consoleProblems).toEqual([]);

  await page.evaluate(() => {
    const script = document.createElement('script');
    script.textContent = 'window.ranInline = true;';
    document.head.append(script);
  });

  expect(await page.evaluate(() => window.ranInline)).toBeUndefined();
  expect(consoleProblems.join()).toContain('Content Security Policy');
  const reports = await eventually('the browser report in the log', async () => {
    const found = browserReports(await server.logs());
    return found.length > 0 ? found : undefined;
  });
  expect(reports.join()).toContain('csp');
});
