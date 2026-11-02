// The European Central Bank's data API, as far as coffer reads it. Rates never
// change, so a total in the display currency doesn't depend on the day of a run.
// A currency picks its scenario: these are real ECB currencies the seed doesn't
// use, so a test asks for a failure by adding an account in one.

export const ECB_HOST = 'data-api.ecb.europa.eu';

export const RATES = { CHF: '0.9400', GBP: '0.8500', JPY: '160.00', USD: '1.1000' };

const FAILURES = {
  DKK: 'not-published',
  NOK: 'outage',
  SEK: 'cut-off',
};

const SERIES_START = '2020-01-01';
const HEADER = 'KEY,FREQ,CURRENCY,CURRENCY_DENOM,EXR_TYPE,EXR_SUFFIX,TIME_PERIOD,OBS_VALUE';
const SERIES_PATH = /^\/service\/data\/EXR\/D\.([A-Z]{3})\.EUR\.SP00\.A$/;
const DAY_MS = 86_400_000;

// One row per weekday from `since` to `today`, as the ECB publishes none at weekends.
function rows(currency, rate, since, today) {
  const lines = [];
  const end = Date.parse(today);
  for (let at = Date.parse(since); at <= end; at += DAY_MS) {
    const weekday = new Date(at).getUTCDay();
    if (weekday !== 0 && weekday !== 6) {
      const on = new Date(at).toISOString().slice(0, 10);
      lines.push(`EXR.D.${currency}.EUR.SP00.A,D,${currency},EUR,SP00,A,${on},${rate}`);
    }
  }
  return lines;
}

export function ecb(request, response, now) {
  const url = new URL(request.url, `https://${ECB_HOST}`);
  const currency = SERIES_PATH.exec(url.pathname)?.[1];
  const scenario = FAILURES[currency];
  const rate = RATES[currency];
  if (scenario === 'outage') {
    response.writeHead(503).end();
    return;
  }
  if (scenario === 'cut-off') {
    // A full answer but for its end: the last row looks like a rate of 1.
    response.writeHead(200, { 'content-type': 'text/csv' });
    response.end(`${HEADER}\nEXR.D.${currency}.EUR.SP00.A,D,${currency},EUR,SP00,A,2026-01-05,1`);
    return;
  }
  if (scenario === 'not-published' || rate === undefined) {
    response.writeHead(404).end();
    return;
  }
  const since = url.searchParams.get('startPeriod') ?? SERIES_START;
  const today = now.toISOString().slice(0, 10);
  const body = [HEADER, ...rows(currency, rate, since, today)].join('\n');
  response.writeHead(200, { 'content-type': 'text/csv' });
  response.end(`${body}\n`);
}
