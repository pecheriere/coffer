// The seed's numbers, by hand from staging/seed/household.mjs, on today and on both ends
// of a month. They are the same on all of these: that is what lets a test or a
// screenshot compare them with fixed answers.
import { days } from '../support/days.mjs';
import { expect, test } from '../support/fixtures.mjs';

const EUR = (minor_units) => ({ minor_units, currency: 'EUR' });

// Everyday Current 8,450.00, Rainy Day Saver 22,000.00, Thistledown Mutual Saver
// £6,400.00 at 0.85, Quillfeather Brokerage $51,200.00 at 1.10, Family Home 320,000.00,
// Bramblewood Card -1,234.56, Home Mortgage -210,000.00, Car Loan -11,800.00.
const NET_WORTH = 18_149_030;

// 12 months of each: mortgage 1,350.00, utilities 180.00, car 320.00, subscription 15.00,
// groceries budget 600.00 less the 85.40 spent this month; insurance 960.00 and boiler
// 220.00 once; the 3,000.00 goal; a 1,000.00 floor; 11 salaries of 4,200.00, today's
// being already in the balance.
const OUTFLOWS = 3_067_460;
const CASH = 2_921_544;
const GOALS = 300_000;
const FLOOR = 100_000;
const INFLOWS = 4_620_000;
const SURPLUS = CASH + INFLOWS - OUTFLOWS - GOALS - FLOOR;

const { today, monthStart, monthEnd } = days();

// The one thing a backdated seed can't do: close an account in the past. Seen from an
// earlier day, the closed account is still open, with a balance a year old.
const expectNetWorthStanding = (report, day) => {
  if (day === today) {
    expect(report.status).toBe('firm');
    expect(report.cautions).toEqual([]);
  } else {
    expect(report.status).toBe('provisional');
    expect(report.cautions.map(({ kind }) => kind)).toEqual(['balance_stale']);
  }
};

for (const [name, day] of [
  ['today', today],
  ['the first of a month', monthStart],
  ['the last of a month', monthEnd],
]) {
  test.describe(`seeded on ${name}`, () => {
    test.use({ seedDay: day });

    test('net worth has its known answer', async ({ api }) => {
      const { body } = await api.get(`/reports/net-worth?from=${day}&to=${day}&interval=day`);

      expectNetWorthStanding(body, day);
      expect(body.display_currency).toBe('EUR');
      expect(body.blockers).toEqual([]);
      const [point] = body.points;
      expect(point.total).toEqual(EUR(NET_WORTH));
      expect(point.unconverted).toEqual([]);
      // Seen from an earlier day, the emptied account still shows, at nothing.
      const held = point.by_currency.filter(({ minor_units }) => minor_units !== 0);
      expect(held).toEqual([
        EUR(12_741_544),
        { minor_units: 640_000, currency: 'GBP' },
        { minor_units: 5_120_000, currency: 'USD' },
      ]);
    });

    test('money that is not the household stays out, in its buckets', async ({ api }) => {
      const { body } = await api.get(`/reports/net-worth?from=${day}&to=${day}&interval=day`);

      const held = body.buckets.map((bucket) => [
        bucket.belongs_to,
        bucket.beneficiary,
        bucket.points[0].total,
      ]);
      expect(held).toEqual([
        ['someone_else', 'Robin', EUR(1_450_000)],
        ['given_away', null, EUR(272_727)],
      ]);
      const counted = body.accounts.map((account) => account.name);
      expect(counted).not.toContain('College Fund');
      expect(counted).not.toContain('Giving Fund');
    });

    test('the forecast has its known answer', async ({ api }) => {
      const { body } = await api.get(`/forecast?as_of=${day}`);

      expect(body.blockers).toEqual([]);
      expect(body.surplus).toEqual(EUR(SURPLUS));
      expect(body.totals).toEqual([
        {
          currency: 'EUR',
          cash_available: EUR(CASH),
          planned_outflows: EUR(OUTFLOWS),
          goals: EUR(GOALS),
          floor: EUR(FLOOR),
          planned_inflows: EUR(INFLOWS),
          excluded_inflows: EUR(0),
          cash_needed: EUR(OUTFLOWS + GOALS + FLOOR - INFLOWS),
          surplus: EUR(SURPLUS),
        },
      ]);
      const expected =
        day === today ? ['unbound_provider_account'] : ['as_of_not_today', 'unbound_provider_account'];
      expect(body.cautions.map(({ kind }) => kind).sort()).toEqual(expected);
    });

    test('three years of history are there, converted', async ({ api, server }) => {
      const from = `${String(Number(day.slice(0, 4)) - 3)}${day.slice(4, 8)}01`;
      const { body } = await api.get(
        `/reports/net-worth?from=${from}&to=${day}&interval=month&account_points=false`,
      );

      expectNetWorthStanding(body, day);
      expect(body.points.length).toBe(37);
      expect(body.points.every(({ unconverted }) => unconverted.length === 0)).toBe(true);
      expect(body.points.at(-1).total).toEqual(EUR(NET_WORTH));
      expect(body.points[0].total.minor_units).toBeLessThan(NET_WORTH);
      expect(server.seeded.today).toBe(day);
    });
  });
}
