// Hand-written JSON-RPC to a server's MCP endpoint, as the protocol has a client send it, with
// no SDK between a test and the wire.
import Ajv2020 from 'ajv/dist/2020.js';
import addFormats from 'ajv-formats';

export const OLDER_REVISION = '2025-11-25';
export const CURRENT_REVISION = '2026-07-28';

export class Mcp {
  constructor(base, token, headers = {}) {
    this.base = base;
    this.token = token;
    this.headers = headers;
  }

  async post(message, headers = {}) {
    const response = await fetch(`${this.base}/mcp`, {
      method: 'POST',
      headers: {
        ...(this.token ? { Authorization: `Bearer ${this.token}` } : {}),
        'Content-Type': 'application/json',
        Accept: 'application/json, text/event-stream',
        'MCP-Protocol-Version': OLDER_REVISION,
        ...this.headers,
        ...headers,
      },
      body: JSON.stringify(message),
    });
    const text = await response.text();
    const type = response.headers.get('content-type') ?? '';
    return {
      status: response.status,
      headers: response.headers,
      body: type.includes('json') ? JSON.parse(text) : text,
    };
  }

  request(method, params) {
    return this.post({ jsonrpc: '2.0', id: 1, method, params });
  }

  async tools() {
    const { body } = await this.request('tools/list', {});
    return body.result.tools;
  }

  async call(name, args = {}) {
    const { status, body } = await this.request('tools/call', { name, arguments: args });
    if (status !== 200) throw new Error(`tools/call ${name} answered ${status}`);
    return body.result;
  }
}

// The problems with `value` against a tool's `outputSchema`, none if it matches. A client may
// check this, so a mismatch would break the tool for it.
export function problemsAgainst(schema, value) {
  const ajv = new Ajv2020({
    strict: false,
    allErrors: true,
    formats: { int32: true, int64: true, password: true },
  });
  addFormats(ajv);
  const validate = ajv.compile(schema);
  if (validate(value)) return [];
  return validate.errors.map(({ instancePath, message }) => `${instancePath} ${message}`);
}
