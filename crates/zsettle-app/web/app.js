'use strict';
// zsettle local page. Everything a seller or the chain supplies is shown as text
// (textContent), never parsed as HTML.

const LINKS = {
  github: 'https://github.com/nerses-asaturyan/zsettle',
  docs: 'https://nerses-asaturyan.github.io/zsettle/book/',
  x: 'https://x.com/NAsaturyan',
};
// Transaction pages, per network. Opening one tells the explorer which transaction you look at.
const EXPLORER = { main: 'https://zecblock.com/tx/', test: 'https://testnet.zecblock.com/tx/' };
const FEE_ZAT = 15000; // a three-action trade or payment under ZIP 317
const SYNC_EVERY_MS = 60000;

const S = {
  token: null,
  status: null,
  view: 'wallet',
  wallet: null,
  market: null,
  shop: null,
  activity: null,
  syncing: false,
  busy: false,
  timers: [],
};

// ------------------------------------------------------------ dom helpers

function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (v === null || v === undefined || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
      else if (k === 'value') el.value = v;
      else el.setAttribute(k, v === true ? '' : String(v));
    }
  }
  for (const k of kids.flat(Infinity)) {
    if (k === null || k === undefined || k === false) continue;
    el.append(k instanceof Node ? k : document.createTextNode(String(k)));
  }
  return el;
}

const ICONS = {
  wallet: ['M3 7a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v1', 'M3 7v10a2 2 0 0 0 2 2h14a1 1 0 0 0 1-1v-3', 'M20 10h-4a2 2 0 0 0 0 4h4v-4z'],
  store: ['M3 9l1.5-5h15L21 9', 'M3 9a3 3 0 0 0 6 0a3 3 0 0 0 6 0a3 3 0 0 0 6 0', 'M5 12v8h14v-8', 'M10 20v-5h4v5'],
  tag: ['M20.6 13.4l-7.2 7.2a2 2 0 0 1-2.8 0L3 13V3h10l7.6 7.6a2 2 0 0 1 0 2.8z', 'M7.5 7.5h.01'],
  clock: ['M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z', 'M12 7v5l3 2'],
  down: ['M12 5v14', 'M19 12l-7 7-7-7'],
  up: ['M12 19V5', 'M5 12l7-7 7 7'],
  refresh: ['M21 12a9 9 0 0 1-15.5 6.2L3 16', 'M3 12a9 9 0 0 1 15.5-6.2L21 8', 'M21 3v5h-5', 'M3 21v-5h5'],
  lock: ['M6 11h12v10H6z', 'M8 11V7a4 4 0 0 1 8 0v4'],
  check: ['M5 12l5 5L20 7'],
  x: ['M6 6l12 12', 'M18 6L6 18'],
  copy: ['M9 9h11v11H9z', 'M5 15H4V4h11v1'],
  history: ['M3 12a9 9 0 1 0 3-6.7L3 8', 'M3 3v5h5', 'M12 7v5l4 2'],
  plus: ['M12 5v14', 'M5 12h14'],
  shield: ['M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6l8-3z', 'M9 12l2 2 4-4'],
  finger: ['M12 11v4', 'M8 12a4 4 0 0 1 8 0v2a8 8 0 0 1-1 4', 'M5 10a7 7 0 0 1 14 0v3', 'M9 16c0 2-.5 3.5-1.5 5'],
  menu: ['M4 7h16', 'M4 12h16', 'M4 17h16'],
  chevron: ['M9 6l6 6-6 6'],
  github: ['M9 19c-4 1.5-4-2-6-2.5', 'M15 22v-3.9a3.4 3.4 0 0 0-1-2.6c3.1-.3 6.4-1.5 6.4-6.9A5.4 5.4 0 0 0 19 4.8 5 5 0 0 0 18.9 1S17.7.7 15 2.5a13.4 13.4 0 0 0-7 0C5.3.7 4.1 1 4.1 1A5 5 0 0 0 4 4.8a5.4 5.4 0 0 0-1.5 3.8c0 5.4 3.3 6.6 6.4 7a3.4 3.4 0 0 0-1 2.5V22'],
  book: ['M4 19.5A2.5 2.5 0 0 1 6.5 17H20', 'M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z'],
  xlogo: ['M4 4l11.7 16H20L8.3 4H4z', 'M4 20l6.8-6.8', 'M13.2 10.8L20 4'],
  share: ['M4 12v7a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-7', 'M16 6l-4-4-4 4', 'M12 2v13'],
  spark: ['M12 3v4', 'M12 17v4', 'M3 12h4', 'M17 12h4', 'M6 6l2.5 2.5', 'M15.5 15.5L18 18', 'M18 6l-2.5 2.5', 'M8.5 15.5L6 18'],
  bag: ['M6 7h12l1 13H5L6 7z', 'M9 7a3 3 0 0 1 6 0'],
  info: ['M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18z', 'M12 11v5', 'M12 8h.01'],
  alert: ['M12 3l9.5 17h-19L12 3z', 'M12 10v4', 'M12 17h.01'],
  external: ['M14 4h6v6', 'M20 4l-9 9', 'M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5'],
  more: ['M5 12h.01', 'M12 12h.01', 'M19 12h.01'],
  paste: ['M9 4h6v3H9z', 'M9 5H6v16h12V5h-3'],
  store2: ['M4 10v10h16V10', 'M2 10l2-6h16l2 6z', 'M9 20v-6h6v6'],
};

function icon(name, cls) {
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('viewBox', '0 0 24 24');
  svg.setAttribute('class', 'i' + (cls ? ' ' + cls : ''));
  svg.setAttribute('aria-hidden', 'true');
  for (const d of ICONS[name] || []) {
    const p = document.createElementNS(NS, 'path');
    p.setAttribute('d', d);
    if (name === 'more') p.setAttribute('stroke-width', '3');
    svg.append(p);
  }
  return svg;
}

