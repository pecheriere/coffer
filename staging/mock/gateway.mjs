// Forwards each published port to the coffer container named after it. The
// container is looked up per connection, since test servers come and go.
import { connect, createServer } from 'node:net';

const FIRST_PORT = 18080;
const LAST_PORT = 18099;

for (let port = FIRST_PORT; port <= LAST_PORT; port += 1) {
  createServer((client) => {
    const server = connect(8080, `coffer-staging-${port}`);
    client.pipe(server).pipe(client);
    client.on('error', () => server.destroy());
    server.on('error', () => client.destroy());
  }).listen(port, '0.0.0.0');
}

process.on('SIGTERM', () => process.exit(0));
