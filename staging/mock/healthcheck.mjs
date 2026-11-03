// Passes once the mock providers accept connections.
import { connect } from 'node:net';

const socket = connect(443, '127.0.0.1', () => {
  socket.end();
  process.exit(0);
});
socket.on('error', () => process.exit(1));
