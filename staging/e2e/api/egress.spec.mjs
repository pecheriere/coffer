// The real egress settings: a setup token only gets coffer to talk to an allowed host.
import { expect, test } from '../support/fixtures.mjs';

for (const [name, claim] of [
  ['another host', 'https://example.com/claim/healthy/not-allowed'],
  ['an address', 'https://172.30.90.10/claim/healthy/not-allowed'],
  ['the same host without TLS', 'http://bridge.simplefin.org/claim/healthy/not-allowed'],
  ['an allowed name inside another', 'https://bridge.simplefin.org.example.com/claim/healthy/x'],
]) {
  test(`a setup token naming ${name} is refused`, async ({ api }) => {
    const { body: before } = await api.get('/connections');

    const { status, body } = await api.post('/connections/simplefin', {
      label: 'Should not exist',
      setup_token: Buffer.from(claim).toString('base64'),
    });

    expect(status).toBe(422);
    expect(body.code).toBe('setup_token_invalid');
    const { body: after } = await api.get('/connections');
    expect(after.items.length).toBe(before.items.length);
  });
}
