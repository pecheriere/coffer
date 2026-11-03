// Answers as both mock providers on port 443, told apart by the Host header. A
// request names its own scenario (see simplefin.mjs and ecb.mjs), so the mock
// holds no mode that tests running side by side could trip over each other with.
import { readFileSync } from 'node:fs';
import { createServer } from 'node:https';
import { join } from 'node:path';

import { ecb, ECB_HOST } from './ecb.mjs';
import { simplefin, SIMPLEFIN_HOSTS } from './simplefin.mjs';

const certs = process.env.MOCK_CERT_DIR;

const server = createServer(
  {
    cert: readFileSync(join(certs, 'mock.crt')),
    key: readFileSync(join(certs, 'mock.key')),
  },
  (request, response) => {
    const host = request.headers.host;
    if (host === ECB_HOST) {
      ecb(request, response, new Date());
    } else if (SIMPLEFIN_HOSTS.includes(host)) {
      simplefin(request, response, new Date());
    } else {
      response.writeHead(421).end();
    }
  },
);

server.listen(443, '0.0.0.0');
process.on('SIGTERM', () => server.close(() => process.exit(0)));
