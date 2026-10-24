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
const ITEM_ZAT = 1000; // the value of an item note
// Confirmations the wallet asks for before it spends a note. The wallet in the browser spends
// from the first: a trade is one transaction, so a chain reorganisation that undid the coins
// paying for it would undo the trade with them. The wallet zsettle-app runs keeps ZIP 315's
// policy, the wallet library's default: 10 for a note from another wallet, 3 for its own
// change.
const confs = () => (S.engine ? { other: 1, change: 1 } : { other: 10, change: 3 });
// When received coins can be spent, in words.
function spendAfter() {
  const c = confs();
  if (c.other === 1) return 'Coins can be spent once they are in a block, about a minute after they are sent.';
  return 'Coins from another wallet can be spent after ' + c.other + ' confirmations, about ' + minutesFor(c.other) + ' minutes.';
}
const BLOCK_SECS = { main: 75, test: 25 };
// An ownership proof is accepted for about this many minutes after the block it names
// (40 blocks before NU7, 120 from NU7).
const PROOF_LIFE_MIN = 50;
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
  camera: ['M4 8h3l2-3h6l2 3h3v11H4z', 'M12 17a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z'],
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
  if (d < 172800) return 'yesterday';
  if (d < 7 * 86400) return Math.round(d / 86400) + ' days ago';
  const when = new Date(sec * 1000);
  const opts = { day: 'numeric', month: 'short' };
  if (when.getFullYear() !== new Date().getFullYear()) opts.year = 'numeric';
  return when.toLocaleDateString(undefined, opts);
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
function art(seed, name, cls = 'art', icon) {
  // The seller's chosen emoji, else a mark from the name.
  const el = h('div', { class: cls + (icon ? ' has-icon' : ''), 'aria-hidden': 'true' }, icon || itemMark(name));
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
  if (/^the faucet/i.test(m)) return m.charAt(0).toUpperCase() + m.slice(1) + '.';
  if (/; your coins were freed for the next purchase$/i.test(m)) return human(m.replace(/; your coins were freed for the next purchase$/i, '')) + ' Your coins were freed for the next purchase.';
  if (/would be dust/i.test(m)) return 'This would leave less than 0.00005 ZEC of change, too little to ever spend. Pick a cheaper item, or add coins first.';
  if (/already spent some of its inputs|another transaction in the mempool|double-spend|duplicate nullifier/i.test(m)) return 'Someone else bought this item first. Your ZEC did not move.' + (/moved to a fresh note/.test(m) ? ' The coins held for it moved to a fresh note.' : '');
  if (/insufficient|not enough|no spendable|no note can pay|InsufficientFunds/i.test(m)) return 'Not enough spendable ZEC. ' + confirmingNote();
  if (/Connection (Failed|refused)|fetching http|could not be reached|timed out/i.test(m)) return 'Could not reach the shop. Check the address and that the shop is open.';
  if (/seller refused the offer/i.test(m)) return m.replace(/^.*?seller refused the offer:\s*/i, 'The seller refused the offer: ');
  if (/no answer from the seller/i.test(m)) return 'The seller did not answer in time. Your ZEC did not move; try again later.';
  if (/the wallet is locked/i.test(m)) return 'The wallet is locked. Unlock it first.';
  const t = m.charAt(0).toUpperCase() + m.slice(1);
  return t.length > 280 ? t.slice(0, 280) + '…' : t;
}

function minutesFor(blocks) {
  return Math.max(1, Math.ceil((blocks * (BLOCK_SECS[(S.status && S.status.network) || 'main'] || 75)) / 60));
}

// Mined notes that still need confirmations before the wallet spends them, and the slowest one.
function maturity(w) {
  const tip = ((S.status && S.status.last_sync && S.status.last_sync.report) || {}).chain_tip || 0;
  const rows = ((w && w.notes) || [])
    .filter((n) => n.mined && tip)
    .map((n) => {
      const need = n.change ? confs().change : confs().other;
      const have = Math.max(0, tip - n.mined + 1);
      return { value: n.value_zat, need, have, left: Math.max(0, need - have) };
    })
    .filter((r) => r.left > 0);
  const left = rows.length ? Math.max(...rows.map((r) => r.left)) : 0;
  const slow = rows.find((r) => r.left === left) || { have: 0, need: 0 };
  return { zat: rows.reduce((a, r) => a + r.value, 0), left, have: slow.have, need: slow.need, minutes: minutesFor(left) };
}

// Coins spendable now: mined, with their confirmations.
function spendableZat() {
  const w = S.wallet;
  if (!w || !w.notes) return null;
  const mined = w.notes.filter((n) => n.mined && !n.reserved).reduce((a, n) => a + n.value_zat, 0);
  return mined - maturity(w).zat;
}
// A warning for a confirm step when the coins spendable now do not cover `zat`.
function spendWarning(zat) {
  const can = spendableZat();
  if (can === null || can >= zat) return null;
  const all = (S.wallet && S.wallet.balance_zat) || 0;
  return h('div', { class: 'callout warn' }, icon('alert'), h('span', null, all >= zat ? 'Not spendable yet. ' + confirmingNote() : 'Your balance is too low for this. ' + (S.status && S.status.network === 'test' ? 'Get test coins on the Wallet tab.' : 'Add ZEC first.')));
}

function confirmingNote() {
  const m = maturity(S.wallet);
  if (m.zat) return zec(m.zat) + ' is still confirming (' + m.have + ' of ' + m.need + ' confirmations); it can be spent in about ' + m.minutes + ' min.';
  const c = confs();
  if (c.other === 1) return 'Coins can be spent once they are in a block.';
  return 'A note from another wallet is spendable after ' + c.other + ' confirmations, your own change after ' + c.change + '.';
}

