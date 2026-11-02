// The made-up household, as a pure function of the day it is seeded on. Nothing in
// it is real.
//
// Numbers stay the same whichever day of the month that is, so a test can compare
// them with fixed answers:
// - every balance today is a fixed amount, and rates never change (mock/ecb.mjs);
// - every monthly item is anchored on today, so a window of N months holds exactly
//   N occurrences of it, today's included, however long the month;
// - this calendar month holds exactly one day of spending, today's. Earlier spending
//   falls a whole number of months and then some days back, so never in this month.

const HISTORY_MONTHS = 36;

function parts(iso) {
  const [year, month, day] = iso.split('-').map(Number);
  return { year, month, day };
}

function format(year, month, day) {
  const mm = String(month).padStart(2, '0');
  const dd = String(day).padStart(2, '0');
  return `${year}-${mm}-${dd}`;
}

// `iso` moved by whole months, clamped to the end of a shorter month.
export function addMonths(iso, months) {
  const { year, month, day } = parts(iso);
  const first = new Date(Date.UTC(year, month - 1 + months, 1));
  const last = new Date(Date.UTC(first.getUTCFullYear(), first.getUTCMonth() + 1, 0)).getUTCDate();
  return format(first.getUTCFullYear(), first.getUTCMonth() + 1, Math.min(day, last));
}

export function addDays(iso, days) {
  const { year, month, day } = parts(iso);
  return new Date(Date.UTC(year, month - 1, day + days)).toISOString().slice(0, 10);
}

// A balance on the same day of each month, from `months` ago to today, ending on
// `end` and moving by `step` a month, with a little wobble so a chart isn't a line.
function history(today, months, end, step) {
  const points = [];
  for (let back = months; back >= 0; back -= 1) {
    const wobble = back === 0 ? 0 : ((back * 7) % 5) * Math.abs(step) * 0.1;
    points.push({ on: addMonths(today, -back), minor: end - back * step + Math.round(wobble) });
  }
  return points;
}

const CATEGORIES = [
  {
    group: 'Home',
    categories: [
      ['Mortgage', 'expense'],
      ['Utilities', 'expense'],
      ['Home insurance', 'expense'],
      ['Repairs', 'expense'],
    ],
  },
  {
    group: 'Daily life',
    categories: [
      ['Groceries', 'expense'],
      ['Eating out', 'expense'],
      ['Car', 'expense'],
      ['Subscriptions', 'expense'],
      ['Holiday', 'expense'],
    ],
  },
  { group: 'Earnings', categories: [['Salary', 'income']] },
];