// The mark: two halves, gold and ink, locking into a Z through a transparent cut, as a
// trade locks the seller's half and the buyer's half into one transaction.
let logoN = 0;
function logo(cls = 'logo') {
  const NS = 'http://www.w3.org/2000/svg';
  const n = ++logoN;
  const el = (tag, attrs) => {
    const e = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
    return e;
  };
  const svg = el('svg', { viewBox: '0 0 64 64', class: cls, 'aria-hidden': 'true' });
  const defs = el('defs', {});
  const clip = el('clipPath', { id: 'lc' + n });
  clip.append(el('rect', { width: 64, height: 64, rx: 18 }));
  const mask = el('mask', { id: 'lm' + n });
  mask.append(el('rect', { width: 64, height: 64, fill: '#fff' }), el('path', { d: 'M-2 22H46L18 42H66', fill: 'none', stroke: '#000', 'stroke-width': 5, 'stroke-linejoin': 'round' }));
  defs.append(clip, mask);
  const g = el('g', { 'clip-path': `url(#lc${n})`, mask: `url(#lm${n})` });
  g.append(el('path', { d: 'M0 0H64V42H18L46 22H0Z', fill: '#F4B728' }), el('path', { d: 'M0 22H46L18 42H64V64H0Z', fill: 'currentColor', class: 'logo-ink' }));
  svg.append(defs, g);
  return svg;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function zec(zat, opts = {}) {
  if (zat === null || zat === undefined) return '';
  const n = Number(zat) / 1e8;
  const s = n.toLocaleString(undefined, { maximumFractionDigits: 8 });
  return opts.bare ? s : s + ' ZEC';
}
function short(s, n = 6) {
  s = String(s || '');
  return s.length > 2 * n + 1 ? s.slice(0, n) + '…' + s.slice(-n) : s;
}
function ago(sec) {
  if (!sec) return '';
  const d = Math.max(0, Math.floor(Date.now() / 1000 - sec));
  if (d < 45) return 'just now';
  if (d < 3600) return Math.round(d / 60) + ' min ago';
  if (d < 86400) return Math.round(d / 3600) + ' h ago';
  return new Date(sec * 1000).toLocaleDateString();
}
function hash(s) {
  let x = 2166136261;
  for (const c of String(s)) x = Math.imul(x ^ c.charCodeAt(0), 16777619) >>> 0;
  return x;
}
function gradient(seed) {
  const x = hash(seed);
  const a = x % 360;
  const b = (a + 35 + ((x >>> 9) % 70)) % 360;
  return `linear-gradient(135deg, hsl(${a} 72% 52%), hsl(${b} 70% 34%))`;
}
function itemMark(name) {
  const t = String(name || '?').trim();
  const num = (t.match(/(\d+)\s*$/) || [])[1];
  return (num ? t[0].toUpperCase() + num : t.slice(0, 2).toUpperCase()).slice(0, 4);
}
function art(seed, name, cls = 'art') {
  const el = h('div', { class: cls, 'aria-hidden': 'true' }, itemMark(name));
  el.style.background = gradient(seed || name);
  return el;
}

function toast(text, kind = 'ok') {
  const t = h('div', { class: 'toast ' + kind }, icon(kind === 'err' ? 'alert' : 'check'), h('span', null, text));
  document.getElementById('toasts').append(t);
  setTimeout(() => t.remove(), kind === 'err' ? 5000 : 2600);
}
async function copy(text, what = 'Copied') {
  try {
    await navigator.clipboard.writeText(text);
    toast(what);
  } catch {
    toast('Copy failed: select the text instead', 'err');
  }
}

// Plain words for the errors people actually meet.
function human(msg) {
  const m = String(msg || 'Something went wrong');
  if (/wrong passphrase/i.test(m)) return 'Wrong passphrase.';
  if (/too recent for the anchor|wait a few blocks/i.test(m)) return 'Your newest funds are a few blocks too fresh to spend. Try again in a minute.';
  if (/demo faucet/i.test(m)) return m.charAt(0).toUpperCase() + m.slice(1) + '.';
  if (/insufficient|not enough|no spendable|InsufficientFunds/i.test(m)) return 'Not enough spendable ZEC. Money from another wallet can be spent after 10 confirmations.';
  if (/Connection (Failed|refused)|fetching http|could not be reached|timed out/i.test(m)) return 'Could not reach the shop. Check the address and that the shop is open.';
  if (/seller refused the offer/i.test(m)) return m.replace(/^.*?seller refused the offer:\s*/i, 'The seller refused the offer: ');
  if (/no answer from the seller/i.test(m)) return 'The seller did not answer in time. Your ZEC did not move; try again later.';
  if (/the wallet is locked/i.test(m)) return 'The wallet is locked. Unlock it first.';
  return m.length > 280 ? m.slice(0, 280) + '…' : m;
}

async function api(path, body) {
  const headers = { 'X-Zsettle': '1' };
  if (S.token) headers['X-Zsettle-Token'] = S.token;
  const opt = { method: body === undefined ? 'GET' : 'POST', headers, credentials: 'same-origin' };
  if (body !== undefined) {
    opt.headers['Content-Type'] = 'application/json';
    opt.body = JSON.stringify(body);
  }
  const r = await fetch(path, opt);
  if (r.status === 401) {
    if (S.hosted) {
      S.timers.forEach(clearInterval);
      document.getElementById('layer').replaceChildren();
      render(viewWelcomeHosted());
      throw new Error('Please sign in.');
    }
    sessionExpired();
    throw new Error('This session ended: open the new link from the terminal.');
  }
  let data;
  try {
    data = await r.json();
  } catch {
    data = { error: 'unexpected answer ' + r.status };
  }
  if (!r.ok) throw new Error(data.error || 'error ' + r.status);
  return data;
}

// The app makes a new session key each time it starts; a tab left open from before holds
// the old one. Say so, instead of failing action by action.
function sessionExpired() {
  if (S.expired) return;
  S.expired = true;
  S.timers.forEach(clearInterval);
  sessionStorage.removeItem('zsettle-token');
  document.getElementById('layer').replaceChildren();
  render(
    h(
      'main',
      { class: 'onboard' },
      h(
        'div',
        { class: 'panel' },
        h('div', { class: 'hero' }, logo(), h('h1', null, 'Open the new link'), h('p', { class: 'lede' }, 'This tab’s page key no longer works: the app was started with a new one. Open the link it printed in its terminal, here or in a new tab. Nothing in your wallet changed.')),
        linksRow(),
      ),
    ),
  );
}

async function follow(jobId, onStep) {
  for (;;) {
    await sleep(900);
    const j = await api('/api/jobs/' + jobId);
    if (onStep) onStep(j);
    if (j.state !== 'running') return j;
  }
}

// ------------------------------------------------------------ sheets

let lastFocus = null;
function sheet({ title, body, foot, dismissable = true, onClose }) {
  const layer = document.getElementById('layer');
  lastFocus = document.activeElement;
  const closeBtn = h('button', { class: 'iconbtn', 'aria-label': 'Close' }, icon('x'));
  const bodyEl = h('div', { class: 'stack' }, body);
  const footEl = h('div', { class: 'foot' }, foot || []);
  const titleEl = h('h2', { id: 'sheet-title' }, title);
  const box = h('div', { class: 'sheet', role: 'dialog', 'aria-modal': 'true', 'aria-labelledby': 'sheet-title' }, h('div', { class: 'grab' }), h('div', { class: 'head' }, titleEl, closeBtn), bodyEl, footEl);
  const scrim = h('div', { class: 'scrim' }, box);
  let canClose = dismissable;
  const api_ = {
    box,
    close() {
      scrim.remove();
      document.removeEventListener('keydown', onKey);
      if (onClose) onClose();
      if (lastFocus && lastFocus.focus) lastFocus.focus();
    },
    setTitle(t) {
      titleEl.textContent = t;
    },
    setBody(...n) {
      bodyEl.replaceChildren(...n.flat().filter(Boolean));
    },
    setFoot(...n) {
      footEl.replaceChildren(...n.flat().filter(Boolean));
      footEl.hidden = !footEl.children.length;
    },
    setDismissable(v) {
      canClose = v;
      closeBtn.hidden = !v;
    },
  };
  function onKey(e) {
    if (e.key === 'Escape' && canClose) api_.close();
    if (e.key === 'Tab') {
      const f = [...box.querySelectorAll('button, input, textarea, select, a[href]')].filter((x) => !x.disabled && !x.hidden && x.offsetParent !== null);
      if (!f.length) return;
      const first = f[0];
      const last = f[f.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        last.focus();
        e.preventDefault();
      } else if (!e.shiftKey && document.activeElement === last) {
        first.focus();
        e.preventDefault();
      }
    }
  }
  closeBtn.addEventListener('click', () => canClose && api_.close());
  scrim.addEventListener('click', (e) => e.target === scrim && canClose && api_.close());
  document.addEventListener('keydown', onKey);
  api_.setDismissable(dismissable);
  footEl.hidden = !footEl.children.length;
  layer.append(scrim);
  setTimeout(() => {
    const f = box.querySelector('input, textarea, .foot .btn.primary') || closeBtn;
    if (f) f.focus({ preventScroll: true });
  }, 30);
  return api_;
}

// Run a job inside a sheet: steps with a progress bar, then a result screen.
async function runInSheet(sh, path, body, { total = 3, success, failTitle = 'That did not work' }) {
  sh.setDismissable(false);
  sh.setFoot();
  const bar = h('div', { style: null });
  bar.style.width = '4%';
  const steps = h('ol', { class: 'steps' });
  sh.setBody(h('div', { class: 'progress' }, bar), steps, h('p', { class: 'small muted' }, 'You can keep this open; it takes from a few seconds to a minute.'));
  S.busy = true;
  try {
    const { job } = await api(path, body);
    const j = await follow(job, (j) => {
      const n = j.steps.length;
      bar.style.width = Math.min(100, Math.round(((n - (j.state === 'running' ? 0.5 : 0)) / total) * 100)) + '%';
      steps.replaceChildren(
        ...j.steps.map((s, i) => {
          let cls = 'done';
          if (i === n - 1 && j.state === 'running') cls = 'active';
          if (i === n - 1 && j.state === 'failed') cls = 'failed';
          return h('li', { class: cls }, h('span', { class: 'mark' }, cls === 'done' ? icon('check') : null), h('span', null, s));
        }),
      );
    });
    if (j.state === 'failed') throw new Error(j.error || 'failed');
    bar.style.width = '100%';
    const out = success(j.result);
    sh.setBody(h('div', { class: 'result' }, h('div', { class: 'big' }, icon('check')), h('h3', null, out.title), out.text ? h('p', null, out.text) : null), out.extra || null);
    sh.setFoot(out.actions || [h('button', { class: 'btn primary', onclick: () => sh.close() }, 'Done')]);
    return j.result;
  } catch (e) {
    sh.setBody(h('div', { class: 'result' }, h('div', { class: 'big bad' }, icon('alert')), h('h3', null, failTitle), h('p', null, human(e.message))));
    sh.setFoot(h('button', { class: 'btn', onclick: () => sh.close() }, 'Close'));
    return null;
  } finally {
    S.busy = false;
    sh.setDismissable(true);
  }
}

// ------------------------------------------------------------ passkeys

const b64u = (buf) =>
  btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const unb64u = (s) => {
  const p = s.replace(/-/g, '+').replace(/_/g, '/');
  return Uint8Array.from(atob(p + '==='.slice((p.length + 3) % 4)), (c) => c.charCodeAt(0));
};
const hexOf = (buf) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, '0')).join('');
const NO_PRF = 'This passkey cannot derive a key. Use a passphrase, or a passkey saved in Chrome, Edge, Safari or a recent Firefox.';
const passkeyUsable = () =>
  !!window.PublicKeyCredential && window.isSecureContext && (location.hostname === 'localhost' || location.protocol === 'https:');

// The passkey's PRF output, never stored, is the wallet's passphrase.
async function passkeyCreate() {
  const salt = S.hosted ? unb64u(S.hosted.salt) : crypto.getRandomValues(new Uint8Array(32));
  const cred = await navigator.credentials.create({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rp: { name: 'zsettle', id: location.hostname },
      user: { id: crypto.getRandomValues(new Uint8Array(16)), name: S.hosted ? 'zsettle demo wallet' : 'zsettle-' + S.status.profile, displayName: S.hosted ? 'zsettle demo wallet' : 'zsettle wallet (' + S.status.profile + ')' },
      pubKeyCredParams: [
        { type: 'public-key', alg: -7 },
        { type: 'public-key', alg: -257 },
      ],
      authenticatorSelection: { residentKey: S.hosted ? 'required' : 'preferred', requireResidentKey: !!S.hosted, userVerification: 'required' },
      extensions: { prf: { eval: { first: salt } } },
    },
  });
  const ext = cred.getClientExtensionResults().prf || {};
  const pk = { id: b64u(cred.rawId), salt: b64u(salt) };
  let secret = ext.results && ext.results.first;
  if (!secret) {
    if (ext.enabled === false) throw new Error(NO_PRF);
    secret = await passkeySecret(pk);
  }
  return { pk, passphrase: 'passkey:' + hexOf(secret) };
}
// Hosted sign-in: the browser offers this site's passkeys; the chosen one names the wallet.
async function passkeySignIn() {
  const a = await navigator.credentials.get({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rpId: location.hostname,
      userVerification: 'required',
      extensions: { prf: { eval: { first: unb64u(S.hosted.salt) } } },
    },
  });
  const r = a.getClientExtensionResults().prf;
  if (!r || !r.results || !r.results.first) throw new Error(NO_PRF);
  return { credential: b64u(a.rawId), passphrase: 'passkey:' + hexOf(r.results.first) };
}