async function api(path, body) {
  if (S.engine) {
    try {
      return await S.engine.api(path, body);
    } catch (e) {
      if (/unlock your wallet/i.test(e.message) && !S.engine.isOpen()) {
        S.timers.forEach(clearInterval);
        document.getElementById('layer').replaceChildren();
        render(await viewWelcomeBrowser());
      }
      throw e;
    }
  }
  const headers = { 'X-Zsettle': '1' };
  if (S.token) headers['X-Zsettle-Token'] = S.token;
  const opt = { method: body === undefined ? 'GET' : 'POST', headers, credentials: 'same-origin' };
  if (body !== undefined) {
    opt.headers['Content-Type'] = 'application/json';
    opt.body = JSON.stringify(body);
  }
  const r = await fetch(path, opt);
  if (r.status === 401) {
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
  box.tabIndex = -1;
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
  requestAnimationFrame(() => box.focus({ preventScroll: true }));
  setTimeout(() => {
    const f = box.querySelector('input:not([type=hidden]), textarea');
    if (f) f.focus({ preventScroll: true });
  }, 30);
  return api_;
}

// Run a job inside a sheet: steps with a progress bar, then a result screen.
async function runInSheet(sh, path, body, { total = 3, success, failTitle = 'That did not work', doneTitle }) {
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
    if (doneTitle) sh.setTitle(doneTitle);
    // The icon follows the outcome, not the job: a check that ran fine but proved nothing
    // shows a warning, not a tick.
    const bad = out.tone === 'bad';
    sh.setBody(h('div', { class: 'result' }, h('div', { class: 'big' + (bad ? ' bad' : '') }, icon(bad ? 'alert' : 'check')), h('h3', null, out.title), out.text ? h('p', null, out.text) : null), out.extra || null);
    sh.setFoot(out.actions || [h('button', { class: 'btn primary', onclick: () => sh.close() }, 'Done')]);
    return j.result;
  } catch (e) {
    if (doneTitle) sh.setTitle(doneTitle);
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
// A browser built into another app (Instagram, Facebook, Telegram, WeChat, a plain Android
// WebView, an iOS app view without Safari): passkeys usually fail there, and some force
// their own dark colours onto the page.
function inAppBrowser(ua = navigator.userAgent || '') {
  if (/FBAN|FBAV|FB_IAB|Instagram|Line\/|MicroMessenger|Snapchat|TikTok|; wv\)/i.test(ua)) return true;
  return /iPhone|iPad/.test(ua) && !/Safari\//.test(ua) && !/CriOS|FxiOS|EdgiOS/.test(ua);
}
const passkeyCancelled = () =>
  inAppBrowser()
    ? 'The passkey prompt did not open. Browsers inside apps often block passkeys: open this page in Chrome or Safari, or use a passphrase instead.'
    : 'The passkey prompt was cancelled. Try again, or use a passphrase instead.';

// The passkey's PRF output, never stored, is the wallet's passphrase.
async function passkeyCreate() {
  const salt = crypto.getRandomValues(new Uint8Array(32));
  const cred = await navigator.credentials.create({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rp: { name: 'zsettle', id: location.hostname },
      user: { id: crypto.getRandomValues(new Uint8Array(16)), name: 'zsettle-' + S.status.profile, displayName: 'zsettle wallet (' + S.status.profile + ')' },
      pubKeyCredParams: [
        { type: 'public-key', alg: -7 },
        { type: 'public-key', alg: -257 },
      ],
      authenticatorSelection: { residentKey: 'preferred', userVerification: 'required' },
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

// A passkey wallet confirms each transaction with the passkey: the secret that opened the
// wallet is derived once more and sent with the request, and the server checks it is the same.
// A passphrase wallet confirms with the confirm step alone.
async function confirmSecret() {
  if (S.engine) return S.engine.confirm();
  const pk = S.status && S.status.passkey;
  if (!pk) return {};
  const secret = await passkeySecret(pk);
  return { confirm: 'passkey:' + hexOf(secret) };
}
// The label of a confirm button: a passkey wallet is told the passkey will be asked.
function confirmLabel(text) {
  return S.status && S.status.passkey ? 'Confirm with passkey' : text;
}
// Ask the passkey; a refusal or a cancel shows a short message and returns null.
async function askConfirm(err) {
  try {
    return await confirmSecret();
  } catch (e) {
    const text = e.name === 'NotAllowedError' ? 'Confirm with your passkey to continue.' : human(e.message);
    if (err) err.textContent = text;
    else toast(text, 'err');
    return null;
  }
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
      // The wallet runs in this page: keys, proofs and signatures stay on this device.
      S.hosted = hello;
      render(h('div', { class: 'boot' }, h('div', { class: 'spinner', 'aria-label': 'Starting the wallet' }), h('p', { class: 'small muted' }, 'Starting the wallet in your browser…')));
      const cannotRun = (e) => render(h('div', { class: 'onboard' }, h('div', { class: 'panel card stack' }, h('div', { class: 'hero' }, logo(), h('h1', null, 'This browser cannot run the wallet')), h('p', { class: 'err' }, human(e.message)), h('p', { class: 'small muted' }, 'Use a current Chrome, Safari, Firefox or Edge, outside other apps.'), linksRow())));
      try {
        S.engine = await import('/engine.js');
      } catch (e) {
        return cannotRun(e);
      }
      // The engine downloads and starts while the visitor looks at the unlock screen; the
      // first call into the wallet waits for it.
      S.engine.start().catch(cannotRun);
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
  if (S.engine && !S.engine.isOpen()) return render(await viewWelcomeBrowser());
  try {
    S.status = await api('/api/status');
  } catch (e) {
    return render(h('div', { class: 'onboard' }, h('div', { class: 'panel card' }, h('p', { class: 'err' }, human(e.message)))));
  }
  if (!S.status.wallet) return render(viewCreate());
  if (!S.status.unlocked) return render(viewUnlock());
  const hv = location.hash.slice(1);
  if (VIEWS.some((v) => v[0] === hv)) S.view = hv;
  history.replaceState({ view: S.view }, '', '#' + S.view);
  renderShell();
  loadView();
  startTimers();
}

function startTimers() {
  S.timers.forEach(clearInterval);
  S.timers = [setInterval(backgroundSync, SYNC_EVERY_MS), setInterval(pollShop, 3000)];
  if (S.engine) S.timers.push(setInterval(watchSync, 2000));
  backgroundSync();
}

// A page in the background gets little time on a phone or laptop, so it syncs only while it
// is in front; coming back starts a sync at once if the last one is old.
function syncOnReturn() {
  if (document.hidden || !document.hasFocus()) return;
  const at = S.status && S.status.last_sync && S.status.last_sync.at;
  if (!at || Date.now() / 1000 - at > 20) backgroundSync();
}
window.addEventListener('focus', syncOnReturn);
document.addEventListener('visibilitychange', syncOnReturn);

// The wallet in the browser may sync by itself (a shop's key to scan): follow it in the chip
// and in the wallet's catching-up line.
async function watchSync() {
  if (!S.status || !S.status.unlocked || document.hidden) return;
  try {
    S.status = await api('/api/status');
  } catch {
    return;
  }
  updateSyncChip();
  const line = document.getElementById('catchup');
  if (line) line.textContent = catchUpText();
}
function catchUpText() {
  const p = S.status && S.status.sync_progress;
  if (!(S.syncing || (S.status && S.status.syncing))) return '';
  return p && p.tip > p.scanned ? 'Catching up with the chain: ' + (p.tip - p.scanned).toLocaleString('en') + ' blocks left' : 'Catching up with the chain…';
}

async function backgroundSync() {
  if (S.syncing || S.busy || document.hidden || !document.hasFocus() || !S.status || !S.status.unlocked) return;
  S.syncing = true;
  updateSyncChip();
  try {
    const { job } = await api('/api/sync', {});
    await follow(job, S.engine ? watchSync : null);
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
      err.textContent = e.name === 'NotAllowedError' ? passkeyCancelled() : human(e.message);
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

// ------------------------------------------------------------ the wallet in this browser

// First visit: create or restore. Later visits: unlock the wallet this browser holds.
async function viewWelcomeBrowser() {
  const held = await S.engine.wallets();
  // The wallet asked for last, or the one opened last, comes first.
  const pref = held.findIndex((x) => x.network === S.preferNet);
  if (pref > 0) held.unshift(held.splice(pref, 1)[0]);
  const err = h('p', { class: 'small err', role: 'alert' });
  const note = h('p', { class: 'small muted', 'aria-live': 'polite' });
  const hero = (title, lede) => h('div', { class: 'hero' }, logo(), h('h1', null, title), h('p', { class: 'lede' }, lede));
  async function act(btn, fn) {
    err.textContent = '';
    btn.disabled = true;
    try {
      await fn();
    } catch (e) {
      err.textContent = e.name === 'NotAllowedError' ? passkeyCancelled() : human(e.message);
    } finally {
      btn.disabled = false;
      note.textContent = '';
    }
  }
  async function opened(fresh) {
    S.view = 'wallet';
    await refresh();
    // A new testnet wallet asks the faucet for its test coins at once.
    if (fresh && S.status && S.status.faucet) {
      try {
        watchFaucet(await api('/api/faucet', {}));
      } catch {
        /* the Get coins button stays */
      }
    }
  }
  if (held.length) {
    const w = held[0];
    const pass = h('input', { class: 'input', type: 'password', id: 'unlockpw', autocomplete: 'current-password', placeholder: 'Passphrase', 'aria-label': 'Passphrase' });
    const btn = h('button', { class: 'btn primary lg block' }, w.lock === 'passkey' ? [icon('finger'), 'Unlock with passkey'] : 'Unlock');
    btn.onclick = () => act(btn, async () => {
      if (w.lock !== 'passkey' && !pass.value) throw new Error('Enter your passphrase.');
      note.textContent = w.lock === 'passkey' ? 'Confirm with your device.' : 'Opening your wallet…';
      await S.engine.unlock(w.network, pass.value);
      await opened(false);
    });
    pass.addEventListener('keydown', (e) => e.key === 'Enter' && btn.click());
    const other = held[1];
    const otherNet = w.network === 'main' ? 'test' : 'main';
    return h(
      'main',
      { class: 'onboard' },
      h(
        'div',
        { class: 'panel' },
        hero('Welcome back', 'Your ' + (w.network === 'main' ? 'mainnet' : 'testnet') + ' wallet is in this browser.'),
        h(
          'div',
          { class: 'card stack lg' },
          w.lock === 'passkey' ? null : pass,
          btn,
          note,
          err,
          other
            ? h('button', { class: 'linkbtn', onclick: async () => { S.preferNet = other.network; render(await viewWelcomeBrowser()); } }, 'Open the ' + (other.network === 'main' ? 'mainnet' : 'testnet') + ' wallet instead')
            : h('button', { class: 'linkbtn', onclick: () => createSheet(otherNet) }, 'Add a ' + (otherNet === 'main' ? 'mainnet' : 'testnet') + ' wallet'),
          h('button', { class: 'linkbtn', onclick: () => restoreSheet(w.network, true) }, w.lock === 'passkey' ? 'Lost the passkey? Restore from the 24 words' : 'Forgot the passphrase? Restore from the 24 words'),
        ),
        linksRow(),
      ),
    );
  }
  let network = 'test';
  const seg = h(
    'div',
    { class: 'segment', role: 'group', 'aria-label': 'Network' },
    ['test', 'main'].map((n) =>
      h('button', { type: 'button', 'aria-pressed': String(n === network), onclick: (e) => { network = n; seg.querySelectorAll('button').forEach((b) => b.setAttribute('aria-pressed', String(b === e.currentTarget))); } }, n === 'test' ? 'Testnet · free coins' : 'Mainnet · real ZEC'),
    ),
  );
  return h(
    'main',
    { class: 'onboard' },
    h(
      'div',
      { class: 'panel' },
      hero('Trade privately on Zcash', 'Buy and sell items for ZEC in one shielded transaction. No escrow, no one in the middle.'),
      h(
        'div',
        { class: 'card stack lg' },
        inAppBrowser()
          ? h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'This page is open inside an app, where passkeys usually do not work. Tap the ⋮ or ··· menu and choose “Open in browser” (Chrome or Safari), or use a passphrase.'))
          : null,
        h(
          'div',
          { class: 'points' },
          h('div', { class: 'p' }, icon('shield'), h('span', null, 'Your wallet lives in this browser. The keys never leave your device.')),
          h('div', { class: 'p' }, icon('finger'), h('span', null, 'Unlock it with your fingerprint, face or device PIN.')),
          h('div', { class: 'p' }, icon('check'), h('span', null, 'Item and payment move together, or not at all.')),
        ),
        h('div', { class: 'field' }, h('label', null, 'Network'), seg),
        h('button', { class: 'btn primary lg block', onclick: () => createSheet(network) }, icon('spark'), 'Create a wallet'),
        h('button', { class: 'btn lg block', onclick: () => restoreSheet(network) }, icon('down'), 'Restore from 24 words'),
        err,
      ),
      howItWorks(),
      linksRow(),
    ),
  );

  // A new wallet: the 24 words first, then how this browser locks it.
  async function createSheet(net) {
    const phrase = await S.engine.newMnemonic();
    const ok = h('input', { type: 'checkbox', id: 'wrote' });
    const sh = sheet({
      title: 'Your recovery words',
      body: [
        h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'Write these 24 words on paper, in order. They are the only way back to this wallet if this browser loses its data. Anyone who sees them controls the wallet.')),
        h('ol', { class: 'words' }, phrase.split(' ').map((x) => h('li', null, x))),
        h('label', { class: 'check' }, ok, ' I wrote the 24 words down'),
      ],
      foot: [h('button', { class: 'btn primary', onclick: () => (ok.checked ? lockSheet(sh, net, phrase, 0) : toast('Write the words down first, then tick the box', 'err')) }, 'Next')],
    });
  }
  async function restoreSheet(net, replacing) {
    const text = h('textarea', { class: 'input mono', id: 'phrase', rows: '4', autocomplete: 'off', autocapitalize: 'off', spellcheck: 'false', placeholder: '24 words, separated by spaces' });
    const bday = h('input', { class: 'input', id: 'bday', inputmode: 'numeric', placeholder: 'optional' });
    const e2 = h('p', { class: 'small err', role: 'alert' });
    let n2 = net;
    const seg2 = h('div', { class: 'segment', role: 'group', 'aria-label': 'Network' }, ['test', 'main'].map((n) => h('button', { type: 'button', 'aria-pressed': String(n === n2), onclick: (e) => { n2 = n; seg2.querySelectorAll('button').forEach((b) => b.setAttribute('aria-pressed', String(b === e.currentTarget))); } }, n === 'test' ? 'Testnet' : 'Mainnet')));
    const sh = sheet({
      title: 'Restore a wallet',
      body: [
        replacing ? h('p', { class: 'small muted' }, 'Your money is safe on chain. Restoring replaces the locked copy in this browser with a new one, locked the way you choose next.') : null,
        h('div', { class: 'field' }, h('label', null, 'Network'), seg2),
        h('div', { class: 'field' }, h('label', { for: 'phrase' }, 'Recovery words'), text),
        h('div', { class: 'field' }, h('label', { for: 'bday' }, 'Block height the wallet was made at (optional)'), bday, h('div', { class: 'hint' }, 'Empty for a wallet made in the last day. An older wallet needs its height, or it misses its first payments.')),
        e2,
      ],
      foot: [h('button', {
        class: 'btn primary',
        onclick: async () => {
          e2.textContent = '';
          const phrase = text.value.trim().toLowerCase().split(/\s+/).join(' ');
          try {
            await S.engine.checkMnemonic(phrase);
            const b = bday.value.trim() ? Number(bday.value.trim()) : 0;
            if (!Number.isInteger(b) || b < 0) throw new Error('the height is a whole number');
            lockSheet(sh, n2, phrase, b);
          } catch (e) {
            e2.textContent = /not a valid recovery phrase/i.test(e.message) ? 'That is not a valid recovery phrase. Check the words and their order.' : human(e.message);
          }
        },
      }, 'Next')],
    });
  }
  function lockSheet(sh, net, phrase, birthday) {
    const p1 = h('input', { class: 'input', type: 'password', id: 'np1', autocomplete: 'new-password', placeholder: '12 characters or more' });
    const p2 = h('input', { class: 'input', type: 'password', id: 'np2', autocomplete: 'new-password', placeholder: 'Repeat it' });
    const e3 = h('p', { class: 'small err', role: 'alert' });
    const n3 = h('p', { class: 'small muted', 'aria-live': 'polite' });
    const pk = h('button', { class: 'btn primary lg block', disabled: !passkeyUsable() }, icon('finger'), 'Lock with a passkey');
    const pw = h('button', { class: 'btn block' }, 'Lock with this passphrase');
    sh.setTitle('Lock it in this browser');
    sh.setBody(
      h('p', { class: 'small muted' }, 'You unlock the wallet this way on each visit, and confirm each payment with it.'),
      pk,
      h('div', { class: 'section-title' }, 'Or a passphrase'),
      h('div', { class: 'field' }, h('label', { for: 'np1' }, 'Passphrase'), p1),
      h('div', { class: 'field' }, h('label', { for: 'np2' }, 'Repeat passphrase'), p2),
      pw,
      n3,
      e3,
    );
    sh.setFoot();
    const go = (btn, args) => async () => {
      e3.textContent = '';
      btn.disabled = true;
      sh.setDismissable(false);
      try {
        n3.textContent = birthday ? 'Restoring the wallet…' : 'Creating the wallet…';
        await S.engine.create({ network: net, phrase, birthday, ...args() });
        sh.close();
        await opened(!birthday);
      } catch (e) {
        e3.textContent = e.name === 'NotAllowedError' ? passkeyCancelled() : human(e.message);
        n3.textContent = '';
      } finally {
        btn.disabled = false;
        sh.setDismissable(true);
      }
    };
    pk.onclick = go(pk, () => ({ lock: 'passkey' }));
    pw.onclick = go(pw, () => {
      if (p1.value.length < 12) throw new Error('use a passphrase of 12 characters or more');
      if (p1.value !== p2.value) throw new Error('the two passphrases differ');
      return { lock: 'passphrase', passphrase: p1.value };
    });
  }
}

/** One trade in three steps, for a first visit. */
function howItWorks() {
  const steps = [
    ['Get a wallet and coins', 'Your wallet is made in this browser and its keys stay here' + ((S.hosted && S.hosted.has_house && S.hosted.network === 'test') || (S.status && S.status.faucet) ? '; free test coins are one tap away.' : '.')],
    ['Pick an item', 'Shops list real shielded notes, signed by their seller.'],
    ['Buy in one transaction', 'The item and the ZEC change hands together, or not at all. The seller never sees your address or balance.'],
  ];
  return h(
    'section',
    { class: 'how', 'aria-label': 'How a trade works' },
    steps.map(([t, d], i) => h('div', { class: 'how-step' }, h('span', { class: 'how-n', 'aria-hidden': 'true' }, String(i + 1)), h('div', null, h('div', { class: 'how-t' }, t), h('div', { class: 'how-d' }, d)))),
  );
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
      err.textContent = e.name === 'NotAllowedError' ? passkeyCancelled() : human(e.message);
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
  const busy = S.syncing || st.syncing;
  const p = st.sync_progress;
  const left = busy && p && p.tip > p.scanned ? p.tip - p.scanned : 0;
  const label = busy ? (left ? 'Syncing · ' + left.toLocaleString('en') + ' left' : 'Syncing…') : at ? 'Synced ' + ago(at) : 'Not synced';
  return h('button', { class: 'chip sync ' + (busy ? 'info' : at ? 'ok' : 'warn'), id: 'syncchip', onclick: openSyncInfo, title: label + (left ? ' blocks' : '') + ' · tap for details', 'aria-label': label + (left ? ' blocks' : '') + '. Network details' }, busy ? h('span', { class: 'spinner sm' }) : h('span', { class: 'dot' }), h('span', { class: 'lbl' }, label));
}
function updateSyncChip() {
  const old = document.getElementById('syncchip');
  const chip = syncChip();
  if (old && old.title !== chip.title) old.replaceWith(chip);
}

// What the dot in the top bar means: the wallet's view of the chain, and how fresh it is.
function openSyncInfo() {
  const st = S.status || {};
  const rep = (st.last_sync && st.last_sync.report) || {};
  const at = st.last_sync && st.last_sync.at;
  const sh = sheet({
    title: 'Network',
    body: [
      h(
        'div',
        { class: 'summary' },
        h('div', { class: 'r' }, h('span', null, 'Chain'), h('span', null, st.network === 'main' ? 'Zcash mainnet' : 'Zcash testnet')),
        h('div', { class: 'r' }, h('span', null, 'Last synced'), h('span', null, S.syncing || st.syncing ? 'Syncing now…' : at ? ago(at) : 'Not yet')),
        rep.chain_tip ? h('div', { class: 'r' }, h('span', null, 'Block height'), h('span', { class: 'mono' }, String(rep.chain_tip))) : null,
        rep.server ? h('div', { class: 'r' }, h('span', null, 'Server'), h('span', { class: 'mono' }, String(rep.server).replace(/^https?:\/\//, ''))) : null,
      ),
      h('p', { class: 'small muted' }, (S.status.browser ? 'Syncing fetches new blocks through this site from a lightwalletd server and scans them in your browser with your viewing keys.' : 'Syncing fetches new blocks from a lightwalletd server and scans them with your viewing keys.') + ' That is how the wallet learns which coins and items it holds and how many confirmations each has. It runs by itself every minute while this page is in front, and as soon as you come back to it; the dot is green when the last sync succeeded.'),
    ],
    foot: [h('button', { class: 'btn primary', onclick: () => { sh.close(); backgroundSync(); } }, 'Sync now')],
  });
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
      h('div', { class: 'who' }, h('span', null, st.browser ? 'Keys in this browser' : st.hosted ? 'Code ' + st.code : st.profile), netChip.cloneNode(true)),
      h('a', { href: '#', onclick: (e) => { e.preventDefault(); lock(); } }, icon('lock'), st.hosted && !st.browser ? 'Sign out' : 'Lock wallet'),
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
      h('p', { class: 'small muted' }, (S.status.browser ? 'Keys in this browser' : S.status.hosted ? 'Wallet code ' + S.status.code : 'Wallet ' + S.status.profile) + ' · ' + (S.status.network === 'main' ? 'Mainnet' : 'Testnet')),
    ],
    foot: [h('button', { class: 'btn', onclick: () => { sh.close(); openBackup(); } }, icon('shield'), 'Backup words'), h('button', { class: 'btn', onclick: () => { sh.close(); lock(); } }, icon('lock'), 'Lock wallet')],
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
  pass.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') show();
  });
  async function show() {
    err.textContent = '';
    if (!pk && !pass.value) return (err.textContent = 'Enter your passphrase.');
    try {
      const r = S.engine
        ? { words: await S.engine.words(pass.value) }
        : await api('/api/backup', { passphrase: pk ? 'passkey:' + hexOf(await passkeySecret(pk)) : pass.value });
      const grid = h('ol', { class: 'words' }, r.words.map((w) => h('li', null, w)));
      sh.setBody(h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'Write these down in order. Close this when you are done.')), grid);
      sh.setFoot(h('button', { class: 'btn primary', onclick: () => sh.close() }, 'I wrote them down'));
    } catch (e) {
      err.textContent = e.name === 'NotAllowedError' ? passkeyCancelled() : human(e.message);
    }
  }
}

