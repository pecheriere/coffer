// A software authenticator holding one fixture key: just enough WebAuthn to enroll
// and sign in through the same ceremonies a browser uses. Node's standard library has no CBOR, so
// the two small structures registration needs are written out by hand.
import { createHash, createPrivateKey, createPublicKey, sign } from 'node:crypto';

const USER_PRESENT_AND_VERIFIED = 0x05;
const ATTESTED_CREDENTIAL = 0x40;
const NO_AAGUID = Buffer.alloc(16);

const cborHead = (major, length) =>
  length < 24
    ? Buffer.from([(major << 5) | length])
    : length < 256
      ? Buffer.from([(major << 5) | 24, length])
      : Buffer.from([(major << 5) | 25, length >> 8, length & 0xff]);
const cborBytes = (bytes) => Buffer.concat([cborHead(2, bytes.length), bytes]);
const cborText = (text) => Buffer.concat([cborHead(3, Buffer.byteLength(text)), Buffer.from(text)]);

const sha256 = (data) => createHash('sha256').update(data).digest();
const uint32 = (value) => {
  const bytes = Buffer.alloc(4);
  bytes.writeUInt32BE(value);
  return bytes;
};

// The public half as a COSE key: EC2, ES256, P-256, x and y.
function coseKey(privateKey) {
  const { x, y } = createPublicKey(privateKey).export({ format: 'jwk' });
  return Buffer.concat([
    Buffer.from([0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21]),
    cborBytes(Buffer.from(x, 'base64url')),
    Buffer.from([0x22]),
    cborBytes(Buffer.from(y, 'base64url')),
  ]);
}

function privateKeyOf(member) {
  return createPrivateKey({
    key: Buffer.from(member.privateKeyPkcs8, 'base64'),
    format: 'der',
    type: 'pkcs8',
  });
}

// Answers `options`, the server's creation options, as navigator.credentials.create
// and coffer.js would.
export function register(options, member, origin) {
  const { challenge, rp } = options.publicKey;
  const privateKey = privateKeyOf(member);
  const credentialId = Buffer.from(member.credentialId, 'base64url');
  const authData = Buffer.concat([
    sha256(rp.id),
    Buffer.from([USER_PRESENT_AND_VERIFIED | ATTESTED_CREDENTIAL]),
    uint32(0),
    NO_AAGUID,
    Buffer.from([credentialId.length >> 8, credentialId.length & 0xff]),
    credentialId,
    coseKey(privateKey),
  ]);
  const attestationObject = Buffer.concat([
    Buffer.from([0xa3]),
    cborText('fmt'),
    cborText('none'),
    cborText('attStmt'),
    Buffer.from([0xa0]),
    cborText('authData'),
    cborBytes(authData),
  ]);
  const clientData = { type: 'webauthn.create', challenge, origin, crossOrigin: false };
  return {
    id: member.credentialId,
    rawId: member.credentialId,
    type: 'public-key',
    response: {
      attestationObject: attestationObject.toString('base64url'),
      clientDataJSON: Buffer.from(JSON.stringify(clientData)).toString('base64url'),
    },
    extensions: {},
  };
}

// Answers `options`, the server's request options, as navigator.credentials.get and
// coffer.js would. The signature count stays 0: an authenticator may keep none.
export function assertion(options, member, origin) {
  const { challenge, rpId } = options.publicKey;
  const authData = Buffer.concat([
    sha256(rpId),
    Buffer.from([USER_PRESENT_AND_VERIFIED]),
    uint32(0),
  ]);
  const clientData = Buffer.from(
    JSON.stringify({ type: 'webauthn.get', challenge, origin, crossOrigin: false }),
  );
  const signature = sign('sha256', Buffer.concat([authData, sha256(clientData)]), privateKeyOf(member));
  return {
    id: member.credentialId,
    rawId: member.credentialId,
    type: 'public-key',
    response: {
      authenticatorData: authData.toString('base64url'),
      clientDataJSON: clientData.toString('base64url'),
      signature: signature.toString('base64url'),
      userHandle: null,
    },
    extensions: {},
  };
}
