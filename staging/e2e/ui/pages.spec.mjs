// The forecast and net worth pages, on a phone and a desktop, light and dark, with the
// seed's known answers.
import { expect, signInSeeded, test } from '../support/browser.mjs';

const WIDTHS = {
  phone: { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true },
  desktop: { viewport: { width: 1280, height: 800 } },
};

// Each answer is the element right after its label.
const PAGES = [
  { path: '/forecast', heading: 'Forecast', label: 'Surplus', answer: '40,740.84 EUR' },
  { path: '/net-worth', heading: 'Net worth', label: 'Net worth on', answer: '181,490.30 EUR' },
];

// What sticks out past the window, or is cut off by a box that hides its overflow: a page can
// look fine to `scrollWidth` while a phone loses the end of a figure. A box that scrolls by
// itself, such as a wide table's, is allowed to hold more than it shows.
const OUT_OF_BOUNDS = () => {
  const width = document.documentElement.clientWidth;
  const found = [];
  const name = (el) => `${el.tagName.toLowerCase()}.${el.getAttribute('class') ?? ''}`;
  for (const el of document.querySelectorAll('body *')) {
    const box = el.getBoundingClientRect();
    if (box.width === 0 || box.height === 0 || el.closest('svg') !== null) continue;
    if (getComputedStyle(el).visibility === 'hidden') continue;
    let scrolls = false;
    for (let up = el.parentElement; up; up = up.parentElement) {
      const overflow = getComputedStyle(up).overflowX;
      if (overflow === 'auto' || overflow === 'scroll') scrolls = true;
      if ((overflow === 'hidden' || overflow === 'clip') && up !== document.body) {
        // A box of one pixel is how text is kept for screen readers alone.
        const { right, width: boxWidth } = up.getBoundingClientRect();
        if (boxWidth > 1 && box.right > right + 1) {
          found.push(`${name(el)} is cut off by ${name(up)}`);
        }
      }
    }
    if (!scrolls && box.right > width + 1) found.push(`${name(el)} sticks out of the window`);
  }
  return found;
};

// How light a colour is, from 0 to 255: enough to tell a light page from a dark one.
const lightness = (color) => {
  const [red, green, blue] = color.match(/[\d.]+/g).slice(0, 3).map(Number);
  return (red + green + blue) / 3;
};

for (const [width, device] of Object.entries(WIDTHS)) {
  for (const colorScheme of ['light', 'dark']) {
    // A dark page has a dark background behind light words, and a light one the reverse.
    const expectScheme = (background, words) => {
      const [back, front] = colorScheme === 'dark' ? ['toBeLessThan', 'toBeGreaterThan'] : ['toBeGreaterThan', 'toBeLessThan'];
      expect(lightness(background))[back](128);
      expect(lightness(words))[front](128);
    };

    test.describe(`${width}, ${colorScheme}`, () => {
      test.use({ ...device, colorScheme });

      for (const { path, heading, label, answer } of PAGES) {
        test(`${heading} shows its answer and fits`, async ({ page, server, authenticator }) => {
          await signInSeeded(page, server, authenticator, 'alex');

          await page.goto(`${server.base}${path}`);

          await expect(page.getByRole('heading', { name: heading, level: 1 })).toBeVisible();
          const figure = page.locator(`h2.answer-label:has-text("${label}") + *`);
          await expect(figure).toHaveText(answer);
          await expect(figure).toBeVisible();
          expect(await page.evaluate(OUT_OF_BOUNDS)).toEqual([]);
          const { background, text } = await page.evaluate(() => ({
            background: getComputedStyle(document.body).backgroundColor,
            text: getComputedStyle(document.querySelector('h1')).color,
          }));
          expectScheme(background, text);
        });
      }

      test('the net worth chart draws', async ({ page, server, authenticator }) => {
        await signInSeeded(page, server, authenticator, 'alex');

        await page.goto(`${server.base}/net-worth`);

        // Each width has its own drawing, and shows only that one.
        const chart = page.locator('svg.chart').locator('visible=true');
        await expect(chart).toHaveCount(1);
        await expect(chart).toHaveAttribute('aria-label', /last 181,490\.30 EUR/);
        const { background, labels } = await page.evaluate(() => ({
          background: getComputedStyle(document.body).backgroundColor,
          labels: getComputedStyle(
            [...document.querySelectorAll('svg.chart')]
              .find((svg) => svg.getBoundingClientRect().width > 0)
              .querySelector('text'),
          ).fill,
        }));
        expectScheme(background, labels);
      });
    });
  }
}

// The check above would pass for the wrong reason if it found nothing it should.
test.describe('the fit check', () => {
  test.use(WIDTHS.phone);

  test('finds a figure cut off inside a box that hides its overflow', async ({
    page,
    server,
    authenticator,
  }) => {
    await signInSeeded(page, server, authenticator, 'alex');
    await page.goto(`${server.base}/forecast`);
    expect(await page.evaluate(OUT_OF_BOUNDS)).toEqual([]);

    await page.locator('h1').evaluate((heading) => {
      heading.style.overflowX = 'hidden';
      heading.style.width = '100px';
      heading.style.whiteSpace = 'nowrap';
      const wide = document.createElement('span');
      wide.style.display = 'inline-block';
      wide.style.width = '300px';
      wide.style.height = '10px';
      heading.append(wide);
    });

    expect((await page.evaluate(OUT_OF_BOUNDS)).join()).toContain('is cut off by h1');
  });
});
