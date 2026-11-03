// Real HTTP to a server's API. Every response, whatever a test goes on to look at, is
// checked against the OpenAPI document.
import { problemsWith } from './openapi.mjs';

// JSON if it says it is; any other body is kept as text, for the document to refuse.
function parseBody(text, type) {
  if (text === '') return undefined;
  return type.endsWith('json') ? JSON.parse(text) : text;
}

export class CheckedApi {
  constructor(base, token) {
    this.base = base;
    this.token = token;
  }

  async call(method, path, body) {
    const response = await fetch(`${this.base}/api/v1${path}`, {
      method,
      headers: {
        ...(this.token ? { Authorization: `Bearer ${this.token}` } : {}),
        ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const type = (response.headers.get('content-type') ?? '').split(';')[0];
    const text = await response.text();
    const parsed = parseBody(text, type);
    const route = path.split('?')[0];
    const problems = problemsWith(method, `/api/v1${route}`, response.status, type, parsed);
    if (problems.length > 0) {
      throw new Error(
        `${method} ${route} answered ${response.status} against the document:\n${problems.join('\n')}`,
      );
    }
    return { status: response.status, body: parsed };
  }

  get(path) {
    return this.call('GET', path);
  }

  post(path, body) {
    return this.call('POST', path, body);
  }

  put(path, body) {
    return this.call('PUT', path, body);
  }

  patch(path, body) {
    return this.call('PATCH', path, body);
  }
}
