// Passkeys (WebAuthn) for the sign-in page and account settings. No build
// step: plain browser APIs, progressive enhancement over the server forms.
(() => {
  const supported =
    typeof window.PublicKeyCredential === 'function' &&
    !!navigator.credentials &&
    typeof navigator.credentials.create === 'function';
  const b64 = {
    decode(text) {
      let s = text.replace(/-/g, '+').replace(/_/g, '/');
      if (s.length % 4) s += '='.repeat(4 - (s.length % 4));
      const bin = atob(s);
      const out = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
      return out.buffer;
    },
    encode(buffer) {
      let bin = '';
      for (const byte of new Uint8Array(buffer)) bin += String.fromCharCode(byte);
      return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
    },
  };
  const post = (url, body, json) =>
    fetch(url, {
      method: 'POST',
      credentials: 'same-origin',
      headers: json
        ? { 'Content-Type': 'application/json', Accept: 'application/json' }
        : { Accept: 'application/json' },
      body,
    });
  const failure = async (response) => {
    try {
      const data = await response.json();
      if (data && typeof data.error === 'string') return new Error(data.error);
    } catch (e) {
      /* not JSON */
    }
    return new Error('Something went wrong. Please try again.');
  };
  const explain = (error) => {
    if (!error) return 'Something went wrong. Please try again.';
    if (error.name === 'NotAllowedError') return 'The passkey request was cancelled or timed out.';
    if (error.name === 'InvalidStateError') return 'This device already has a passkey for your account.';
    if (error.name === 'SecurityError') return 'Passkeys only work on the site’s own address over HTTPS.';
    return error.message || 'Something went wrong. Please try again.';
  };
  const creationOptions = (pk) => ({
    ...pk,
    challenge: b64.decode(pk.challenge),
    user: { ...pk.user, id: b64.decode(pk.user.id) },
    excludeCredentials: (pk.excludeCredentials || []).map((c) => ({ ...c, id: b64.decode(c.id) })),
  });
  const requestOptions = (pk) => ({
    ...pk,
    challenge: b64.decode(pk.challenge),
    allowCredentials: (pk.allowCredentials || []).map((c) => ({ ...c, id: b64.decode(c.id) })),
  });

  // --- Account settings: add a passkey ---------------------------------------
  const form = document.querySelector('[data-passkey-register]');
  if (form) {
    const status = form.querySelector('[data-passkey-status]');
    const button = form.querySelector('button[type=submit]');
    if (!supported) {
      status.textContent = 'This browser does not support passkeys.';
      button.disabled = true;
    } else {
      form.addEventListener('submit', async (event) => {
        event.preventDefault();
        button.disabled = true;
        status.textContent = 'Confirming your identity…';
        try {
          const options = await post(form.dataset.optionsUrl, new URLSearchParams(new FormData(form)));
          if (!options.ok) throw await failure(options);
          const { publicKey } = await options.json();
          status.textContent = 'Follow your browser’s prompt to create the passkey…';
          const credential = await navigator.credentials.create({ publicKey: creationOptions(publicKey) });
          const response = credential.response;
          const transports = typeof response.getTransports === 'function' ? response.getTransports() : [];
          const saved = await post(
            form.dataset.registerUrl,
            JSON.stringify({
              name: form.elements.name.value,
              id: credential.id,
              rawId: b64.encode(credential.rawId),
              type: credential.type,
              authenticatorAttachment: credential.authenticatorAttachment || null,
              response: {
                clientDataJSON: b64.encode(response.clientDataJSON),
                attestationObject: b64.encode(response.attestationObject),
                transports,
              },
            }),
            true,
          );
          if (!saved.ok) throw await failure(saved);
          const { redirect } = await saved.json();
          location.assign(redirect || '/settings?saved=passkey');
        } catch (error) {
          status.textContent = explain(error);
          button.disabled = false;
        }
      });
    }
  }

  // --- Sign-in page: passkey button and autofill (conditional UI) ------------
  const panel = document.querySelector('[data-passkey-signin]');
  if (panel && supported) {
    panel.hidden = false;
    const status = panel.querySelector('[data-passkey-status]');
    const button = panel.querySelector('[data-passkey-button]');
    let conditional = null;
    const signIn = async (mediation) => {
      if (conditional) conditional.abort();
      const controller = new AbortController();
      conditional = mediation === 'conditional' ? controller : null;
      const options = await post(panel.dataset.optionsUrl, null);
      if (!options.ok) throw await failure(options);
      const { publicKey } = await options.json();
      const credential = await navigator.credentials.get({
        publicKey: requestOptions(publicKey),
        mediation,
        signal: controller.signal,
      });
      const response = credential.response;
      const result = await post(
        panel.dataset.signInUrl,
        JSON.stringify({
          id: credential.id,
          rawId: b64.encode(credential.rawId),
          type: credential.type,
          response: {
            clientDataJSON: b64.encode(response.clientDataJSON),
            authenticatorData: b64.encode(response.authenticatorData),
            signature: b64.encode(response.signature),
            userHandle: response.userHandle ? b64.encode(response.userHandle) : null,
          },
        }),
        true,
      );
      if (!result.ok) throw await failure(result);
      const { redirect } = await result.json();
      location.assign(redirect || '/dashboard');
    };
    button.addEventListener('click', async () => {
      button.disabled = true;
      status.textContent = '';
      try {
        await signIn('optional');
      } catch (error) {
        if (error && error.name !== 'AbortError') status.textContent = explain(error);
        button.disabled = false;
      }
    });
    if (typeof PublicKeyCredential.isConditionalMediationAvailable === 'function') {
      PublicKeyCredential.isConditionalMediationAvailable().then((available) => {
        if (!available) return;
        // Autofill requests end quietly when the user ignores them or the
        // browser has nothing to offer; only server-side failures are shown.
        signIn('conditional').catch((error) => {
          if (error && error.name !== 'AbortError' && error.name !== 'NotAllowedError') {
            status.textContent = explain(error);
          }
        });
      });
    }
  }
})();
