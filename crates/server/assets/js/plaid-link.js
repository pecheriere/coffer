// The Plaid Link pages: opens Link with the page's link token and reports the
// result to coffer. Plaid's own script defines `Plaid`; this file loads
// nothing. It never logs a token and never writes Plaid's text into the page.
'use strict';

(() => {
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
    if (!response.ok) throw new Error(data.detail || data.title || `HTTP ${response.status}`);
    return data;
  };

  document.addEventListener('DOMContentLoaded', () => {
    const button = document.getElementById('plaid-link');
    if (!button) return;
    if (typeof Plaid === 'undefined') {
      button.disabled = true;
      show('Plaid Link could not be loaded. Reload the page to try again.', false);
      return;
    }
    const { linkToken, csrf, submit } = button.dataset;
    const field = document.getElementById('plaid-label');
    let label = '';

    const handler = Plaid.create({
      token: linkToken,
      onSuccess: async (publicToken) => {
        show('Saving the connection…', true);
        // Update mode leaves the access token as it is: its public token is
        // not needed, so it is not sent.
        const body = field ? { label, public_token: publicToken } : {};
        try {
          const done = await post(submit, body, csrf);
          window.location.assign(done.redirect);
        } catch (error) {
          // A link token and a public token work once: only a new page helps.
          show(`${error.message} Reload the page to try again.`, false);
        }
      },
      onExit: (error) => {
        button.disabled = false;
        if (error) show('Plaid Link stopped before finishing. You can try again.', false);
      },
    });

    button.addEventListener('click', () => {
      if (field) {
        if (!field.reportValidity()) return;
        label = field.value;
      }
      button.disabled = true;
      show('', true);
      handler.open();
    });
  });
})();