async function passkeySecret(pk) {
  const a = await navigator.credentials.get({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rpId: location.hostname,
      allowCredentials: [{ type: 'public-key', id: unb64u(pk.id) }],
      userVerification: 'required',
      extensions: { prf: { eval: { first: unb64u(pk.salt) } } },
    },
  });
  const r = a.getClientExtensionResults().prf;
  if (!r || !r.results || !r.results.first) throw new Error(NO_PRF);
  return r.results.first;
}

// ------------------------------------------------------------ boot and routing

function render(node) {
  document.getElementById('app').replaceChildren(node);
}

function linksRow() {
  return h(
    'nav',
    { class: 'links', 'aria-label': 'Project links' },
    h('a', { href: LINKS.docs, target: '_blank', rel: 'noopener noreferrer' }, icon('book'), 'Docs'),
    h('a', { href: LINKS.github, target: '_blank', rel: 'noopener noreferrer' }, icon('github'), 'GitHub'),
    h('a', { href: LINKS.x, target: '_blank', rel: 'noopener noreferrer' }, icon('xlogo'), 'X'),
  );
}

async function boot() {
  try {
    const hello = await (await fetch('/api/hello', { headers: { 'X-Zsettle': '1' } })).json();
    if (hello.mode === 'hosted') {
      S.hosted = hello;
      if (!hello.signed_in) return render(viewWelcomeHosted());
      return refresh();
    }
  } catch {
    /* an old app without /api/hello: local mode */
  }
  const m = location.hash.match(/token=([0-9a-f]{32})/);
  if (m) {
    sessionStorage.setItem('zsettle-token', m[1]);
    history.replaceState(null, '', location.pathname);
  }
  S.token = sessionStorage.getItem('zsettle-token');
  if (!S.token) {
    return render(
      h('div', { class: 'onboard' }, h('div', { class: 'panel card stack' }, h('div', { class: 'hero' }, logo(), h('h1', null, 'Open the link from the terminal'), h('p', { class: 'lede' }, 'zsettle-app printed a link with a key for this session. Open that link to continue.')), linksRow())),
    );
  }
  await refresh();
}

async function refresh() {
  try {
    S.status = await api('/api/status');
  } catch (e) {
    return render(h('div', { class: 'onboard' }, h('div', { class: 'panel card' }, h('p', { class: 'err' }, human(e.message)))));
  }
  if (S.hosted && !S.status.unlocked) return render(viewWelcomeHosted());
  if (!S.status.wallet) return render(viewCreate());
  if (!S.status.unlocked) return render(viewUnlock());
  renderShell();
  loadView();
  startTimers();
}

function startTimers() {
  S.timers.forEach(clearInterval);
  S.timers = [setInterval(backgroundSync, SYNC_EVERY_MS), setInterval(pollShop, 3000)];
  backgroundSync();
}

async function backgroundSync() {
  if (S.syncing || S.busy || document.hidden || !S.status || !S.status.unlocked) return;
  S.syncing = true;
  updateSyncChip();
  try {
    const { job } = await api('/api/sync', {});
    await follow(job);
    S.status = await api('/api/status');
    if (!document.querySelector('#layer .scrim') && !isTyping()) loadView(true);
  } catch {
    /* the next tick tries again */
  } finally {
    S.syncing = false;
    updateSyncChip();
  }
}
const isTyping = () => document.activeElement && /INPUT|TEXTAREA/.test(document.activeElement.tagName);

// ------------------------------------------------------------ create and unlock

function viewCreate() {
  let network = 'test';
  const err = h('p', { class: 'small err', role: 'alert' });
  const seg = h(
    'div',
    { class: 'segment', role: 'group', 'aria-label': 'Network' },
    ['test', 'main'].map((n) =>
      h('button', { type: 'button', 'aria-pressed': String(n === network), onclick: (e) => { network = n; seg.querySelectorAll('button').forEach((b) => b.setAttribute('aria-pressed', String(b === e.currentTarget))); } }, n === 'test' ? 'Testnet · free coins' : 'Mainnet · real ZEC'),
    ),
  );
  const p1 = h('input', { class: 'input', type: 'password', autocomplete: 'new-password', placeholder: 'At least 10 characters', id: 'p1' });
  const p2 = h('input', { class: 'input', type: 'password', autocomplete: 'new-password', placeholder: 'Repeat it', id: 'p2' });
  const passBox = h('div', { class: 'stack', hidden: true }, h('div', { class: 'field' }, h('label', { for: 'p1' }, 'Passphrase'), p1), h('div', { class: 'field' }, h('label', { for: 'p2' }, 'Repeat passphrase'), p2), h('button', { class: 'btn block', onclick: () => create(false) }, 'Create with passphrase'));
  const pkBtn = h('button', { class: 'btn primary lg block', onclick: () => create(true), disabled: !passkeyUsable() }, icon('finger'), 'Create wallet with a passkey');

  async function create(viaPasskey) {
    err.textContent = '';
    pkBtn.disabled = true;
    try {
      let body = { network };
      if (viaPasskey) {
        const { pk, passphrase } = await passkeyCreate();
        body = { ...body, passphrase, passkey: pk };
      } else {
        if (p1.value.length < 10) throw new Error('Use at least 10 characters.');
        if (p1.value !== p2.value) throw new Error('The passphrases differ.');
        body.passphrase = p1.value;
      }
      const r = await api('/api/wallet/create', body);
      S.view = 'wallet';
      await refresh();
      openReceive(r.fund, true);
    } catch (e) {
      err.textContent = e.name === 'NotAllowedError' ? 'The passkey prompt was cancelled.' : human(e.message);
    } finally {
      pkBtn.disabled = !passkeyUsable();
    }
  }

  return h(
    'main',
    { class: 'onboard' },
    h(
      'div',
      { class: 'panel' },
      h('div', { class: 'hero' }, logo(), h('h1', null, 'Trade privately on Zcash'), h('p', { class: 'lede' }, 'Buy and sell items for ZEC in one shielded transaction. No escrow, no one in the middle.')),
      h(
        'div',
        { class: 'card stack lg' },
        h(
          'div',
          { class: 'points' },
          h('div', { class: 'p' }, icon('shield'), h('span', null, 'Your wallet stays on this computer.')),
          h('div', { class: 'p' }, icon('finger'), h('span', null, 'Unlock it with your fingerprint, face or device PIN.')),
          h('div', { class: 'p' }, icon('check'), h('span', null, 'Item and payment move together, or not at all.')),
        ),
        h('div', { class: 'field' }, h('label', null, 'Network'), seg),
        pkBtn,
        passkeyUsable() ? null : h('p', { class: 'small muted' }, 'Passkeys need this page opened at http://localhost.'),
        h('button', { class: 'linkbtn', onclick: () => (passBox.hidden = !passBox.hidden) }, 'Use a passphrase instead'),
        passBox,
        err,
        h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'The passkey is the only key to this wallet. If you lose it, the funds are gone, so keep amounts small.')),
      ),
      linksRow(),
    ),
  );
}

