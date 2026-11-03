// The SimpleFin bridge. A setup token is the base64 of a claim URL, whose path
// picks the scenario and a one-time name: /claim/<scenario>/<name>. Claiming it
// answers an access URL whose user is the scenario, so every later fetch through
// that connection behaves that way.

export const SIMPLEFIN_HOSTS = ['bridge.simplefin.org', 'beta-bridge.simplefin.org'];

const PASSWORD = 'made-up-staging-password';
const CLAIM_PATH = /^\/claim\/([a-z-]+)\/([\w-]+)$/;
const ACCOUNTS_PATH = '/simplefin/accounts';
const DAY_SECONDS = 86_400;
const HISTORY_DAYS = 60;
const SCENARIOS = ['healthy', 'outage', 'cut-off', 'bad-json', 'revoked', 'rate-limited'];

// A claim works once, as on the real bridge.
const claimed = new Set();

function claim(request, response, path) {
  const match = CLAIM_PATH.exec(path);
  const [scenario, name] = match ? match.slice(1) : [];
  if (request.method !== 'POST' || !SCENARIOS.includes(scenario) || claimed.has(name)) {
    response.writeHead(403).end();
    return;
  }
  claimed.add(name);
  const host = request.headers.host;
  response.writeHead(200, { 'content-type': 'text/plain' });
  response.end(`https://${scenario}:${PASSWORD}@${host}/simplefin`);
}

// A purchase every five days. An id comes from its day, so syncing again on a
// later day adds the new ones and repeats none.
function cardTransactions(nowSeconds, startSeconds) {
  const today = Math.floor(nowSeconds / DAY_SECONDS);
  const transactions = [];
  for (let day = today - HISTORY_DAYS; day <= today; day += 1) {
    const posted = day * DAY_SECONDS;
    if (day % 5 === 0 && posted >= startSeconds) {
      transactions.push({
        id: `card-${day}`,
        posted,
        amount: `-${20 + (day % 7) * 5}.50`,
        description: `CORNER GROCER ${day % 3}`,
        payee: 'Corner Grocer',
      });
    }
  }
  return transactions;
}

function accountSet(nowSeconds, startSeconds) {
  const org = { name: 'Bramblewood Bank', 'sfin-url': 'https://bridge.simplefin.org/simplefin' };
  return {
    errors: [],
    accounts: [
      {
        org,
        id: 'bramblewood-card',
        name: 'Bramblewood Card',
        currency: 'EUR',
        balance: '-1234.56',
        'balance-date': nowSeconds,
        transactions: cardTransactions(nowSeconds, startSeconds),
      },
      {
        org,
        id: 'bramblewood-travel',
        name: 'Bramblewood Travel Saver',
        currency: 'USD',
        balance: '2500.00',
        'balance-date': nowSeconds,
        transactions: [],
      },
    ],
  };
}

function scenarioOf(request) {
  const [kind, encoded] = (request.headers.authorization ?? '').split(' ');
  if (kind !== 'Basic' || !encoded) return undefined;
  const [user, password] = Buffer.from(encoded, 'base64').toString().split(':');
  return password === PASSWORD && SCENARIOS.includes(user) ? user : undefined;
}

function accounts(request, response, url, now) {
  const scenario = scenarioOf(request);
  if (scenario === undefined || scenario === 'revoked') {
    response.writeHead(403).end();
    return;
  }
  if (scenario === 'outage') {
    response.writeHead(503).end();
    return;
  }
  if (scenario === 'rate-limited') {
    response.writeHead(429).end();
    return;
  }
  const nowSeconds = Math.floor(now.getTime() / 1000);
  const start = Number(url.searchParams.get('start-date') ?? 0);
  const body = JSON.stringify(accountSet(nowSeconds, start));
  const half = body.slice(0, Math.floor(body.length / 2));
  if (scenario === 'bad-json') {
    // A complete response whose body stops halfway.
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(half);
    return;
  }
  if (scenario === 'cut-off') {
    // Promises the whole body, then the connection drops.
    response.writeHead(200, {
      'content-type': 'application/json',
      'content-length': Buffer.byteLength(body),
    });
    response.write(half, () => request.socket.destroy());
    return;
  }
  response.writeHead(200, { 'content-type': 'application/json' });
  response.end(body);
}

export function simplefin(request, response, now) {
  const url = new URL(request.url, `https://${request.headers.host}`);
  if (url.pathname === ACCOUNTS_PATH && request.method === 'GET') {
    accounts(request, response, url, now);
  } else {
    claim(request, response, url.pathname);
  }
}
