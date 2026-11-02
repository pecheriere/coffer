// A browser's side of a test: a virtual authenticator, signing in, and what counts as
// the page having gone wrong.
import { eventually, expect, test as serverTest } from './fixtures.mjs';
import { member } from './server.mjs';

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

  // What the browser said went wrong, gathered all through a test.
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
    async ({ consoleProblems, server, expectProblems }, use) => {
      await use();
      if (!expectProblems) {
        const reports = browserReports(await server.logs());
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