function viewWelcomeHosted() {
  const err = h('p', { class: 'small err', role: 'alert' });
  const busy = (b) => document.querySelectorAll('.onboard .btn').forEach((x) => (x.disabled = b));
  async function act(fn) {
    err.textContent = '';
    busy(true);
    try {
      await fn();
    } catch (e) {
      err.textContent = e.name === 'NotAllowedError' ? 'The passkey prompt was cancelled.' : human(e.message);
    } finally {
      busy(false);
    }
  }
  async function done() {
    S.hosted.signed_in = true;
    S.view = 'wallet';
    await refresh();
  }
  const createPk = () => act(async () => {
    const { pk, passphrase } = await passkeyCreate();
    await api('/api/wallet/create', { passphrase, passkey: pk });
    await done();
  });
  const signInPk = () => act(async () => {
    await api('/api/unlock', await passkeySignIn());
    await done();
  });
  const code = h('input', { class: 'input', id: 'code', placeholder: 'Wallet code', autocomplete: 'username', autocapitalize: 'off', spellcheck: 'false' });
  const pass = h('input', { class: 'input', id: 'pw', type: 'password', placeholder: 'Passphrase (12 characters or more)', autocomplete: 'current-password' });
  const signInPw = () => act(async () => {
    await api('/api/unlock', { code: code.value.trim(), passphrase: pass.value });
    await done();
  });
  const createPw = () => act(async () => {
    if (pass.value.length < 12) throw new Error('Use a passphrase of at least 12 characters.');
    const r = await api('/api/wallet/create', { passphrase: pass.value });
    await done();
    showCode(r.code);
  });
  const pwBox = h(
    'div',
    { class: 'stack', hidden: true },
    h('div', { class: 'field' }, h('label', { for: 'code' }, 'Wallet code (to sign in)'), code),
    h('div', { class: 'field' }, h('label', { for: 'pw' }, 'Passphrase'), pass),
    h('div', { class: 'row' }, h('button', { class: 'btn', onclick: signInPw }, 'Sign in'), h('button', { class: 'btn', onclick: createPw }, 'New wallet with this passphrase')),
  );
  return h(
    'main',
    { class: 'onboard' },
    h(
      'div',
      { class: 'panel' },
      h('div', { class: 'hero' }, logo(), h('h1', null, 'Trade privately on Zcash'), h('p', { class: 'lede' }, 'Buy and sell items for ZEC in one shielded transaction. No escrow, no one in the middle.')),
      h(
        'div',
        { class: 'card stack lg' },
        h('div', { class: 'callout info' }, icon('info'), h('span', null, 'This is a public demo on the Zcash testnet: the coins are free and have no value. The demo server keeps your wallet; for real money, run zsettle on your own computer.')),
        h('button', { class: 'btn primary lg block', onclick: createPk, disabled: !passkeyUsable() }, icon('finger'), 'Create a wallet with a passkey'),
        h('button', { class: 'btn lg block', onclick: signInPk, disabled: !passkeyUsable() }, icon('lock'), 'Sign in with your passkey'),
        h('button', { class: 'linkbtn', onclick: () => (pwBox.hidden = !pwBox.hidden) }, 'No passkey? Use a wallet code and passphrase'),
        pwBox,
        err,
      ),
      howItWorks(),
      linksRow(),
    ),
  );
}

/** One trade in three steps, for a first visit. */
function howItWorks() {
  const steps = [
    ['Get a wallet and coins', 'A passkey makes a shielded wallet; the demo gives free test coins.'],
    ['Pick an item', 'Shops list real shielded notes, signed by their seller.'],
    ['Buy in one transaction', 'The item and the ZEC change hands together, or not at all. The seller never sees your address or balance.'],
  ];
  return h(
    'section',
    { class: 'how', 'aria-label': 'How a trade works' },
    steps.map(([t, d], i) => h('div', { class: 'how-step' }, h('span', { class: 'how-n', 'aria-hidden': 'true' }, String(i + 1)), h('div', null, h('div', { class: 'how-t' }, t), h('div', { class: 'how-d' }, d)))),
  );
}

function showCode(code) {
  const sh = sheet({
    title: 'Your wallet code',
    body: [h('p', { class: 'muted' }, 'You sign in with this code and your passphrase. Write both down: nobody can recover them.'), h('div', { class: 'addr mono' }, code)],
    foot: [h('button', { class: 'btn primary', onclick: () => copy(code, 'Code copied') }, icon('copy'), 'Copy code')],
  });
  sh.box.querySelector('.addr').style.fontSize = '22px';
}

function viewUnlock() {
  const err = h('p', { class: 'small err', role: 'alert' });
  const pk = S.status.passkey;
  const pass = h('input', { class: 'input', type: 'password', autocomplete: 'current-password', placeholder: 'Passphrase', 'aria-label': 'Passphrase' });
  const btn = h('button', { class: 'btn primary lg block', onclick: () => unlock(!!pk) }, pk ? [icon('finger'), 'Unlock with passkey'] : 'Unlock');
  async function unlock(viaPasskey) {
    err.textContent = '';
    btn.disabled = true;
    try {
      const passphrase = viaPasskey ? 'passkey:' + hexOf(await passkeySecret(pk)) : pass.value;
      await api('/api/unlock', { passphrase });
      await refresh();
    } catch (e) {
      err.textContent = e.name === 'NotAllowedError' ? 'The passkey prompt was cancelled.' : human(e.message);
    } finally {
      btn.disabled = false;
    }
  }
  pass.addEventListener('keydown', (e) => e.key === 'Enter' && unlock(false));
  return h(
    'main',
    { class: 'onboard' },
    h(
      'div',
      { class: 'panel' },
      h('div', { class: 'hero' }, logo(), h('h1', null, 'Welcome back'), h('p', { class: 'lede' }, 'Wallet ', h('b', null, S.status.profile), ' · ', S.status.network === 'main' ? 'Mainnet' : 'Testnet')),
      h('div', { class: 'card stack' }, pk ? null : pass, btn, pk && !passkeyUsable() ? h('p', { class: 'small muted' }, 'Open this page at http://localhost to use the passkey.') : null, err),
      linksRow(),
    ),
  );
}

// ------------------------------------------------------------ shell

const VIEWS = [
  ['wallet', 'Wallet', 'wallet'],
  ['market', 'Market', 'store'],
  ['sell', 'Sell', 'tag'],
  ['activity', 'Activity', 'clock'],
];

function syncChip() {
  const st = S.status || {};
  const at = st.last_sync && st.last_sync.at;
  const label = S.syncing ? 'Syncing…' : at ? 'Synced ' + ago(at) : 'Not synced';
  return h('button', { class: 'chip sync ' + (S.syncing ? 'info' : at ? 'ok' : 'warn'), id: 'syncchip', onclick: () => backgroundSync(), title: label + ' (tap to sync now)', 'aria-label': label }, S.syncing ? h('span', { class: 'spinner sm' }) : h('span', { class: 'dot' }), h('span', { class: 'lbl' }, label));
}
function updateSyncChip() {
  const old = document.getElementById('syncchip');
  if (old) old.replaceWith(syncChip());
}

function renderShell() {
  const st = S.status;
  const netChip = h('span', { class: 'chip ' + (st.network === 'main' ? 'accent' : 'info') }, st.network === 'main' ? 'Mainnet' : 'Testnet');
  const nav = (cls) =>
    VIEWS.map(([k, label, ic]) => h('button', { class: cls, 'aria-current': S.view === k ? 'page' : null, onclick: () => go(k) }, icon(ic), h('span', null, label)));
  const sidebar = h(
    'aside',
    { class: 'sidebar', 'aria-label': 'Main' },
    h('a', { class: 'brand', href: '#', onclick: (e) => { e.preventDefault(); go('wallet'); } }, logo(), h('span', { class: 'wordmark' }, 'z', h('span', null, 'settle'))),
    h('nav', { class: 'stack', style: null }, nav('sidenav')),
    h(
      'div',
      { class: 'sidefoot' },
      h('a', { href: LINKS.docs, target: '_blank', rel: 'noopener noreferrer' }, icon('book'), 'Docs'),
      h('a', { href: LINKS.github, target: '_blank', rel: 'noopener noreferrer' }, icon('github'), 'GitHub'),
      h('a', { href: LINKS.x, target: '_blank', rel: 'noopener noreferrer' }, icon('xlogo'), 'Follow on X'),
      h('a', { href: '#', onclick: (e) => { e.preventDefault(); openBackup(); } }, icon('shield'), 'Backup words'),
      h('div', { class: 'who' }, h('span', null, st.hosted ? 'Code ' + st.code : st.profile), netChip.cloneNode(true)),
      h('a', { href: '#', onclick: (e) => { e.preventDefault(); lock(); } }, icon('lock'), st.hosted ? 'Sign out' : 'Lock wallet'),
    ),
  );
  const top = h(
    'header',
    { class: 'topbar' },
    h('a', { class: 'brand', href: '#', onclick: (e) => { e.preventDefault(); go('wallet'); } }, logo(), h('span', { class: 'wordmark' }, 'z', h('span', null, 'settle'))),
    h('span', { class: 'spacer' }),
    netChip,
    syncChip(),
    h('button', { class: 'iconbtn menu', 'aria-label': 'Menu', onclick: openMenu }, icon('menu')),
  );
  const bottom = h('nav', { class: 'bottomnav', 'aria-label': 'Main' }, nav('navbtn'));
  render(h('div', { class: 'shell' }, sidebar, h('div', { class: 'main' }, top, h('main', { class: 'content', id: 'content' }, skeleton())), bottom));
}

function openMenu() {
  const a = (href, ic, text) => h('a', { class: 'li', href, target: '_blank', rel: 'noopener noreferrer', style: null }, h('span', { class: 'ico' }, icon(ic)), h('span', { class: 'grow title' }, text), icon('external'));
  const sh = sheet({
    title: 'zsettle',
    body: [
      h('div', { class: 'list' }, a(LINKS.docs, 'book', 'Docs'), a(LINKS.github, 'github', 'Source on GitHub'), a(LINKS.x, 'xlogo', 'Follow on X')),
      h('p', { class: 'small muted' }, (S.status.hosted ? 'Wallet code ' + S.status.code : 'Wallet ' + S.status.profile) + ' · ' + (S.status.network === 'main' ? 'Mainnet' : 'Testnet')),
    ],
    foot: [h('button', { class: 'btn', onclick: () => { sh.close(); openBackup(); } }, icon('shield'), 'Backup words'), h('button', { class: 'btn', onclick: () => { sh.close(); lock(); } }, icon('lock'), S.hosted ? 'Sign out' : 'Lock wallet')],
  });
  sh.box.querySelectorAll('a.li').forEach((x) => (x.style.textDecoration = 'none'));
}

