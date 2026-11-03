// The two ways into coffer the seed uses, both the real ones: the pages, with a
// session from a passkey ceremony, for what only a person can do, and the API with
// an agent token for everything else.
import { assertion, register } from './webauthn.mjs';

const CSRF_FIELD = /name="csrf" value="([^"]+)"/;
const NEW_LINK = /<code id="new-link">([^<]+)<\/code>/;
const NEW_TOKEN = /<code id="[^"]*">(coffer[^<]+)<\/code>/;

async function failIfRefused(response, what) {
  // Not `ok`: a page answers a form with a redirect, which these clients don't follow.
  if (response.status >= 400) {
    throw new Error(`${what} answered ${response.status}: ${await response.text()}`);
  }
  return response;
}

// A browser, as far as the pages can tell: a session cookie and the public origin.
export class PageSession {
  constructor(base, origin) {
    this.base = base;
    this.origin = origin;
    this.cookie = undefined;
  }

  async post(path, headers, body) {
    const response = await fetch(`${this.base}${path}`, {
      method: 'POST',
      redirect: 'manual',
      headers: {
        Origin: this.origin,
        ...(this.cookie ? { Cookie: this.cookie } : {}),
        ...headers,
      },
      body,
    });
    const set = response.headers.get('set-cookie');
    if (set) [this.cookie] = set.split(';');
    return failIfRefused(response, `POST ${path}`);
  }

  postJson(path, value) {
    return this.post(path, { 'Content-Type': 'application/json' }, JSON.stringify(value));
  }

  // Redeems a setup or invite link with a fixture passkey, as coffer.js does.
  async enroll(link, member) {
    const token = new URL(link).pathname.split('/').pop();
    const begun = await this.postJson('/auth/enroll/begin', {
      token,
      username: member.username,
      display_name: member.displayName,
    });
    const { ceremony, options } = await begun.json();
    await this.postJson('/auth/enroll/finish', {
      ceremony,
      label: 'fixture passkey',
      credential: register(options, member, this.origin),
    });
  }

  async login(member) {
    const begun = await this.postJson('/auth/login/begin', { username: member.username });
    const { ceremony, options } = await begun.json();
    await this.postJson('/auth/login/finish', {
      ceremony,
      credential: assertion(options, member, this.origin),
    });
  }

  async page(path) {
    const response = await fetch(`${this.base}${path}`, { headers: { Cookie: this.cookie } });
    await failIfRefused(response, `GET ${path}`);
    return response.text();
  }

  // Posts a form holding the page's anti-forgery token and answers the page it renders.
  async submit(formPage, path, fields) {
    const csrf = CSRF_FIELD.exec(await this.page(formPage))?.[1];
    if (!csrf) throw new Error(`${formPage} has no anti-forgery token`);
    const response = await this.post(
      path,
      { 'Content-Type': 'application/x-www-form-urlencoded' },
      new URLSearchParams({ csrf, ...fields }).toString(),
    );
    return response.text();
  }

  async createInvite(role, displayName) {
    const page = await this.submit('/admin/invites', '/admin/invites', {
      role,
      display_name: displayName,
    });
    const link = NEW_LINK.exec(page)?.[1];
    if (!link) throw new Error('the invites page shows no new link');
    return link;
  }

  // `levels` is the grant form's selects: `all` and `account_<id>`.
  async createToken(name, scope, levels = {}) {
    const page = await this.submit('/account', '/account/tokens', { name, scope, ...levels });
    const secret = NEW_TOKEN.exec(page)?.[1];
    if (!secret) throw new Error('the account page shows no new token');
    return secret;
  }
}

export class Api {
  constructor(base, token) {
    this.base = base;
    this.token = token;
  }

  async request(method, path, body) {
    const response = await fetch(`${this.base}/api/v1${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${this.token}`,
        ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    await failIfRefused(response, `${method} ${path}`);
    return response.status === 204 ? undefined : response.json();
  }

  get(path) {
    return this.request('GET', path);
  }

  post(path, body) {
    return this.request('POST', path, body);
  }

  put(path, body) {
    return this.request('PUT', path, body);
  }

  patch(path, body) {
    return this.request('PATCH', path, body);
  }
}

// Asks until `check` answers something, as the jobs behind it run on their own time.
export async function until(what, check, { attempts = 180, pauseMs = 1000 } = {}) {
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    const answer = await check();
    if (answer) return answer;
    await new Promise((resolve) => {
      setTimeout(resolve, pauseMs);
    });
  }
  throw new Error(`gave up waiting for ${what}`);
}
