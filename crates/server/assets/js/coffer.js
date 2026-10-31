// coffer pages: WebAuthn glue for enrollment, sign-in and adding a passkey,
// and the copy buttons. Served as /static/coffer.js; pages carry no inline
// script.
'use strict';

(() => {
  // --- base64url <-> ArrayBuffer -------------------------------------------

  const fromB64u = (text) => {
    const b64 = text.replace(/-/g, '+').replace(/_/g, '/');
    const padded = b64 + '='.repeat((4 - (b64.length % 4)) % 4);
    const bin = atob(padded);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i += 1) bytes[i] = bin.charCodeAt(i);
    return bytes.buffer;
  };

  const toB64u = (buffer) => {
    const bytes = new Uint8Array(buffer);
    let bin = '';
    for (const byte of bytes) bin += String.fromCharCode(byte);
    return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  };

  // --- requests --------------------------------------------------------------

  const show = (text, ok) => {
    const result = document.getElementById('result');
    if (!result) return;
    result.textContent = text;
    result.className = ok ? 'ok' : 'error';
  };

  const post = async (path, body, csrf) => {
    const headers = { 'Content-Type': 'application/json' };
    if (csrf) headers['X-CSRF-Token'] = csrf;
    const response = await fetch(path, {
      method: 'POST',
      credentials: 'same-origin',
      headers,
      body: JSON.stringify(body),
    });
    let data = {};
    try {
      data = await response.json();
    } catch (_) {
      data = {};
    }
    if (!response.ok) throw new Error(data.detail || data.title || `HTTP ${response.status}`);
    return data;
  };

  // --- WebAuthn ---------------------------------------------------------------

  const createCredential = async (options) => {
    const publicKey = options.publicKey;
    publicKey.challenge = fromB64u(publicKey.challenge);
    publicKey.user.id = fromB64u(publicKey.user.id);
    for (const cred of publicKey.excludeCredentials || []) cred.id = fromB64u(cred.id);
    const cred = await navigator.credentials.create({ publicKey });
    return {
      id: cred.id,
      rawId: toB64u(cred.rawId),
      type: cred.type,
      response: {
        attestationObject: toB64u(cred.response.attestationObject),
        clientDataJSON: toB64u(cred.response.clientDataJSON),
        transports: cred.response.getTransports ? cred.response.getTransports() : undefined,
      },
      extensions: cred.getClientExtensionResults(),
    };
  };

  const getAssertion = async (options) => {
    const publicKey = options.publicKey;
    publicKey.challenge = fromB64u(publicKey.challenge);
    for (const cred of publicKey.allowCredentials || []) cred.id = fromB64u(cred.id);
    const cred = await navigator.credentials.get({ publicKey });
    const response = cred.response;
    return {
      id: cred.id,
      rawId: toB64u(cred.rawId),
      type: cred.type,
      response: {
        authenticatorData: toB64u(response.authenticatorData),
        clientDataJSON: toB64u(response.clientDataJSON),
        signature: toB64u(response.signature),
        userHandle: response.userHandle ? toB64u(response.userHandle) : null,
      },
      extensions: cred.getClientExtensionResults(),
    };
  };

  const value = (id) => {
    const field = document.getElementById(id);
    return field ? field.value : '';
  };

  // Runs `ceremony` behind a button: disables it meanwhile and reports errors.
  const guarded = (button, ceremony) => {
    button.addEventListener('click', async () => {
      button.disabled = true;
      try {
        await ceremony();
      } catch (error) {
        show(error.message, false);
        button.disabled = false;
      }
    });
  };

  const go = (done) => {
    window.location.assign(done.redirect);
  };

  // --- pages ------------------------------------------------------------------

  const setupEnrollment = () => {
    const button = document.getElementById('enroll');
    if (!button) return;
    // The link's token is the last path segment: /setup/<token> or /invite/<token>.
    const token = window.location.pathname.split('/').filter(Boolean).pop();
    guarded(button, async () => {
      const begun = await post('/auth/enroll/begin', {
        token,
        username: value('username'),
        display_name: value('display-name'),
      });
      show('Create the passkey on this device…', true);
      const credential = await createCredential(begun.options);
      const done = await post('/auth/enroll/finish', {
        ceremony: begun.ceremony,
        label: value('label') || 'passkey',
        credential,
      });
      go(done);
    });
  };

  const setupLogin = () => {
    const button = document.getElementById('login');
    if (!button) return;
    guarded(button, async () => {
      const begun = await post('/auth/login/begin', { username: value('username') });
      show('Confirm with your passkey…', true);
      const credential = await getAssertion(begun.options);
      const done = await post('/auth/login/finish', { ceremony: begun.ceremony, credential });
      go(done);
    });
  };

  const setupAddPasskey = () => {
    const button = document.getElementById('add-passkey');
    if (!button) return;
    const csrf = button.dataset.csrf;
    guarded(button, async () => {
      const begun = await post('/auth/passkeys/begin', {}, csrf);
      show('Create the passkey on this device…', true);
      const credential = await createCredential(begun.options);
      const done = await post(
        '/auth/passkeys/finish',
        { ceremony: begun.ceremony, label: value('passkey-label') || 'passkey', credential },
        csrf,
      );
      go(done);
    });
  };

  const setupCopy = () => {
    for (const button of document.querySelectorAll('button[data-copy]')) {
      button.addEventListener('click', async () => {
        const source = document.getElementById(button.dataset.copy);
        if (!source) return;
        try {
          await navigator.clipboard.writeText(source.textContent);
          button.textContent = 'Copied';
        } catch (_) {
          button.textContent = 'Copy failed: select and copy by hand';
        }
      });
    }
  };

  document.addEventListener('DOMContentLoaded', () => {
    setupEnrollment();
    setupLogin();
    setupAddPasskey();
    setupCopy();
  });
})();