async function openBackup() {
  const pk = S.status.passkey;
  const pass = h('input', { class: 'input', type: 'password', placeholder: 'Your passphrase', autocomplete: 'current-password', 'aria-label': 'Passphrase' });
  const err = h('p', { class: 'small err', role: 'alert' });
  const sh = sheet({
    title: 'Backup words',
    body: [
      h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'Anyone who sees these 24 words controls this wallet. Write them on paper, out of sight of cameras, and never type them into a website.')),
      pk ? h('p', { class: 'small muted' }, 'Confirm with your passkey to show them.') : pass,
      err,
    ],
    foot: [h('button', { class: 'btn primary', onclick: () => show() }, pk ? [icon('finger'), 'Confirm and show'] : 'Show words')],
  });
  async function show() {
    err.textContent = '';
    try {
      const passphrase = pk ? 'passkey:' + hexOf(await passkeySecret(pk)) : pass.value;
      const r = await api('/api/backup', { passphrase });
      const grid = h('ol', { class: 'words' }, r.words.map((w) => h('li', null, w)));
      sh.setBody(h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'Write these down in order. Close this when you are done.')), grid);
      sh.setFoot(h('button', { class: 'btn primary', onclick: () => sh.close() }, 'I wrote them down'));
    } catch (e) {
      err.textContent = e.name === 'NotAllowedError' ? 'The passkey prompt was cancelled.' : human(e.message);
    }
  }
}

function go(view) {
  S.view = view;
  document.querySelectorAll('[aria-current="page"]').forEach((b) => b.removeAttribute('aria-current'));
  document.querySelectorAll('.sidenav, .navbtn').forEach((b, i) => {
    if (VIEWS[i % 4][0] === view) b.setAttribute('aria-current', 'page');
  });
  const c = document.getElementById('content');
  if (c) c.replaceChildren(skeleton());
  window.scrollTo({ top: 0 });
  loadView();
}

async function lock() {
  // A hosted shop keeps selling after its owner signs out; say so first.
  if (S.hosted && S.status && S.status.shop && S.status.shop.running &&
      !confirm('Your shop stays open while you are away: it keeps selling and signing offers. To close it, stop it on the Sell tab first. Sign out anyway?')) return;
  try {
    await api('/api/lock', {});
  } catch {
    /* already signed out */
  }
  S.timers.forEach(clearInterval);
  S.wallet = S.market = S.shop = S.activity = null;
  if (S.hosted) {
    S.hosted.signed_in = false;
    return render(viewWelcomeHosted());
  }
  await refresh();
}

function skeleton() {
  const w = h('div', { class: 'stack lg', 'aria-busy': 'true' });
  for (const ht of [170, 24, 200]) {
    const d = h('div', { class: 'sk' });
    d.style.height = ht + 'px';
    w.append(d);
  }
  return w;
}

async function loadView(quiet) {
  const view = S.view;
  try {
    if (view === 'wallet') {
      const [w, a] = await Promise.all([api('/api/wallet'), api('/api/activity')]);
      S.wallet = w;
      S.activity = a;
    } else if (view === 'sell') {
      const [w, s] = await Promise.all([api('/api/wallet'), api('/api/shop')]);
      S.wallet = w;
      S.shop = s;
    } else if (view === 'activity') {
      S.activity = await api('/api/activity');
    } else if (view === 'market' && S.hosted) {
      S.shops = await api('/api/shops');
      if (!S.wallet) S.wallet = await api('/api/wallet');
    }
  } catch (e) {
    if (!quiet) toast(human(e.message), 'err');
  }
  if (S.view !== view) return;
  const c = document.getElementById('content');
  if (!c) return;
  // A background refresh redraws only when something changed, so a tap never lands on a
  // screen that is being replaced.
  const key = JSON.stringify([view, S.wallet, S.shop && S.shop.running, S.activity, S.market, S.status && S.status.sellers, S.shops]);
  if (quiet && key === S.lastKey && c.firstChild && !c.querySelector('[aria-busy="true"]')) {
    updateSyncChip();
    return;
  }
  S.lastKey = key;
  const page = { wallet: viewWallet, market: viewMarket, sell: viewSell, activity: viewActivity }[view]();
  c.replaceChildren(page);
  updateSyncChip();
}

// ------------------------------------------------------------ wallet

function viewWallet() {
  const w = S.wallet || { balance_zat: 0, notes: [], items: [] };
  const incoming = w.notes.filter((n) => !n.mined).reduce((a, n) => a + n.value_zat, 0);
  const empty = !w.notes.length && !w.items.length;
  return h(
    'div',
    { class: 'stack lg' },
    S.hosted ? h('div', { class: 'callout info' }, icon('info'), h('span', null, 'Public demo on the Zcash testnet. Wallet code ', h('b', null, S.status.code || ''), '. Coins here are free and worthless.')) : null,
    S.status.faucet ? h('div', { class: 'card row nowrap' }, h('span', { class: 'ico' }, icon('spark')), h('div', { class: 'grow' }, h('b', null, 'Free test coins'), h('div', { class: 'small muted' }, 'Get 0.003 test ZEC to try buying and selling.')), h('button', { class: 'btn primary', onclick: getCoins }, 'Get coins')) : null,
    h(
      'section',
      { class: 'balance-card', 'aria-label': 'Balance' },
      h('div', { class: 'label' }, 'Balance'),
      h('div', { class: 'amount' }, zec(w.balance_zat, { bare: true }), h('small', null, 'ZEC')),
      h('div', { class: 'small muted' }, incoming ? zec(incoming) + ' on its way' : w.notes.length ? w.notes.length + ' note' + (w.notes.length > 1 ? 's' : '') + ' · shielded' : 'Shielded'),
      h(
        'div',
        { class: 'actions' },
        h('button', { class: 'btn primary lg', onclick: () => openReceive() }, icon('down'), 'Receive'),
        h('button', { class: 'btn lg', onclick: openSend, disabled: !w.balance_zat }, icon('up'), 'Send'),
      ),
    ),
    empty
      ? h(
          'div',
          { class: 'card' },
          h(
            'div',
            { class: 'empty' },
            h('div', { class: 'ico' }, icon('wallet')),
            h('h3', null, 'Fund your wallet'),
            h('p', null, S.status.network === 'test' ? 'Press Receive, then paste the address into a Zcash testnet faucet. Coins can be spent after 10 confirmations, a few minutes.' : 'Press Receive and send ZEC from any shielded Zcash wallet. Coins can be spent after 10 confirmations, about 13 minutes.'),
            h('button', { class: 'btn primary', onclick: () => openReceive() }, icon('down'), 'Show my address'),
          ),
        )
      : null,
    h('div', null, h('div', { class: 'section-title' }, 'My items', h('span', { class: 'spacer' }), w.items.length ? h('span', { class: 'chip' }, String(w.items.length)) : null), itemsGrid(w.items, (it) => openOwnItem(it), 'No items yet. Buy one in the Market, or create your own in Sell.')),
    S.activity && S.activity.length
      ? h('div', null, h('div', { class: 'section-title' }, 'Recent activity', h('span', { class: 'spacer' }), h('button', { class: 'linkbtn', onclick: () => go('activity') }, 'See all')), h('div', { class: 'card', style: null }, activityList(S.activity.slice(0, 4))))
      : null,
  );
}

async function getCoins() {
  const sh = sheet({ title: 'Free test coins', body: [] });
  await runInSheet(sh, '/api/faucet', {}, { total: 1, success: (r) => ({ title: 'On their way', text: zec(r.amount_zat) + ' arrives in about a minute and can be spent after 10 confirmations, a few minutes later.', extra: txRow(r.txid) }), failTitle: 'No coins this time' });
  S.status = await api('/api/status');
  loadView(true);
}

function itemsGrid(items, onOpen, emptyText) {
  if (!items || !items.length) return h('div', { class: 'card' }, h('div', { class: 'empty' }, h('div', { class: 'ico' }, icon('spark')), h('p', null, emptyText)));
  return h(
    'div',
    { class: 'items' },
    items.map((it) =>
      h(
        'button',
        { class: 'item', onclick: () => onOpen(it), 'aria-label': (it.name || 'Item') + (it.price_zat ? ', ' + zec(it.price_zat) : '') },
        art(it.genesis || it.note, it.name),
        h(
          'div',
          { class: 'body' },
          h('div', { class: 'name' }, it.name || 'Unnamed item'),
          it.price_zat ? h('div', { class: 'price' }, zec(it.price_zat)) : it.listed_zat ? h('div', { class: 'meta' }, 'Listed · ' + zec(it.listed_zat)) : h('div', { class: 'meta' }, it.mined ? 'In your wallet' : 'Arriving…'),
        ),
      ),
    ),
  );
}

function openOwnItem(it) {
  const sh = sheet({
    title: it.name || 'Item',
    body: [
      art(it.note, it.name, 'art big'),
      h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Status'), h('span', null, it.listed_zat ? 'Listed for ' + zec(it.listed_zat) : it.mined ? 'In your wallet' : 'Arriving')), h('div', { class: 'r' }, h('span', null, 'Note'), h('span', { class: 'mono' }, short(it.note, 10)))),
    ],
    foot: [h('button', { class: 'btn', onclick: () => { sh.close(); openHistory(null, it.note, it.name); } }, icon('history'), 'History'), h('button', { class: 'btn primary', onclick: () => { sh.close(); go('sell'); } }, icon('tag'), 'Sell')],
  });
}

