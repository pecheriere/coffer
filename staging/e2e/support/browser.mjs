// A browser's side of a test: a virtual authenticator, signing in, and what counts as
// the page having gone wrong.
import { eventually, expect, test as serverTest } from './fixtures.mjs';
import { member } from './server.mjs';

const QUIET_MS = 2_000;
const LOG_WAIT_LIMIT_MS = 15_000;

// The log once it has stopped growing: a browser may send a report a while after the thing it
// reports, and a test that read the log at once would pass without it.
export async function settledLogs(server) {
  const limit = Date.now() + LOG_WAIT_LIMIT_MS;
  let logs = await server.logs();
  let changed = Date.now();
  while (Date.now() - changed < QUIET_MS && Date.now() < limit) {
    await new Promise((resolve) => {
      setTimeout(resolve, 250);
    });
    const now = await server.logs();
    if (now.length !== logs.length) {
      logs = now;
      changed = Date.now();
    }
  }
  return logs;
}

// Reads the log the way an operator would, per docs/configuration.md.
export function browserReports(logs) {
  const reports = [];
  for (const line of logs.split('\n')) {
    if (line.includes('"trust":"untrusted"')) {
      const { kind, page, client_message: message } = JSON.parse(line).fields;
      reports.push(`browser report on ${page}: ${kind}: ${message}`);
    }
  }
  return reports;
}

export const test = serverTest.extend({
  // For the one test about catching a problem.
  expectProblems: [false, { option: true }],

  // What the browser said went wrong, gathered all through a test. Console warnings are left
  // out on purpose: browsers use them for advice about a page, not for something broken.
  consoleProblems: async ({ page }, use) => {
    const problems = [];
    page.on('console', (message) => {
      if (message.type() === 'error') problems.push(`console: ${message.text()}`);
    });
    page.on('pageerror', (error) => problems.push(`uncaught: ${error.message}`));
    await use(problems);
  },

  // Runs for every test. A violation of the content security policy shows twice: on the
  // console here, and in the report the browser sends to the server's log.
  noBrowserProblems: [
    async ({ page, consoleProblems, server, expectProblems }, use) => {
      await use();
      if (!expectProblems) {
        // Leaving the page makes the browser send what it has been holding back.
        await page.goto('about:blank');
        const reports = browserReports(await settledLogs(server));
        expect([...consoleProblems, ...reports]).toEqual([]);
      }
    },
    { auto: true },
  ],

  // A virtual authenticator on the page, answering at once with a verified user. It holds
  // what a test enrolls; `holdPasskeyOf` loads a seeded member's key.
  authenticator: async ({ page }, use) => {
    const session = await page.context().newCDPSession(page);
    await session.send('WebAuthn.enable');
    const { authenticatorId } = await session.send('WebAuthn.addVirtualAuthenticator', {
      options: {
        protocol: 'ctap2',
        transport: 'internal',
        hasResidentKey: true,
        hasUserVerification: true,
        isUserVerified: true,
        automaticPresenceSimulation: true,
      },
    });
    await use({ session, authenticatorId });
  },
});

export { eventually, expect };

async function holdPasskeyOf({ session, authenticatorId }, username) {
  const { credentialId, privateKeyPkcs8 } = member(username);
  await session.send('WebAuthn.addCredential', {
    authenticatorId,
    credential: {
      credentialId: Buffer.from(credentialId, 'base64url').toString('base64'),
      rpId: 'localhost',
      privateKey: privateKeyPkcs8,
      isResidentCredential: false,
      signCount: 0,
    },
  });
}

// Signs in through the login page with the passkey the authenticator holds.
export async function signIn(page, server, username) {
  await page.goto(`${server.base}/login`);
  await page.getByLabel('Username').fill(username);
  await page.getByRole('button', { name: 'Sign in with a passkey' }).click();
  await page.waitForURL((url) => url.pathname !== '/login');
}

export async function signInSeeded(page, server, authenticator, username) {
  await holdPasskeyOf(authenticator, username);
  await signIn(page, server, username);
}
