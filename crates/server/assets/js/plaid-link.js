// The Plaid Link pages: opens Link with the page's link token and reports the
// result to coffer. Plaid's own script defines `Plaid`; this file loads
// nothing. It never logs a token and never writes Plaid's text into the page
// or a report. coffer.js, loaded first, catches this page's uncaught errors.
'use strict';

(() => {
  const LOAD_FAILED = 'Plaid Link could not be loaded. Reload the page to try again.';
  const STOPPED = 'Plaid Link stopped before finishing. Reload the page to try again.';
  // The server's rule for a label (`ConnectionLabel`), checked before a member
  // spends a bank login on a label it would refuse.
  const LABEL_INVALID = 'The label must be 1 to 100 characters without control characters.';
  const CONTROL_CHARACTER = /\p{Cc}/u;
  // How long Link may take to show a sign of life once opened.
  const OPEN_TIMEOUT_MS = 15000;

  // Plaid's codes are capitals, digits and underscores; its messages stay out.
  const PLAID_CODE = /^[A-Z0-9_]{1,64}$/;

  const report = (message) => {
    if (window.coffer) window.coffer.report('plaid', message);
  };

  const show = (text, ok) => {
    const result = document.getElementById('result');
    result.textContent = text;
    result.className = ok ? 'ok' : 'error';
  };

  const post = async (path, body, csrf) => {
    const response = await fetch(path, {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
      body: JSON.stringify(body),
    });
    let data = {};
    try {
      data = await response.json();
    } catch (_) {
      data = {};
    }
    if (!response.ok) {
      const refused = new Error(data.detail || data.title || `HTTP ${response.status}`);
      refused.status = response.status;
      throw refused;
    }
    return data;
  };

  document.addEventListener('DOMContentLoaded', () => {
    const button = document.getElementById('plaid-link');
    if (!button) return;
    const fail = (text) => {
      button.disabled = true;
      show(text, false);
    };
    if (typeof Plaid === 'undefined') {
      report('script not loaded');
      fail(LOAD_FAILED);
      return;
    }
    const { linkToken, csrf, submit } = button.dataset;
    const field = document.getElementById('plaid-label');
    let label = '';
    let silent = null;
    let gaveUp = false;

    // Link has answered: it is not stuck, even if it took longer than allowed.
    const alive = () => {
      clearTimeout(silent);
      if (gaveUp) show('', true);
      gaveUp = false;
    };

    let handler;
    try {
      handler = Plaid.create({
        token: linkToken,
        onEvent: alive,
        onSuccess: async (publicToken) => {
          alive();
          show('Saving the connection…', true);
          // Update mode leaves the access token as it is: its public token is
          // not needed, so it is not sent.
          const body = field ? { label, public_token: publicToken } : {};
          try {
            const done = await post(submit, body, csrf);
            window.location.assign(done.redirect);
          } catch (error) {
            // A link token and a public token work once: only a new page helps.
            report(error.status ? `saving: the server answered ${error.status}` : 'saving failed');
            show(`${error.message} Reload the page to try again.`, false);
          }
        },
        onExit: (error) => {
          alive();
          if (error) {
            // The link token may be dead: only a new page mints another.
            const code = PLAID_CODE.test(error.error_code) ? error.error_code : 'no code';
            report(`link exited: ${code}`);
            fail(STOPPED);
          } else {
            button.disabled = false;
          }
        },
      });
    } catch (_) {
      report('create failed');
      fail(LOAD_FAILED);
      return;
    }

    button.addEventListener('click', () => {
      if (field) {
        const text = field.value.trim();
        const usable = text !== '' && !CONTROL_CHARACTER.test(text);
        field.setCustomValidity(usable ? '' : LABEL_INVALID);
        if (!field.reportValidity()) return;
        label = text;
      }
      button.disabled = true;
      show('', true);
      silent = setTimeout(() => {
        gaveUp = true;
        report('open timed out');
        fail(LOAD_FAILED);
      }, OPEN_TIMEOUT_MS);
      try {
        handler.open();
      } catch (_) {
        clearTimeout(silent);
        report('open failed');
        fail(LOAD_FAILED);
      }
    });
  });
})();