async function openReceive(given, first) {
  const sh = sheet({ title: first ? 'Your wallet is ready' : 'Receive ZEC', body: [h('div', { class: 'sk' })] });
  let addr = given;
  try {
    if (!addr) addr = (await api('/api/address', {})).address;
  } catch (e) {
    sh.setBody(h('p', { class: 'err' }, human(e.message)));
    return;
  }
  const qr = h('div', { class: 'qr' });
  try {
    qr.append(QR.svg(addr));
  } catch {
    qr.remove();
  }
  sh.setBody(
    first ? h('p', { class: 'muted' }, 'Fund it by sending ZEC to this address.') : h('p', { class: 'muted' }, 'A fresh address each time: none of them can be linked to the others.'),
    qr,
    h('div', { class: 'addr mono' }, addr),
    h('div', { class: 'callout info' }, icon('info'), h('span', null, S.status.network === 'test' ? 'Testnet coins are free: paste this address into a Zcash testnet faucet.' : 'Only shielded payments reach this address. Coins can be spent after 10 confirmations.')),
  );
  sh.setFoot(
    navigator.share ? h('button', { class: 'btn', onclick: () => navigator.share({ text: addr }).catch(() => {}) }, icon('share'), 'Share') : null,
    h('button', { class: 'btn primary', onclick: () => copy(addr, 'Address copied') }, icon('copy'), 'Copy address'),
  );
}

function openSend() {
  const bal = (S.wallet && S.wallet.balance_zat) || 0;
  const prefix = S.status.network === 'main' ? 'u1' : 'utest1';
  const to = h('input', { class: 'input', id: 'to', placeholder: prefix + '…', autocomplete: 'off', autocapitalize: 'off', spellcheck: 'false' });
  const amount = h('input', { class: 'input', id: 'amt', inputmode: 'decimal', placeholder: '0.001', autocomplete: 'off' });
  const memo = h('input', { class: 'input', id: 'memo', placeholder: 'Only the recipient can read it', maxlength: '500' });
  const err = h('p', { class: 'small err', role: 'alert' });
  const paste = h('button', { class: 'btn sm action', type: 'button', onclick: async () => { try { to.value = (await navigator.clipboard.readText()).trim(); } catch { toast('Paste with your keyboard instead', 'err'); } } }, icon('paste'), 'Paste');
  const sh = sheet({
    title: 'Send ZEC',
    body: [
      h('div', { class: 'field' }, h('label', { for: 'to' }, 'To'), h('div', { class: 'inputwrap' }, to, paste)),
      h('div', { class: 'field' }, h('label', { for: 'amt' }, 'Amount'), h('div', { class: 'inputwrap' }, amount, h('span', { class: 'suffix' }, 'ZEC')), h('div', { class: 'hint' }, 'Available ' + zec(bal) + '. The network fee is about ' + zec(FEE_ZAT) + '.')),
      h('div', { class: 'field' }, h('label', { for: 'memo' }, 'Message (optional)'), memo),
      err,
    ],
    foot: [h('button', { class: 'btn primary lg', onclick: review }, 'Review')],
  });
  function review() {
    err.textContent = '';
    const a = to.value.trim();
    const amt = amount.value.trim().replace(',', '.');
    const zat = Math.round(Number(amt) * 1e8);
    if (!a.startsWith(prefix)) return (err.textContent = 'This is not a ' + (S.status.network === 'main' ? 'mainnet' : 'testnet') + ' Zcash address. It should start with ' + prefix + '.');
    if (!/^\d*\.?\d{0,8}$/.test(amt) || !(zat > 0)) return (err.textContent = 'Enter an amount in ZEC, for example 0.001.');
    if (zat + FEE_ZAT > bal) return (err.textContent = 'Not enough ZEC for this amount plus the network fee.');
    sh.setTitle('Review');
    sh.setBody(
      h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'To'), h('span', { class: 'mono' }, short(a, 10))), h('div', { class: 'r' }, h('span', null, 'Amount'), h('span', null, zec(zat))), h('div', { class: 'r' }, h('span', null, 'Network fee'), h('span', null, 'about ' + zec(FEE_ZAT))), memo.value.trim() ? h('div', { class: 'r' }, h('span', null, 'Message'), h('span', null, memo.value.trim())) : null, h('div', { class: 'r total' }, h('span', null, 'Total'), h('span', null, 'about ' + zec(zat + FEE_ZAT)))),
      h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'Check the address. A payment cannot be undone.')),
    );
    sh.setFoot(
      h('button', { class: 'btn', onclick: () => { sh.close(); openSend(); } }, 'Back'),
      h('button', {
        class: 'btn primary',
        onclick: async () => {
          sh.setTitle('Sending');
          await runInSheet(sh, '/api/pay', { to: a, amount: amt + ' zec', memo: memo.value.trim() }, { total: 2, success: (r) => ({ title: 'Sent', text: zec(zat) + ' is on its way. It arrives in about a minute.', extra: txRow(r.txid) }), failTitle: 'Not sent' });
          loadView(true);
        },
      }, 'Confirm and send'),
    );
  }
}

function txLink(txid, n = 6) {
  const base = EXPLORER[(S.status && S.status.network) || 'main'];
  return h(
    'span',
    { class: 'txlink' },
    h('a', { href: base + txid, target: '_blank', rel: 'noopener noreferrer', title: 'Open in the block explorer' }, short(txid, n), icon('external')),
    h('button', { class: 'iconbtn sm', 'aria-label': 'Copy transaction id', onclick: () => copy(txid, 'Transaction id copied') }, icon('copy')),
  );
}

function txRow(txid) {
  if (!txid) return null;
  return h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Transaction'), txLink(txid, 8)));
}

// ------------------------------------------------------------ market