function go(view, push = true) {
  S.view = view;
  if (push && location.hash !== '#' + view) history.pushState({ view }, '', '#' + view);
  document.querySelectorAll('[aria-current="page"]').forEach((b) => b.removeAttribute('aria-current'));
  document.querySelectorAll('.sidenav, .navbtn').forEach((b, i) => {
    if (VIEWS[i % 4][0] === view) b.setAttribute('aria-current', 'page');
  });
  // What the page last knew draws at once; the wallet's answer redraws it. A wallet busy with
  // a long sync can take a while to answer.
  const known = { wallet: S.wallet && S.activity, sell: S.wallet && S.shop, activity: S.activity, market: S.shops }[view];
  const c = document.getElementById('content');
  if (c) c.replaceChildren(known ? { wallet: viewWallet, market: viewMarket, sell: viewSell, activity: viewActivity }[view]() : skeleton());
  window.scrollTo({ top: 0 });
  loadView(!!known);
}

async function lock() {
  if (S.engine) {
    S.timers.forEach(clearInterval);
    await S.engine.lock();
    S.wallet = S.market = S.shop = S.activity = S.status = null;
    document.getElementById('layer').replaceChildren();
    return render(await viewWelcomeBrowser());
  }
  try {
    await api('/api/lock', {});
  } catch {
    /* already signed out */
  }
  S.timers.forEach(clearInterval);
  S.wallet = S.market = S.shop = S.activity = null;
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
  // An empty page that stays empty: say why.
  const slow = setTimeout(() => {
    const c = document.getElementById('content');
    if (S.view === view && c && c.querySelector('.sk') && !c.querySelector('.busy-note')) c.prepend(h('p', { class: 'small muted busy-note' }, 'The wallet is busy catching up with the chain. This page fills in when it is done.'));
  }, 2500);
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
  } finally {
    clearTimeout(slow);
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
  // Coins the wallet cannot see yet: a payment it was told about (the faucet, a sale) that is
  // not among its notes. The next block brings it in.
  const known = new Set(w.notes.map((n) => String(n.note).split(':')[0]));
  const nowSec = Date.now() / 1000;
  const told = (S.activity || []).filter((a) => nowSec - a.at < 1800 && ((a.kind === 'faucet' || a.kind === 'sale') && a.txid ? !known.has(a.txid) : a.kind === 'faucet' && !w.notes.length));
  const incoming = w.notes.filter((n) => !n.mined && !n.change).reduce((a, n) => a + n.value_zat, 0) + told.reduce((a, x) => a + (x.amount_zat || x.price_zat || 0), 0);
  // Change that is not mined yet means a payment of ours is on its way out: the wallet
  // records its own transactions when it sends them, before the next block.
  const settling = w.notes.some((n) => !n.mined && n.change);
  const out = settling ? (S.activity || []).find((a) => ['buy', 'pay', 'mint', 'rotate', 'cancel'].includes(a.kind) && nowSec - a.at < 1800) : null;
  const leaving = out ? (out.kind === 'buy' ? (out.price_zat || 0) + (out.fee_zat || 0) : out.kind === 'pay' ? (out.amount_zat || 0) + mintFee(1) : 0) : 0;
  // Coins in the wallet that still need confirmations before they can be spent.
  const mat = maturity(w);
  const empty = !w.notes.length && !w.items.length;
  return h(
    'div',
    { class: 'stack lg' },
    h('h2', { class: 'sr-only' }, 'Wallet'),
    S.status.browser
      ? h('div', { class: 'callout info' }, icon('shield'), h('span', null, (S.status.network === 'test' ? 'Testnet: these coins are free and have no value. ' : '') + 'Your keys stay in this browser: proofs and signatures are made here, and the server only passes blocks along.'))
      : S.hosted
        ? h('div', { class: 'callout info' }, icon('info'), h('span', null, S.status.network === 'test' ? 'This wallet is on the Zcash testnet: its coins are free and have no value.' : 'This wallet is kept on this server.', S.status.passkey ? null : [' Your wallet code is ', h('b', null, S.status.code || ''), '; you need it to sign in again.']))
        : null,
    S.faucetPending
      ? h('div', { class: 'card row nowrap' }, h('span', { class: 'ico' }, h('span', { class: 'spinner sm', 'aria-hidden': 'true' })), h('div', { class: 'grow' }, h('b', null, 'Free test coins on the way'), h('div', { class: 'small muted' }, '0.003 test ZEC is being sent to your new wallet. This can take a minute.')))
      : S.status.faucet ? h('div', { class: 'card row nowrap' }, h('span', { class: 'ico' }, icon('spark')), h('div', { class: 'grow' }, h('b', null, 'Free test coins'), h('div', { class: 'small muted' }, 'Get 0.003 test ZEC to try buying and selling.')), h('button', { class: 'btn primary', onclick: getCoins }, 'Get coins')) : null,
    w.waiting_zat && !w.movable_zat
      ? h('div', { class: 'card row nowrap' }, h('span', { class: 'ico' }, icon('shield')), h('div', { class: 'grow' }, h('b', null, zec(w.waiting_zat) + ' held for an offer'), h('div', { class: 'small muted' }, 'These coins pay for an offer that has not settled. If it does not, they come back after the offer expires' + (w.waiting_until && S.status.last_sync ? ', in about ' + minutesFor(Math.max(1, w.waiting_until - ((S.status.last_sync.report || {}).chain_tip || 0))) + ' min' : '') + '.')))
      : null,
    w.movable_zat
      ? h('div', { class: 'card row nowrap' }, h('span', { class: 'ico' }, icon('shield')), h('div', { class: 'grow' }, h('b', null, zec(w.movable_zat) + ' held'), h('div', { class: 'small muted' }, 'These coins paid for an offer that did not settle. Move them to a fresh note, so the shop cannot link them to your next payment.')), h('button', { class: 'btn primary', onclick: moveHeld }, 'Move them'))
      : null,
    h(
      'section',
      { class: 'balance-card', 'aria-label': 'Balance' },
      h('div', { class: 'label' }, 'Balance'),
      h('div', { class: 'amount' }, zec(w.balance_zat, { bare: true }), h('small', null, 'ZEC')),
      S.engine ? h('div', { class: 'small muted', id: 'catchup', 'aria-live': 'polite' }, catchUpText()) : null,
      h(
        'div',
        { class: 'small muted' },
        settling
          ? (leaving ? zec(leaving) + ' leaving' : 'A payment is settling') + ' · the next block confirms it'
          : incoming
          ? zec(incoming) + ' on its way · arrives with the next block'
          : mat.zat
            ? zec(mat.zat) + ' confirming · ' + mat.have + ' of ' + mat.need + ' confirmations · spendable in about ' + mat.minutes + ' min'
            : w.notes.some((n) => !n.reserved)
              ? w.notes.filter((n) => !n.reserved).length + ' note' + (w.notes.filter((n) => !n.reserved).length > 1 ? 's' : '') + ' · shielded'
              : 'Shielded',
      ),
      h(
        'div',
        { class: 'actions' },
        h('button', { class: 'btn primary lg', onclick: () => openReceive() }, icon('down'), 'Receive'),
        h('button', { class: 'btn lg', onclick: openSend, disabled: !w.balance_zat }, icon('up'), 'Send'),
      ),
    ),
    empty && !incoming && !S.faucetPending
      ? h(
          'div',
          { class: 'card' },
          h(
            'div',
            { class: 'empty' },
            h('div', { class: 'ico' }, icon('wallet')),
            h('h3', null, 'Fund your wallet'),
            h('p', null, (S.status.network === 'test' ? 'Press Receive, then paste the address into a Zcash testnet faucet.' : 'Press Receive and send ZEC from any shielded Zcash wallet.') + ' ' + spendAfter()),
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

async function moveHeld() {
  const ok = await askConfirm();
  if (!ok) return;
  const sh = sheet({ title: 'Moving held coins', body: [] });
  await runInSheet(sh, '/api/rotate', ok, { total: 2, doneTitle: 'Held coins', success: (r) => ({ title: (r.moved || []).length ? 'Moved' : 'Nothing to move yet', text: (r.moved || []).length ? 'They come back to your balance once the move is mined, in a few minutes.' : 'The offer they paid for can still settle; try again later.', extra: (r.moved || []).length ? txRow(r.moved[0]) : null }), failTitle: 'Not moved' });
  loadView(true);
}

// A hosted server sends a new wallet its test coins by itself; the wallet page shows them coming.
async function watchFaucet(f) {
  if (!f || !f.job) return;
  S.faucetPending = true;
  if (S.view === 'wallet') loadView(true);
  try {
    const j = await follow(f.job);
    if (j.state === 'done') toast('Free test coins sent. They arrive in about a minute.');
  } catch {
    /* the Get coins button stays */
  } finally {
    S.faucetPending = false;
    try {
      S.status = await api('/api/status');
    } catch {
      /* signed out meanwhile */
    }
    if (S.view === 'wallet') loadView(true);
  }
}

async function getCoins() {
  const sh = sheet({ title: 'Free test coins', body: [] });
  await runInSheet(sh, '/api/faucet', {}, { total: 1, success: (r) => ({ title: 'On their way', text: zec(r.amount_zat) + (confs().other === 1 ? ' arrives with the next block, in about a minute, and can be spent then.' : ' arrives in about a minute and can be spent after ' + confs().other + ' confirmations, about ' + minutesFor(confs().other) + ' minutes later.'), extra: txRow(r.txid) }), failTitle: 'No coins this time' });
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
        art(it.genesis || it.note, it.name, 'art', it.icon),
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
      art(it.note, it.name, 'art big', it.icon),
      it.description ? h('p', { class: 'desc' }, it.description) : null,
      h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Status'), h('span', null, it.listed_zat ? 'Listed for ' + zec(it.listed_zat) : it.mined ? 'In your wallet' : 'Arriving')), h('div', { class: 'r' }, h('span', null, 'Note'), h('span', { class: 'mono' }, short(it.note, 10)))),
    ],
    foot: [
      h('button', { class: 'btn', onclick: () => { sh.close(); openHistory(null, it.note, it.name); } }, icon('history'), 'History'),
      it.mined ? h('button', { class: 'btn', onclick: () => { sh.close(); openProve(it); } }, icon('shield'), 'Prove I own it') : null,
      h('button', { class: 'btn primary', onclick: () => { sh.close(); go('sell'); } }, icon('tag'), 'Sell'),
    ],
  });
}

// ------------------------------------------------------------ ownership proofs

/** A fresh challenge for a checker to hand out: short, random, easy to read aloud. */
function newChallenge() {
  const A = 'abcdefghjkmnpqrstuvwxyz23456789';
  const r = crypto.getRandomValues(new Uint8Array(8));
  return 'zs-' + [...r].map((b) => A[b % A.length]).join('').replace(/^(.{4})/, '$1-');
}

function openProve(it) {
  const label = h('input', { class: 'input', id: 'plabel', maxlength: '80', autocomplete: 'off', autocapitalize: 'off', spellcheck: 'false', placeholder: '@yourname' });
  const ch = h('input', { class: 'input', id: 'challenge', maxlength: '200', autocomplete: 'off', autocapitalize: 'off', spellcheck: 'false', placeholder: 'zs-abcd-efgh' });
  const err = h('p', { class: 'small err', role: 'alert' });
  const life = PROOF_LIFE_MIN;
  const sh = sheet({
    title: 'Prove you own ' + (it.name || 'this item'),
    body: [
      h('p', { class: 'small muted' }, 'Make a proof once and share it, like an address. Anyone can check it for about the next ' + life + ' minutes, and it stops working at once if the item leaves your wallet. It does not show your ZEC balance or the address you get paid at.'),
      h('div', { class: 'field' }, h('label', { for: 'plabel' }, 'Your name or handle (optional)'), label, h('div', { class: 'hint' }, 'Signed into the proof, so a copy still says it was you.')),
      h('details', null, h('summary', { class: 'small muted' }, 'Live check (optional)'), h('div', { class: 'field', style: null }, h('label', { for: 'challenge' }, 'Their code'), ch, h('div', { class: 'hint' }, 'Only if the checker is with you and gave you a code. The proof then answers that one check.'))),
      h('div', { class: 'callout info' }, icon('info'), h('span', null, 'The proof carries the viewing key of your item account. Whoever checks it can see every item this account has held and when each one moved, and can match it to your shop if you have one. Your ZEC sits in another account and stays private.')),
      err,
    ],
    foot: [
      h('button', {
        class: 'btn primary lg',
        onclick: async () => {
          const c = ch.value.trim();
          const l = label.value.trim();
          if (S.engine && !(await askConfirm(err))) return;
          sh.setTitle('Making the proof');
          await runInSheet(sh, '/api/prove', { note: it.note, challenge: c, label: l }, {
            total: 2,
            success: (r) => {
              const text = JSON.stringify(r.proof, null, 2);
              const box = h('textarea', { class: 'input mono proof', readonly: true, rows: '6', 'aria-label': 'Ownership proof' });
              box.value = text;
              // The same proof as a QR code, for a checker standing next to you.
              const qr = h('div', { class: 'qr big' });
              try {
                qr.append(QR.svg(JSON.stringify(r.proof), { ecl: 'L' }));
                qr.querySelector('svg').setAttribute('aria-label', 'QR code of the proof');
              } catch {
                qr.remove();
              }
              return {
                title: 'Proof ready',
                text: c
                  ? 'Show this code to the checker, or send them the text. It answers their code only.'
                  : 'Share it, post it, or show the code. Anyone can check it for about the next ' + life + ' minutes.',
                extra: [qr, box],
                actions: [
                  navigator.share ? h('button', { class: 'btn', onclick: () => navigator.share({ text }).catch(() => {}) }, icon('share'), 'Share') : null,
                  h('button', { class: 'btn', onclick: () => copy(text, 'Proof copied') }, icon('copy'), 'Copy'),
                  h('button', { class: 'btn primary', onclick: () => sh.close() }, 'Done'),
                ],
              };
            },
            failTitle: 'No proof made',
            doneTitle: 'Ownership proof',
          });
        },
      }, icon('shield'), 'Make the proof'),
    ],
  });
}

function download(name, text) {
  const a = h('a', { href: URL.createObjectURL(new Blob([text], { type: 'application/json' })), download: name });
  document.body.append(a);
  a.click();
  setTimeout(() => {
    URL.revokeObjectURL(a.href);
    a.remove();
  }, 1000);
}

function openCheckProof() {
  const challenge = newChallenge();
  const proof = h('textarea', { class: 'input mono proof', id: 'proof', rows: '6', placeholder: '{ "version": 2, ... }', spellcheck: 'false' });
  const err = h('p', { class: 'small err', role: 'alert' });
  const sh = sheet({
    title: 'Check that someone owns an item',
    body: [
      h('p', { class: 'small muted' }, 'Paste or scan the proof they shared. It counts if it was made in about the last ' + PROOF_LIFE_MIN + ' minutes and the item is still theirs.'),
      h('div', { class: 'field' }, h('label', { for: 'proof' }, 'Their proof'), proof, 'BarcodeDetector' in window ? h('div', { class: 'row', style: null }, h('button', { class: 'btn sm', type: 'button', onclick: () => scanProof(proof) }, icon('camera'), 'Scan their QR code')) : null),
      h(
        'details',
        null,
        h('summary', { class: 'small muted' }, 'Live check (optional)'),
        h('p', { class: 'small muted', style: null }, 'If the holder is with you, give them this code before they make the proof. The proof then answers only this check, so it cannot be one made earlier.'),
        h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Code'), h('span', { class: 'row nowrap' }, h('b', { class: 'mono' }, challenge), h('button', { class: 'linkbtn', onclick: () => copy(challenge, 'Code copied') }, 'Copy')))),
      ),
      err,
    ],
    foot: [
      h('button', {
        class: 'btn primary lg',
        onclick: async () => {
          const text = proof.value.trim();
          if (!text) return (err.textContent = 'Paste the proof, or scan it.');
          // A proof made for a live check carries a code: it is checked against ours. A shared
          // proof carries none and is checked without one.
          let parsed = null;
          try {
            parsed = JSON.parse(text);
          } catch {
            /* the server says what is wrong */
          }
          const body = { proof: text };
          if (parsed && parsed.challenge) body.challenge = challenge;
          sh.setTitle('Checking the proof');
          await runInSheet(sh, '/api/check-proof', body, {
            total: 2,
            doneTitle: 'Proof check',
            success: (r) => {
              const ok = r.valid && r.held;
              const reason = human(r.reason || 'The proof does not hold.');
              const who = r.label ? r.label : 'Whoever made this proof';
              const made = r.age_blocks != null ? r.age_blocks + ' block' + (r.age_blocks === 1 ? '' : 's') + ' ago, about ' + minutesFor(Math.max(1, r.age_blocks)) + ' min' : null;
              const rows = [
                h('div', { class: 'r' }, h('span', null, 'Item note'), h('span', { class: 'mono' }, short(r.note || '', 8))),
                r.label ? h('div', { class: 'r' }, h('span', null, 'Signed name'), h('span', null, r.label)) : null,
                h('div', { class: 'r' }, h('span', null, 'Made after block'), h('span', null, String(r.block_height || '') + (made ? ' (' + made + ')' : ''))),
                h('div', { class: 'r' }, h('span', null, 'Live check'), h('span', null, r.live ? 'Yes, it answers your code' : 'No, a shared proof')),
                h('div', { class: 'r' }, h('span', null, 'Signature and block'), h('span', null, r.valid ? 'Match' : 'Do not match')),
                h('div', { class: 'r' }, h('span', null, 'Item still held'), h('span', null, r.valid ? (r.held ? 'Yes' : 'No') : 'Not checked')),
              ];
              return {
                title: ok ? 'They hold it' : 'Not proven',
                tone: ok ? 'ok' : 'bad',
                text: ok
                  ? who + ' holds ' + (r.name || 'this item') + ' right now (checked up to block ' + r.scanned_to + ').'
                  : reason.charAt(0).toUpperCase() + reason.slice(1),
                extra: h('div', { class: 'summary' }, rows),
              };
            },
            failTitle: 'Could not check',
          });
        },
      }, icon('shield'), 'Check'),
    ],
  });
}

// Read a proof's QR code with the camera (browsers with a barcode detector: Chrome, Edge,
// Samsung Internet). The result goes into the proof field.
async function scanProof(into) {
  const video = h('video', { class: 'scanvid', autoplay: '', playsinline: '', muted: '' });
  video.muted = true;
  let stream = null;
  const stop = () => {
    if (stream) stream.getTracks().forEach((t) => t.stop());
    stream = null;
  };
  const sh = sheet({ title: 'Scan the proof', body: [video, h('p', { class: 'small muted' }, 'Point the camera at the code on the holder’s screen.')], onClose: stop });
  try {
    stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: 'environment' } });
    video.srcObject = stream;
  } catch {
    sh.close();
    return toast('The camera is not available; paste the proof instead', 'err');
  }
  const det = new BarcodeDetector({ formats: ['qr_code'] });
  const tick = async () => {
    if (!stream) return;
    try {
      const codes = await det.detect(video);
      if (codes.length && codes[0].rawValue) {
        into.value = codes[0].rawValue;
        sh.close();
        toast('Proof scanned');
        return;
      }
    } catch {
      /* not ready yet */
    }
    setTimeout(tick, 250);
  };
  video.addEventListener('loadeddata', () => setTimeout(tick, 300), { once: true });
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
    h('div', { class: 'callout info' }, icon('info'), h('span', null, S.status.network === 'test' ? (S.status.faucet ? 'Testnet coins are free: press Get coins on the Wallet tab, or paste this address into a Zcash testnet faucet.' : 'Testnet coins are free: paste this address into a Zcash testnet faucet.') : 'Only shielded payments reach this address. ' + spendAfter())),
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
  async function review() {
    err.textContent = '';
    const a = to.value.trim();
    const amt = amount.value.trim().replace(',', '.');
    const zat = Math.round(Number(amt) * 1e8);
    if (!a.startsWith(prefix)) return (err.textContent = 'This is not a ' + (S.status.network === 'main' ? 'mainnet' : 'testnet') + ' Zcash address. It should start with ' + prefix + '.');
    if (!/^\d*\.?\d{0,8}$/.test(amt) || !(zat > 0)) return (err.textContent = 'Enter an amount in ZEC, for example 0.001.');
    if (zat + FEE_ZAT > bal) return (err.textContent = 'Not enough ZEC for this amount plus the network fee.');
    if (S.engine && !(await S.engine.checkAddress(a).catch(() => false))) return (err.textContent = 'This wallet cannot pay that address. Check it, or ask for a shielded address (a unified address with an Orchard part).');
    sh.setTitle('Confirm payment');
    sh.setBody(
      h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'To'), h('span', { class: 'mono' }, short(a, 10))), h('div', { class: 'r' }, h('span', null, 'Amount'), h('span', null, zec(zat))), h('div', { class: 'r' }, h('span', null, 'Network fee'), h('span', null, 'about ' + zec(FEE_ZAT))), memo.value.trim() ? h('div', { class: 'r' }, h('span', null, 'Message'), h('span', null, memo.value.trim())) : null, h('div', { class: 'r total' }, h('span', null, 'Total'), h('span', null, 'about ' + zec(zat + FEE_ZAT)))),
      spendWarning(zat + FEE_ZAT),
      h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'Check the address. Confirming sends the payment; it cannot be undone.')),
    );
    sh.setFoot(
      h('button', { class: 'btn', onclick: () => { sh.close(); openSend(); } }, 'Back'),
      h('button', {
        class: 'btn primary',
        onclick: async () => {
          const ok = await askConfirm();
          if (!ok) return;
          sh.setTitle('Sending');
          await runInSheet(sh, '/api/pay', { to: a, amount: amt + ' zec', memo: memo.value.trim(), ...ok }, { total: 2, doneTitle: 'Payment', success: (r) => ({ title: 'Sent', text: zec(zat) + ' is on its way. It arrives in about a minute.', extra: txRow(r.txid) }), failTitle: 'Not sent' });
          loadView(true);
        },
      }, confirmLabel('Confirm and send')),
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
    h('p', { class: 'pagesub' }, (S.hosted ? 'Pick a shop, or open one by its address. ' : 'Open a shop by its address. ') + 'A shop’s list of items is signed with its seller’s key; your wallet checks that signature first, so names and prices are what the seller wrote and nobody in between changed them.'),
    // On a hosted server the open shops come first: that is where a new visitor starts.
    S.hosted && openShops.length
      ? h('div', null, h('div', { class: 'section-title' }, 'Shops open now'), h('div', { class: 'card' }, h('div', { class: 'list' }, openShops.map((s) => h('button', { class: 'li', onclick: () => open(s.base) }, shopAvatar(s.base), h('span', { class: 'grow' }, h('div', { class: 'title' }, s.name + (s.base === S.status.public_shop ? ' (yours)' : '')), h('div', { class: 'sub' }, s.items + ' item' + (s.items === 1 ? '' : 's') + ' for sale')), icon('chevron'))))))
      : null,
    S.hosted && openShops.length ? h('div', { class: 'section-title' }, 'Or open a shop by its address') : null,
    h('div', { class: 'card stack' }, h('div', { class: 'field' }, h('label', { for: 'shopurl' }, 'Shop address'), h('div', { class: 'inputwrap' }, input, openBtn)), err, recent.length ? h('div', null, h('div', { class: 'small muted', style: null }, 'Recent shops'), h('div', { class: 'list' }, recent.slice(0, 4).map((s) => h('button', { class: 'li', onclick: () => open(s) }, avatar(s, 'ico'), h('span', { class: 'grow' }, h('div', { class: 'title' }, s.replace(/^https?:\/\//, '')), h('div', { class: 'sub' }, 'Opened before')), icon('chevron'))))) : null),
    S.market ? viewShopFront() : null,
    h('div', { class: 'section-title' }, 'Ownership'),
    h('div', { class: 'card' }, h('div', { class: 'list' }, h('button', { class: 'li', onclick: openCheckProof }, h('span', { class: 'ico' }, icon('shield')), h('span', { class: 'grow' }, h('div', { class: 'title' }, 'Check who owns an item'), h('div', { class: 'sub' }, 'Paste or scan a proof they shared. It shows the item is theirs now; their ZEC and payout address stay hidden.')), icon('chevron')))),
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
      art(it.note, it.name, 'art big', it.icon),
      it.description ? h('p', { class: 'desc' }, it.description) : null,
      h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Price'), h('b', null, zec(it.price_zat))), h('div', { class: 'r' }, h('span', null, 'Seller'), h('span', null, S.market.base.replace(/^https?:\/\//, ''))), h('div', { class: 'r' }, h('span', null, 'Item note'), h('span', { class: 'mono' }, short(it.note, 8)))),
      wrongNet ? h('div', { class: 'callout warn' }, icon('alert'), h('span', null, 'This shop is on another network than your wallet.')) : null,
    ],
    foot: [h('button', { class: 'btn', onclick: () => { sh.close(); openHistory(S.market.base, it.note, it.name); } }, icon('history'), 'History'), S.market.own ? h('span', { class: 'chip' }, 'Your own shop') : h('button', { class: 'btn primary', disabled: wrongNet, onclick: () => buyReview(sh, it) }, icon('bag'), 'Buy')],
  });
}

function buyReview(sh, it) {
  const bal = (S.wallet && S.wallet.balance_zat) || null;
  sh.setTitle('Confirm purchase');
  sh.setBody(
    h('div', { class: 'row nowrap' }, art(it.note, it.name, 'ico', it.icon), h('div', null, h('b', null, it.name || 'Item'), h('div', { class: 'small muted' }, 'from ' + S.market.base.replace(/^https?:\/\//, '')))),
    h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Price'), h('span', null, zec(it.price_zat))), h('div', { class: 'r' }, h('span', null, 'Network fee'), h('span', null, 'about ' + zec(FEE_ZAT))), h('div', { class: 'r total' }, h('span', null, 'Total'), h('span', null, 'about ' + zec(it.price_zat + FEE_ZAT)))),
    bal !== null ? spendWarning(it.price_zat + FEE_ZAT) : null,
    h('div', { class: 'callout' }, icon('shield'), h('span', null, 'Your payment and the item move in one transaction. If someone else buys it first, your ZEC stays with you.')),
    h('p', { class: 'small muted' }, 'Confirming sends the transaction. It cannot be undone.'),
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
        const ok = await askConfirm();
        if (!ok) return;
        sh.setTitle('Buying ' + (it.name || 'item'));
        const r = await runInSheet(sh, '/api/buy', { url: S.market.base, item: it.note, name: it.name, description: it.description, icon: it.icon, ...ok }, {
          total: 5,
          success: (r) => ({
            title: (it.name || 'The item') + ' is yours',
            text: 'Signed by both sides and sent. It shows in your wallet once mined, in about a minute.',
            extra: txRow(r.txid),
            actions: [h('button', { class: 'btn', onclick: () => sh.close() }, 'Keep shopping'), h('button', { class: 'btn primary', onclick: () => { sh.close(); go('wallet'); } }, 'Go to wallet')],
          }),
          failTitle: 'Not bought',
          doneTitle: 'Purchase',
        });
        if (r) {
          try {
            S.market = await api('/api/market', { url: S.market.base });
          } catch {
            /* refreshed next time */
          }
          // The shop drops the item when it next looks at the chain; until then it is gone here.
          if (S.market && S.market.items) S.market.items = S.market.items.filter((x) => x.note !== it.note);
          if (S.view === 'market') loadView(true);
        }
      },
    }, confirmLabel('Confirm and pay')),
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
  const h_ = String(hop.holder || '');
  if (you || h_ === 'you') return 'held by you';
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
        shop.running && S.status.browser
          ? h('p', { class: 'small muted' }, 'Your shop is open while this page is open: it signs offers here, with the key in this browser. Close the page and the shop closes.')
          : null,
        S.hosted ? null : h('details', null, h('summary', { class: 'small muted' }, 'Advanced'), h('div', { class: 'field', style: null }, h('label', { for: 'listen' }, 'Listen on'), h('input', { class: 'input', id: 'listen', value: listen, onchange: (e) => (S.listen = e.target.value) }), h('div', { class: 'hint' }, 'Use 0.0.0.0:8900 to let other machines in. For the internet, put a TLS proxy or a Tor hidden service in front.'))),
      ),
      h(
        'div',
        { class: 'card stack' },
        h('b', null, 'Create items'),
        names,
        h('button', { class: 'btn primary', onclick: () => (names.value.trim() ? mint(names.value) : (names.focus(), toast('Write a name for each item, one per line', 'err'))) }, icon('spark'), 'Create'),
        h('div', { class: 'hint small muted' }, 'Each item is a shielded note of ' + zec(ITEM_ZAT) + ', named in its encrypted memo. Creating costs a network fee of about ' + zec(mintFee(1)) + ', a little more for several at once.'),
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
  const thumb = art(it.note, it.name, 'ico', it.icon);
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

const ITEM_ICONS = ['🎟️', '🎫', '🎵', '🎧', '🎬', '🎨', '🖼️', '📷', '🎮', '🕹️', '📚', '📖', '🏆', '🏅', '⚽', '💎', '🔑', '🎁', '☕', '🍕', '👕', '🧢', '🌟', '🚀'];

function listSheet(it) {
  const price = h('input', { class: 'input', id: 'price', inputmode: 'decimal', placeholder: '0.001', autocomplete: 'off' });
  const desc = h('input', { class: 'input', id: 'desc', maxlength: '280', placeholder: 'Front row, Saturday night', autocomplete: 'off', value: it.description || '' });
  let chosen = it.icon || '';
  const picks = ITEM_ICONS.map((e) => {
    const b = h('button', { type: 'button', class: 'iconpick' + (e === chosen ? ' on' : ''), 'aria-label': 'Icon ' + e, 'aria-pressed': String(e === chosen) }, e);
    b.onclick = () => {
      chosen = chosen === e ? '' : e;
      picks.forEach((x) => {
        const on = x.textContent === chosen;
        x.classList.toggle('on', on);
        x.setAttribute('aria-pressed', String(on));
      });
    };
    return b;
  });
  const err = h('p', { class: 'small err', role: 'alert' });
  const sh = sheet({
    title: 'List ' + (it.name || 'item'),
    body: [
      h('div', { class: 'field' }, h('label', { for: 'price' }, 'Price'), h('div', { class: 'inputwrap' }, price, h('span', { class: 'suffix' }, 'ZEC')), h('div', { class: 'hint' }, 'More than 0.00005 ZEC. The buyer pays the network fee.')),
      h('div', { class: 'field' }, h('label', { for: 'desc' }, 'Description (optional)'), desc),
      h('div', { class: 'field' }, h('div', { class: 'label' }, 'Icon (optional)'), h('div', { class: 'iconpicks', role: 'group', 'aria-label': 'Icon' }, picks)),
      h('p', { class: 'small muted' }, 'The description and icon go into your signed listings file, so buyers see them as you wrote them.'),
      err,
    ],
    foot: [
      h('button', {
        class: 'btn primary lg',
        onclick: async () => {
          const p = price.value.trim().replace(',', '.');
          if (!/^\d*\.?\d{0,8}$/.test(p) || !(Number(p) > 0.00005)) return (err.textContent = 'Enter a price above 0.00005 ZEC.');
          if (S.engine && !(await askConfirm(err))) return;
          sh.setTitle('Listing ' + (it.name || 'item'));
          await runInSheet(sh, '/api/publish', { note: it.note, name: it.name, price: p + ' zec', description: desc.value.trim(), icon: chosen }, { total: 2, doneTitle: 'Listing', success: () => ({ title: 'Listed', text: (it.name || 'The item') + ' is for sale at ' + p + ' ZEC.' + (S.shop && S.shop.running ? '' : ' Open your shop so buyers can reach it.') }), failTitle: 'Not listed' });
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
      h('button', { class: 'btn danger', onclick: () => confirmCancel(sh, it) }, 'Cancel offers'),
      h('button', { class: 'btn', onclick: async () => { if (S.engine && !(await askConfirm())) return; sh.setTitle('Delisting'); await runInSheet(sh, '/api/delist', { note: it.note }, { total: 1, doneTitle: 'Listing', success: () => ({ title: 'Delisted', text: 'The item is no longer for sale.' }), failTitle: 'Not delisted' }); loadView(true); } }, 'Delist'),
    ],
  });
}

// Cancelling is a transaction (the item moves to a fresh note): it is confirmed first.
function confirmCancel(sh, it) {
  sh.setTitle('Confirm cancelling');
  sh.setBody(
    h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Item'), h('b', null, it.name || short(it.note, 8))), h('div', { class: 'r' }, h('span', null, 'Network fee'), h('span', null, 'about ' + zec(mintFee(1))))),
    h('p', { class: 'small muted' }, 'The item moves to a fresh note of yours, so offers your shop already signed can never settle. Confirming sends the transaction; it cannot be undone.'),
  );
  sh.setFoot(
    h('button', { class: 'btn', onclick: () => sh.close() }, 'Back'),
    h('button', { class: 'btn danger', onclick: async () => { const ok = await askConfirm(); if (!ok) return; sh.setTitle('Cancelling offers'); await runInSheet(sh, '/api/cancel', { note: it.note, ...ok }, { total: 1, doneTitle: 'Offers', success: () => ({ title: 'Offers cancelled', text: 'The item moved to a fresh note of yours.' }), failTitle: 'Not cancelled' }); loadView(true); } }, confirmLabel('Confirm and cancel')),
  );
}

// ZIP 317 fee of a transaction with one input and n item outputs plus change.
function mintFee(n) {
  return 5000 * Math.max(2, n + 1);
}

// Creating items is a transaction: a review with the cost comes before anything is sent.
function mint(text) {
  const names = text.split('\n').map((s) => s.trim()).filter(Boolean);
  if (!names.length) return toast('Write at least one name', 'err');
  const n = names.length;
  const sh = sheet({
    title: 'Confirm ' + n + ' new item' + (n > 1 ? 's' : ''),
    body: [
      h('ul', { class: 'small plainlist' }, names.map((x) => h('li', null, x))),
      h('div', { class: 'summary' }, h('div', { class: 'r' }, h('span', null, 'Item notes'), h('span', null, zec(ITEM_ZAT * n))), h('div', { class: 'r' }, h('span', null, 'Network fee'), h('span', null, 'about ' + zec(mintFee(n)))), h('div', { class: 'r total' }, h('span', null, 'Total'), h('span', null, 'about ' + zec(ITEM_ZAT * n + mintFee(n))))),
      spendWarning(ITEM_ZAT * n + mintFee(n)),
      h('p', { class: 'small muted' }, 'Each item becomes a shielded note of ' + zec(ITEM_ZAT) + ' in your wallet, named in its encrypted memo. Confirming sends the transaction; it cannot be undone.'),
    ],
    foot: [
      h('button', { class: 'btn', onclick: () => sh.close() }, 'Cancel'),
      h('button', {
        class: 'btn primary',
        onclick: async () => {
          const ok = await askConfirm();
          if (!ok) return;
          sh.setTitle('Creating ' + n + ' item' + (n > 1 ? 's' : ''));
          await runInSheet(sh, '/api/mint', { names, ...ok }, { total: 2, doneTitle: 'New items', success: (r) => ({ title: 'Created', text: 'They can be listed once the block is mined, in about a minute.', extra: r && r.txid ? txRow(r.txid) : null }), failTitle: 'Not created' });
          loadView(true);
        },
      }, confirmLabel('Confirm and create')),
    ],
  });
}

async function toggleShop(open, sw) {
  sw.disabled = true;
  // The shop signs with the item key held in this page: confirm before it opens.
  if (S.engine && open && !(await askConfirm())) {
    sw.disabled = false;
    return;
  }
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
  prove: ['acc', 'shield'],
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
    h('p', { class: 'pagesub' }, 'Everything this wallet did, newest first.'),
    a.length ? h('div', { class: 'card' }, activityList(a)) : h('div', { class: 'card' }, h('div', { class: 'empty' }, h('div', { class: 'ico' }, icon('clock')), h('h3', null, 'Nothing yet'), h('p', null, 'Receive ZEC, buy an item or create one, and it shows up here.'))),
  );
}

// A new link pasted into this tab changes only the part after '#', which does not reload the
// page: take the new key and start over.
window.addEventListener('popstate', (e) => {
  if (/token=[0-9a-f]{32}/.test(location.hash)) return;
  // Back closes an open sheet first (not one that is busy with a job), then shows the tab.
  const open = document.querySelector('#layer .scrim');
  if (open) {
    const closeBtn = open.querySelector('.iconbtn[aria-label="Close"]');
    if (closeBtn && !closeBtn.hidden) closeBtn.click();
  }
  const v = (e.state && e.state.view) || location.hash.slice(1);
  if (VIEWS.some((x) => x[0] === v) && v !== S.view && S.status && S.status.unlocked) go(v, false);
});
window.addEventListener('hashchange', () => {
  if (/token=[0-9a-f]{32}/.test(location.hash)) location.reload();
});
document.addEventListener('visibilitychange', () => {
  if (!document.hidden && S.status && S.status.unlocked) backgroundSync();
});
boot();