// What the seed does in the API's own words, apart from ids it learns on the way.
export function household(today) {
  const accounts = [
    {
      key: 'current',
      name: 'Everyday Current',
      kind: 'checking',
      currency: 'EUR',
      history: history(today, HISTORY_MONTHS, 845_000, 6_000),
    },
    {
      key: 'saver',
      name: 'Rainy Day Saver',
      kind: 'savings',
      currency: 'EUR',
      history: history(today, HISTORY_MONTHS, 2_200_000, 25_000),
    },
    {
      key: 'pounds',
      name: 'Thistledown Mutual Saver',
      kind: 'savings',
      currency: 'GBP',
      // Out of the forecast, which adds up one currency: it counts in net worth only.
      includeInForecast: false,
      history: history(today, HISTORY_MONTHS, 640_000, 4_000),
    },
    {
      key: 'brokerage',
      name: 'Quillfeather Brokerage',
      kind: 'brokerage',
      currency: 'USD',
      history: history(today, HISTORY_MONTHS, 5_120_000, 70_000),
    },
    {
      key: 'home',
      name: 'Family Home',
      kind: 'property',
      currency: 'EUR',
      history: history(today, HISTORY_MONTHS, 32_000_000, 60_000),
    },
    {
      key: 'mortgage',
      name: 'Home Mortgage',
      kind: 'mortgage',
      currency: 'EUR',
      history: history(today, HISTORY_MONTHS, -21_000_000, 55_000),
    },
    {
      key: 'car-loan',
      name: 'Car Loan',
      kind: 'loan',
      currency: 'EUR',
      history: history(today, 24, -1_180_000, 32_000),
    },
    {
      key: 'college',
      name: 'College Fund',
      kind: 'custodial',
      currency: 'EUR',
      beneficiary: 'Robin',
      history: history(today, 30, 1_450_000, 25_000),
    },
    {
      key: 'giving',
      name: 'Giving Fund',
      kind: 'charitable',
      currency: 'USD',
      history: history(today, 12, 300_000, 5_000),
    },
    {
      // Emptied a year ago and closed along the way.
      key: 'old-franc',
      name: 'Old Franc Account',
      kind: 'checking',
      currency: 'CHF',
      closed: true,
      history: [
        ...history(addMonths(today, -12), 18, 150_000, 3_000).slice(0, -1),
        { on: addMonths(today, -12), minor: 0 },
      ],
    },
  ];

  const everyMonth = { type: 'recurring', every: 1, unit: 'months', anchor_on: today };
  const planItems = [
    {
      name: 'Salary',
      category: 'Salary',
      amount: 420_000,
      currency: 'EUR',
      direction: 'inflow',
      schedule: everyMonth,
    },
    {
      name: 'Mortgage payment',
      category: 'Mortgage',
      amount: 135_000,
      currency: 'EUR',
      direction: 'outflow',
      schedule: everyMonth,
    },
    {
      name: 'Electricity and water',
      category: 'Utilities',
      amount: 18_000,
      currency: 'EUR',
      direction: 'outflow',
      schedule: everyMonth,
    },
    {
      name: 'Car loan payment',
      category: 'Car',
      amount: 32_000,
      currency: 'EUR',
      direction: 'outflow',
      schedule: everyMonth,
    },
    {
      name: 'Music subscription',
      category: 'Subscriptions',
      amount: 1_500,
      currency: 'EUR',
      direction: 'outflow',
      schedule: everyMonth,
    },
    {
      name: 'Groceries budget',
      category: 'Groceries',
      amount: 60_000,
      currency: 'EUR',
      direction: 'outflow',
      spending: 'budget',
      schedule: everyMonth,
    },
    {
      name: 'Home insurance',
      category: 'Home insurance',
      amount: 96_000,
      currency: 'EUR',
      direction: 'outflow',
      schedule: { type: 'recurring', every: 1, unit: 'years', anchor_on: addMonths(today, 2) },
    },
    {
      name: 'Boiler service',
      category: 'Repairs',
      amount: 22_000,
      currency: 'EUR',
      direction: 'outflow',
      schedule: { type: 'one_off', on: addMonths(today, 2) },
    },
    {
      name: 'Summer holiday',
      category: 'Holiday',
      amount: 300_000,
      currency: 'EUR',
      direction: 'outflow',
      schedule: { type: 'goal', start_on: addMonths(today, -3), due_on: addMonths(today, 6) },
    },
  ];

  const transactions = [];
  const onCurrent = (on, minor, description, category) =>
    transactions.push({ on, minor, description, category });
  for (let back = HISTORY_MONTHS; back >= 0; back -= 1) {
    const day = addMonths(today, -back);
    onCurrent(day, 420_000, 'Salary, Hollowbrook Studio', 'Salary');
    onCurrent(day, -135_000, 'Mortgage payment', 'Mortgage');
    onCurrent(day, -18_000, 'Electricity and water', 'Utilities');
    onCurrent(day, -8_540, 'Fernhill Market', 'Groceries');
    if (back > 0) {
      onCurrent(addDays(day, -7), -12_310, 'Fernhill Market', 'Groceries');
      onCurrent(addDays(day, -14), -9_875, 'Fernhill Market', 'Groceries');
      onCurrent(addDays(day, -10), -4_620, 'The Copper Kettle', 'Eating out');
    }
  }

  return {
    today,
    settings: {
      display_currency: 'EUR',
      default_horizon_months: 12,
      include_income: true,
      floor: { kind: 'fixed', amount: { minor_units: 100_000, currency: 'EUR' } },
    },
    categories: CATEGORIES,
    accounts,
    planItems,
    transactions,
    // Claimed once, under a new name each time, when the template is seeded. The
    // second account stays unbound, for a person or a test to bind.
    simplefinClaim: 'https://bridge.simplefin.org/claim/healthy',
    boundProviderAccount: {
      providerName: 'Bramblewood Card',
      kind: 'credit_card',
      name: 'Bramblewood Card',
    },
  };
}