function viewMarket() {
  const input = h('input', { class: 'input', id: 'shopurl', type: 'url', inputmode: 'url', placeholder: S.hosted ? location.origin + '/shop/…' : 'http://127.0.0.1:8900', autocomplete: 'off', autocapitalize: 'off', spellcheck: 'false', value: S.market ? S.market.base : '' });
  const err = h('p', { class: 'small err', role: 'alert' });
  const openBtn = h('button', { class: 'btn primary sm action', onclick: () => open() }, 'Open');
  async function open(u) {
    err.textContent = '';
    const url = (u || input.value).trim();
    if (!url) return (err.textContent = 'Enter the address of a shop.');
    openBtn.disabled = true;
    try {
      S.market = await api('/api/market', { url: /^https?:\/\//.test(url) ? url : 'http://' + url });
      S.status = await api('/api/status');
      loadView(true);
    } catch (e) {
      err.textContent = human(e.message);
    } finally {
      openBtn.disabled = false;
    }
  }
  input.addEventListener('keydown', (e) => e.key === 'Enter' && open());
  const openShops = (S.shops || []).filter((s) => !S.market || s.base !== S.market.base);
  // Shops already in the open list are not repeated under recent ones.
  const recent = (S.status.sellers || []).filter((s) => (!S.market || s !== S.market.base) && !(S.hosted && openShops.some((o) => o.base === s)));
  return h(
    'div',
    null,
    h('h2', { class: 'page' }, 'Market'),
    h('p', { class: 'pagesub' }, S.hosted ? 'Pick a shop, or open one by its address. The seller’s signature is checked before anything is shown.' : 'Open a shop by its address. The seller’s signature is checked before anything is shown.'),
    // On the demo the open shops come first: that is where a new visitor starts.
    S.hosted && openShops.length
      ? h('div', null, h('div', { class: 'section-title' }, 'Open shops on this demo'), h('div', { class: 'card' }, h('div', { class: 'list' }, openShops.map((s) => h('button', { class: 'li', onclick: () => open(s.base) }, shopAvatar(s.base), h('span', { class: 'grow' }, h('div', { class: 'title' }, s.name + (s.base === S.status.public_shop ? ' (yours)' : '')), h('div', { class: 'sub' }, s.items + ' item' + (s.items === 1 ? '' : 's') + ' for sale')), icon('chevron'))))))
      : null,
    S.hosted && openShops.length ? h('div', { class: 'section-title' }, 'Or open a shop by its address') : null,
    h('div', { class: 'card stack' }, h('div', { class: 'field' }, h('label', { for: 'shopurl' }, 'Shop address'), h('div', { class: 'inputwrap' }, input, openBtn)), err, recent.length ? h('div', null, h('div', { class: 'small muted', style: null }, 'Recent shops'), h('div', { class: 'list' }, recent.slice(0, 4).map((s) => h('button', { class: 'li', onclick: () => open(s) }, avatar(s, 'ico'), h('span', { class: 'grow' }, h('div', { class: 'title' }, s.replace(/^https?:\/\//, '')), h('div', { class: 'sub' }, 'Opened before')), icon('chevron'))))) : null),
    S.market ? viewShopFront() : null,
  );
}

function avatar(seed, cls = 'avatar') {
  const el = h('div', { class: cls, 'aria-hidden': 'true' }, String(seed).replace(/^https?:\/\//, '').replace(/[^a-z0-9]/gi, '').slice(0, 1).toUpperCase() || 'S');
  el.style.background = gradient(seed);
  el.style.color = '#fff';
  el.style.fontWeight = '800';
  return el;
}

function shopAvatar(seed) {
  const el = h('div', { class: 'avatar', 'aria-hidden': 'true' }, icon('store'));
  el.style.background = gradient(seed);
  el.style.color = '#fff';
  return el;
}

function viewShopFront() {
  const m = S.market;
  const wrongNet = m.network !== S.status.network;
  return h(
    'div',
    null,
    h('div', { class: 'section-title' }, 'Shop'),
    h(
      'div',
      { class: 'card stack' },
      h('div', { class: 'shophead' }, shopAvatar(m.fingerprint), h('div', { style: null, class: 'grow' }, h('div', { style: null }, h('b', null, ((S.shops || []).find((x) => x.base === m.base) || {}).name || m.base.replace(/^https?:\/\//, ''))), h('div', { class: 'row', style: null }, h('span', { class: 'chip ok' }, icon('shield'), 'Signed by the seller'), h('span', { class: 'chip' }, m.items.length + ' for sale'), wrongNet ? h('span', { class: 'chip err' }, 'Other network') : null))),
      h('div', { class: 'callout info' }, icon('info'), h('span', null, 'When you buy, the seller sees the price and the time of the sale, never your address or your balance.')),
    ),
    h('div', { class: 'section-title' }, 'For sale'),
    itemsGrid(m.items.map((i) => ({ ...i, mined: true })), (it) => openShopItem(it, wrongNet), 'Nothing for sale right now. Come back later.'),
  );
}

function openShopItem(it, wrongNet) {
  const sh = sheet({
    title: it.name || 'Item',
    body: [
      art(it.note, it.name, 'art big'),
      h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Price'), h('b', null, zec(it.price_zat))), h('div', { class: 'r' }, h('span', null, 'Seller'), h('span', null, S.market.base.replace(/^https?:\/\//, ''))), h('div', { class: 'r' }, h('span', null, 'Item note'), h('span', { class: 'mono' }, short(it.note, 8)))),
      wrongNet ? h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'This shop is on another network than your wallet.')) : null,
    ],
    foot: [h('button', { class: 'btn', onclick: () => { sh.close(); openHistory(S.market.base, it.note, it.name); } }, icon('history'), 'History'), S.market.own ? h('span', { class: 'chip' }, 'Your own shop') : h('button', { class: 'btn primary', disabled: wrongNet, onclick: () => buyReview(sh, it) }, icon('bag'), 'Buy')],
  });
}

function buyReview(sh, it) {
  const bal = (S.wallet && S.wallet.balance_zat) || null;
  sh.setTitle('Review purchase');
  sh.setBody(
    h('div', { class: 'row nowrap' }, art(it.note, it.name, 'ico'), h('div', null, h('b', null, it.name || 'Item'), h('div', { class: 'small muted' }, 'from ' + S.market.base.replace(/^https?:\/\//, '')))),
    h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Price'), h('span', null, zec(it.price_zat))), h('div', { class: 'r' }, h('span', null, 'Network fee'), h('span', null, 'about ' + zec(FEE_ZAT))), h('div', { class: 'r total' }, h('span', null, 'Total'), h('span', null, 'about ' + zec(it.price_zat + FEE_ZAT)))),
    bal !== null && bal < it.price_zat + FEE_ZAT ? h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'Your balance may be too low for this purchase.')) : null,
    h('div', { class: 'callout' }, icon('shield'), h('span', null, 'Your payment and the item move in one transaction. If someone else buys it first, nothing happens and your ZEC stays with you.')),
  );
  const art2 = sh.box.querySelector('.row .ico');
  if (art2) {
    art2.style.width = '52px';
    art2.style.height = '52px';
    art2.style.borderRadius = '14px';
    art2.style.display = 'grid';
    art2.style.placeItems = 'center';
    art2.style.fontSize = '16px';
  }
  sh.setFoot(
    h('button', { class: 'btn', onclick: () => sh.close() }, 'Cancel'),
    h('button', {
      class: 'btn primary',
      onclick: async () => {
        sh.setTitle('Buying ' + (it.name || 'item'));
        const r = await runInSheet(sh, '/api/buy', { url: S.market.base, item: it.note, name: it.name }, {
          total: 5,
          success: (r) => ({
            title: (it.name || 'The item') + ' is yours',
            text: 'Signed by both sides and sent. It shows in your wallet once mined, in about a minute.',
            extra: txRow(r.txid),
            actions: [h('button', { class: 'btn', onclick: () => sh.close() }, 'Keep shopping'), h('button', { class: 'btn primary', onclick: () => { sh.close(); go('wallet'); } }, 'Go to wallet')],
          }),
          failTitle: 'Not bought',
        });
        if (r) {
          try {
            S.market = await api('/api/market', { url: S.market.base });
          } catch {
            /* refreshed next time */
          }
          if (S.view === 'market') loadView(true);
        }
      },
    }, 'Confirm and buy'),
  );
}

async function openHistory(base, note, name) {
  const sh = sheet({ title: 'History of ' + (name || 'item'), body: [] });
  await runInSheet(sh, '/api/verify', base ? { url: base, item: note } : { item: note }, {
    total: 1,
    failTitle: 'No history found',
    success: (r) => {
      const hops = r.hops || [];
      const lastMine = !base && hops.length && hops[hops.length - 1].txid === String(note).split(':')[0];
      return {
        title: lastMine ? 'It is in your wallet' : statusText(r.status),
        text: 'Read from the chain with the sellers’ viewing keys only. Nobody was asked who owns what.',
        extra: h(
          'div',
          { class: 'timeline' },
          hops.map((hop, i) => {
            const you = lastMine && i === hops.length - 1;
            return h('div', { class: 'hop' + (you ? ' you' : '') }, h('div', { class: 't' }, i === 0 ? 'Created' : 'Changed hands'), h('div', { class: 'small muted' }, 'Block ' + hop.height + ' · ' + holderText(hop, you)), txLink(hop.txid, 8));
          }),
        ),
      };
    },
  });
}
function holderText(hop, you) {
  if (you) return 'held by you';
  const h_ = String(hop.holder || '');
  if (!h_) return 'held by someone whose key you do not have';
  if (h_.endsWith('listings.json')) return 'held by your shop';
  return 'held by the seller';
}
function statusText(s) {
  const st = s && (s.state || s);
  if (st === 'held') return 'Held by the seller';
  if (st === 'transferred_to_unknown_holder') return 'Sold on';
  if (st === 'lineage_broken') return 'The trail stops';
  return 'Traced';
}

// ------------------------------------------------------------ sell

function viewSell() {
  const w = S.wallet || { items: [] };
  const shop = S.shop || { running: false, events: [] };
  const listen = (shop.listen || (S.status.shop && S.status.shop.listen) || '127.0.0.1:8900');
  const shopUrl = shop.public || (S.status.shop && S.status.shop.public) || 'http://' + listen.replace('0.0.0.0', location.hostname === 'localhost' ? '127.0.0.1' : location.hostname);
  const sw = h('button', { class: 'switch', role: 'switch', 'aria-checked': String(!!shop.running), 'aria-label': 'Shop open', onclick: () => toggleShop(!shop.running, sw) });
  const names = h('textarea', { class: 'input', id: 'names', placeholder: 'One name per line\nGolden ticket #1\nGolden ticket #2' });
  const listed = w.items.filter((i) => i.listed_zat).length;
  return h(
    'div',
    null,
    h('h2', { class: 'page' }, 'Sell'),
    h('p', { class: 'pagesub' }, 'Create items, set prices, and open your shop. It signs only its half of each trade.'),
    h(
      'div',
      { class: 'grid2' },
      h(
        'div',
        { class: 'card stack' },
        h('div', { class: 'row nowrap' }, h('div', { class: 'grow', style: null }, h('b', null, 'Your shop'), h('div', { class: 'small muted' }, shop.running ? 'Open · ' + listed + ' item' + (listed === 1 ? '' : 's') + ' listed' : 'Closed')), h('span', { class: 'spacer' }), sw),
        shop.running
          ? h('div', { class: 'stack' }, h('div', { class: 'addr mono' }, shopUrl), h('div', { class: 'row' }, h('button', { class: 'btn sm', onclick: () => copy(shopUrl, 'Shop address copied') }, icon('copy'), 'Copy address'), navigator.share ? h('button', { class: 'btn sm', onclick: () => navigator.share({ text: shopUrl }).catch(() => {}) }, icon('share'), 'Share') : null))
          : h('p', { class: 'small muted' }, listed ? 'Open the shop so buyers can reach it.' : 'List an item below, then open the shop.'),
        shop.running && S.hosted ? h('p', { class: 'small muted' }, 'Your shop stays open and keeps signing offers after you sign out.') : null,
        S.hosted ? null : h('details', null, h('summary', { class: 'small muted' }, 'Advanced'), h('div', { class: 'field', style: null }, h('label', { for: 'listen' }, 'Listen on'), h('input', { class: 'input', id: 'listen', value: listen, onchange: (e) => (S.listen = e.target.value) }), h('div', { class: 'hint' }, 'Use 0.0.0.0:8900 to let other machines in. For the internet, put a TLS proxy or a Tor hidden service in front.'))),
      ),
      h(
        'div',
        { class: 'card stack' },
        h('b', null, 'Create items'),
        names,
        h('button', { class: 'btn primary', onclick: () => mint(names.value) }, icon('spark'), 'Create'),
        h('div', { class: 'hint small muted' }, 'Each item is a real shielded note of 0.00001 ZEC, named in its encrypted memo. Creating costs a network fee of about 0.00015 ZEC.'),
      ),
    ),
    h('div', { class: 'section-title' }, 'Inventory', h('span', { class: 'spacer' }), w.items.length ? h('span', { class: 'chip' }, String(w.items.length)) : null),
    w.items.length
      ? h('div', { class: 'card', style: null }, h('div', { class: 'list' }, w.items.map((it) => inventoryRow(it))))
      : h('div', { class: 'card' }, h('div', { class: 'empty' }, h('div', { class: 'ico' }, icon('tag')), h('h3', null, 'Nothing to sell yet'), h('p', null, 'Create your first items above. They can be listed once their block is mined, about a minute later.'))),
    h('div', { class: 'section-title' }, 'Offers'),
    h('div', { class: 'card', id: 'feed' }, feedList(shop.events)),
  );
}

function inventoryRow(it) {
  const thumb = art(it.note, it.name, 'ico');
  thumb.style.fontSize = '14px';
  const status = it.listed_zat ? h('span', { class: 'chip ok' }, zec(it.listed_zat)) : it.mined ? h('span', { class: 'chip' }, 'Not listed') : h('span', { class: 'chip warn' }, 'Waiting for block');
  return h(
    'div',
    { class: 'li' },
    thumb,
    h('div', { class: 'grow' }, h('div', { class: 'title' }, it.name || 'Unnamed item'), h('div', { class: 'sub' }, short(it.note, 6))),
    status,
    it.listed_zat
      ? h('button', { class: 'iconbtn', 'aria-label': 'Options for ' + (it.name || 'item'), onclick: () => listedOptions(it) }, icon('more'))
      : h('button', { class: 'btn sm primary', disabled: !it.mined, onclick: () => listSheet(it) }, 'List'),
  );
}

function listSheet(it) {
  const price = h('input', { class: 'input', id: 'price', inputmode: 'decimal', placeholder: '0.001', autocomplete: 'off' });
  const err = h('p', { class: 'small err', role: 'alert' });
  const sh = sheet({
    title: 'List ' + (it.name || 'item'),
    body: [h('div', { class: 'field' }, h('label', { for: 'price' }, 'Price'), h('div', { class: 'inputwrap' }, price, h('span', { class: 'suffix' }, 'ZEC')), h('div', { class: 'hint' }, 'At least 0.00005001 ZEC. The buyer pays the network fee.')), err],
    foot: [
      h('button', {
        class: 'btn primary lg',
        onclick: async () => {
          const p = price.value.trim().replace(',', '.');
          if (!/^\d*\.?\d{0,8}$/.test(p) || !(Number(p) > 0.00005)) return (err.textContent = 'Enter a price above 0.00005 ZEC.');
          sh.setTitle('Listing ' + (it.name || 'item'));
          await runInSheet(sh, '/api/publish', { note: it.note, name: it.name, price: p + ' zec' }, { total: 2, success: () => ({ title: 'Listed', text: (it.name || 'The item') + ' is for sale at ' + p + ' ZEC.' + (S.shop && S.shop.running ? '' : ' Open your shop so buyers can reach it.') }), failTitle: 'Not listed' });
          loadView(true);
        },
      }, 'List for sale'),
    ],
  });
}

function listedOptions(it) {
  const sh = sheet({
    title: it.name || 'Item',
    body: [
      h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Listed for'), h('b', null, zec(it.listed_zat)))),
      h('p', { class: 'small muted' }, 'Delisting hides the item. Offers your shop already signed stay valid for up to 40 blocks; cancelling ends them for good by moving the item to a fresh note, for a network fee.'),
    ],
    foot: [
      h('button', { class: 'btn danger', onclick: async () => { sh.setTitle('Cancelling offers'); await runInSheet(sh, '/api/cancel', { note: it.note }, { total: 1, success: () => ({ title: 'Offers cancelled', text: 'The item moved to a fresh note of yours.' }), failTitle: 'Not cancelled' }); loadView(true); } }, 'Cancel offers'),
      h('button', { class: 'btn', onclick: async () => { sh.setTitle('Delisting'); await runInSheet(sh, '/api/delist', { note: it.note }, { total: 1, success: () => ({ title: 'Delisted', text: 'The item is no longer for sale.' }), failTitle: 'Not delisted' }); loadView(true); } }, 'Delist'),
    ],
  });
}

async function mint(text) {
  const names = text.split('\n').map((s) => s.trim()).filter(Boolean);
  if (!names.length) return toast('Write at least one name', 'err');
  const sh = sheet({ title: 'Creating ' + names.length + ' item' + (names.length > 1 ? 's' : ''), body: [] });
  await runInSheet(sh, '/api/mint', { names }, { total: 2, success: () => ({ title: 'Created', text: 'They can be listed once the block is mined, in about a minute.' }), failTitle: 'Not created' });
  loadView(true);
}

async function toggleShop(open, sw) {
  sw.disabled = true;
  try {
    const listen = (document.getElementById('listen') || {}).value || '127.0.0.1:8900';
    S.shop = await api(open ? '/api/shop/start' : '/api/shop/stop', open ? { listen } : {});
    toast(open ? 'Shop open' : 'Shop closed');
  } catch (e) {
    toast(human(e.message), 'err');
  }
  loadView(true);
}

const EVENT = {
  signed: (e) => ['in', 'check', 'Offer accepted' + (e.name ? ' · ' + e.name : ''), e.paid_zat ? '+' + zec(e.paid_zat) : ''],
  sold: (e) => ['acc', 'bag', 'Sold' + (e.name ? ' · ' + e.name : ''), ''],
  rejected: (e) => ['out', 'x', 'Offer refused', String(e.reason || '').replace(/^refused:\s*/, '')],
  deferred: () => ['', 'clock', 'Offer waiting for the next block', ''],
  error: (e) => ['out', 'alert', 'Problem', e.error || ''],
};
function feedList(events) {
  const rows = (events || []).filter((e) => EVENT[e.event]).slice(-40).reverse();
  if (!rows.length) return h('div', { class: 'empty' }, h('div', { class: 'ico' }, icon('store')), h('p', null, 'Offers appear here while your shop is open.'));
  return h(
    'div',
    { class: 'list' },
    rows.map((e) => {
      const [kind, ic, title, extra] = EVENT[e.event](e);
      const isAmt = /^\+/.test(extra);
      return h('div', { class: 'li' }, h('span', { class: 'ico ' + kind }, icon(ic)), h('div', { class: 'grow' }, h('div', { class: 'title' }, title), h('div', { class: 'sub' }, ago(e.at) + (extra && !isAmt ? ' · ' + extra : ''))), isAmt ? h('span', { class: 'amt in' }, extra) : null);
    }),
  );
}
async function pollShop() {
  if (S.view !== 'sell' || document.hidden) return;
  try {
    const shop = await api('/api/shop');
    const changed = !S.shop || shop.events.length !== S.shop.events.length || shop.running !== S.shop.running;
    S.shop = shop;
    if (!changed) return;
    const feed = document.getElementById('feed');
    if (feed) feed.replaceChildren(feedList(shop.events));
    if (shop.events.some((e) => e.event === 'sold') && !document.querySelector('#layer .scrim') && !isTyping()) loadView(true);
  } catch {
    /* next tick */
  }
}

// ------------------------------------------------------------ activity

const KIND = {
  buy: ['out', 'bag'],
  sale: ['in', 'tag'],
  mint: ['acc', 'spark'],
  list: ['', 'tag'],
  delist: ['', 'tag'],
  cancel: ['', 'x'],
  pay: ['out', 'up'],
  wallet: ['acc', 'shield'],
  faucet: ['in', 'down'],
};
function activityList(items) {
  return h(
    'div',
    { class: 'list' },
    items.map((a) => {
      const [kind, ic] = KIND[a.kind] || ['', 'clock'];
      // A buy cost its price and the network fee.
      const amount = a.kind === 'buy' && a.price_zat && a.fee_zat ? a.price_zat + a.fee_zat : a.price_zat || a.amount_zat;
      return h(
        'div',
        { class: 'li' },
        h('span', { class: 'ico ' + kind }, icon(ic)),
        h('div', { class: 'grow' }, h('div', { class: 'title' }, a.text || a.kind), h('div', { class: 'sub' }, ago(a.at), a.kind === 'buy' && a.fee_zat ? ' · fee ' + zec(a.fee_zat) : null, a.txid ? [' · ', txLink(a.txid, 5)] : null)),
        amount ? h('span', { class: 'amt' + (a.kind === 'sale' || a.kind === 'faucet' ? ' in' : '') }, (a.kind === 'sale' || a.kind === 'faucet' ? '+' : '−') + zec(amount)) : null,
      );
    }),
  );
}
function viewActivity() {
  const a = S.activity || [];
  return h(
    'div',
    null,
    h('h2', { class: 'page' }, 'Activity'),
    h('p', { class: 'pagesub' }, 'Everything this wallet did, newest first. Transaction links open ZecBlock, which then sees which transaction you look at.'),
    a.length ? h('div', { class: 'card' }, activityList(a)) : h('div', { class: 'card' }, h('div', { class: 'empty' }, h('div', { class: 'ico' }, icon('clock')), h('h3', null, 'Nothing yet'), h('p', null, 'Receive ZEC, buy an item or create one, and it shows up here.'))),
  );
}

// A new link pasted into this tab changes only the part after '#', which does not reload the
// page: take the new key and start over.
window.addEventListener('hashchange', () => {
  if (/token=[0-9a-f]{32}/.test(location.hash)) location.reload();
});
document.addEventListener('visibilitychange', () => {
  if (!document.hidden && S.status && S.status.unlocked) backgroundSync();
});
boot();
