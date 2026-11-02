// The forecast and net worth pages, on a phone and a desktop, light and dark, with the
// seed's known answers.
import { expect, signInSeeded, test } from '../support/browser.mjs';

const WIDTHS = {
  phone: { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true },
  desktop: { viewport: { width: 1280, height: 800 } },
};

const PAGES = [
  { path: '/forecast', heading: 'Forecast', answer: '40,740.84 EUR' },
  { path: '/net-worth', heading: 'Net worth', answer: '181,490.30' },
];

// How light a colour is, from 0 to 255: enough to tell a light page from a dark one.
const lightness = (color) => {
  const [red, green, blue] = color.match(/[\d.]+/g).slice(0, 3).map(Number);
  return (red + green + blue) / 3;
};

for (const [width, device] of Object.entries(WIDTHS)) {
  for (const colorScheme of ['light', 'dark']) {
    test.describe(`${width}, ${colorScheme}`, () => {
      test.use({ ...device, colorScheme });

      for (const { path, heading, answer } of PAGES) {
        test(`${heading} shows its answer and fits`, async ({ page, server, authenticator }) => {
          await signInSeeded(page, server, authenticator, 'alex');

          await page.goto(`${server.base}${path}`);

          await expect(page.getByRole('heading', { name: heading, level: 1 })).toBeVisible();
          await expect(page.getByText(answer).first()).toBeVisible();
          const { sideways, background } = await page.evaluate(() => ({
            sideways: document.documentElement.scrollWidth - document.documentElement.clientWidth,
            background: getComputedStyle(document.body).backgroundColor,
          }));
          expect(sideways, 'the page scrolls sideways').toBeLessThanOrEqual(0);
          if (colorScheme === 'dark') {
            expect(lightness(background)).toBeLessThan(128);
          } else {
            expect(lightness(background)).toBeGreaterThan(128);
          }
        });
      }

      test('the net worth chart draws', async ({ page, server, authenticator }) => {
        await signInSeeded(page, server, authenticator, 'alex');

        await page.goto(`${server.base}/net-worth`);

        // Each width has its own drawing, and shows only that one.
        const chart = page.locator('svg.chart').locator('visible=true');
        await expect(chart).toHaveCount(1);
        await expect(chart).toHaveAttribute('aria-label', /last 181,490\.30 EUR/);
      });
    });
  }
}
