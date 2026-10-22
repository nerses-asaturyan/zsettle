// zsettle wallet: a Zcash wallet that runs in this page.
//
// The recovery phrase is made here and stays here, encrypted with a key from a passkey or a
// passphrase. The wallet database (notes, history) is kept in this browser too, encrypted with
// the same key. The server that sent this page only relays public chain data (blocks, tree
// states, finished transactions) and passes offers to its shops; it never sees a key.
import init, { initThreadPool } from './pkg/zsettle_web.js';

const $app = document.getElementById('app');
const S = { record: null, key: null, view: null, syncing: null, worker: null, name: null };

// ------------------------------------------------------------ small helpers

function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
    else if (k === 'value') el.value = v;
    else el.setAttribute(k, v === true ? '' : String(v));
  }
  for (const k of kids.flat(Infinity)) {
    if (k === null || k === undefined || k === false) continue;
    el.append(k instanceof Node ? k : document.createTextNode(String(k)));
  }
  return el;
}

function toast(text, kind = 'ok') {
  const t = h('div', { class: 'toast ' + kind }, text);
  document.getElementById('toasts').append(t);
  setTimeout(() => t.remove(), kind === 'err' ? 6000 : 2600);
}

async function copy(text, what = 'Copied') {
  try {
    await navigator.clipboard.writeText(text);
    toast(what);
  } catch {
    toast('Copy failed: select the text instead', 'err');
  }
}

const zec = (zat) => {
  const s = (Number(zat || 0) / 1e8).toFixed(8).replace(/0+$/, '').replace(/\.$/, '');
  return s + ' ZEC';
};
const toZat = (text) => {
  const t = String(text).trim();
  if (!/^\d+(\.\d{1,8})?$/.test(t)) throw new Error('Write the amount like 0.001');
  const [a, b = ''] = t.split('.');
  return Number(BigInt(a) * 100000000n + BigInt((b + '00000000').slice(0, 8)));
};
const short = (s) => (s.length > 20 ? s.slice(0, 10) + '…' + s.slice(-8) : s);

function human(msg) {
  const m = String(msg || 'Something went wrong');
  if (/no note can pay yet|insufficient|not enough|InsufficientFunds/i.test(m))
    return 'Not enough spendable ZEC yet. Money you receive can be spent after 10 blocks, change after 3.';
  if (/would be dust/i.test(m)) return 'This would leave too little change to ever spend. Pick a cheaper item, or add coins first.';
  if (/the shop refused the offer: (.*)/i.test(m)) return 'The shop refused the offer: ' + m.match(/refused the offer: (.*)/i)[1];
  if (/too many requests/i.test(m)) return 'The server is busy. Wait a minute and try again.';
  return m.charAt(0).toUpperCase() + m.slice(1);
}

// ------------------------------------------------------------ sheets

function sheet(title, body, { dismissable = true } = {}) {
  const x = h('button', { class: 'x', 'aria-label': 'Close' }, '×');
  const bodyEl = h('div', { class: 'stack' }, body);
  const box = h('div', { class: 'sheet', role: 'dialog', 'aria-modal': 'true' }, h('div', { class: 'head' }, h('h2', null, title), x), bodyEl);
  const scrim = h('div', { class: 'scrim' }, box);
  let canClose = dismissable;
  const api = {
    close() {
      scrim.remove();
    },
    set(...kids) {
      bodyEl.replaceChildren(...kids.flat().filter(Boolean));
    },
    lock(v) {
      canClose = !v;
      x.hidden = v;
    },
  };
  x.onclick = () => canClose && api.close();
  scrim.addEventListener('click', (e) => e.target === scrim && canClose && api.close());
  api.lock(!dismissable);
  document.getElementById('layer').append(scrim);
  return api;
}

// ------------------------------------------------------------ the wallet engine

let nextId = 1;
const pending = new Map();
let queue = Promise.resolve();

// Calls run one after another: the wallet database has one writer.
function call(op, args = [], onProgress) {
  const run = () =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject, onProgress });
      S.worker.postMessage({ id, op, args });
    });
  const p = queue.then(run, run);
  queue = p.catch(() => {});
  return p;
}

async function startEngine() {
  if (!self.crossOriginIsolated) throw new Error('This browser cannot run the wallet here (the page is not isolated). Try a current Chrome, Safari, Firefox or Edge.');
  const module = await WebAssembly.compileStreaming(fetch('./pkg/zsettle_web_bg.wasm'));
  const exports = await init({ module_or_path: module });
  const threads = Math.max(2, Math.min(navigator.hardwareConcurrency || 4, 8));
  await initThreadPool(threads);
  // The wallet's worker, and a helper that builds the proving key in the background: they
  // share the module and the memory, so the key the helper builds is the one trades use,
  // and the wallet stays free for the visitor meanwhile.
  S.worker = await spawn(module, exports.memory);
  S.helper = await spawn(module, exports.memory);
}

function spawn(module, memory) {
  const worker = new Worker('./worker.js', { type: 'module' });
  return new Promise((resolve, reject) => {
    worker.onmessage = ({ data }) => {
      if (data.type === 'ready') return resolve(worker);
      if (data.type === 'failed') return reject(new Error(data.err));
      if (data.type === 'crash') {
        // The worker cannot go on after a panic: fail what is in flight and say so.
        console.warn('wallet worker:', data.err);
        for (const [id, p] of pending) {
          pending.delete(id);
          p.reject(new Error('The wallet stopped: ' + data.err.split('\n')[0] + ' Reload the page.'));
        }
        return;
      }
      const p = pending.get(data.id);
      if (!p) return;
      if ('progress' in data) return p.onProgress && p.onProgress(data.progress);
      pending.delete(data.id);
      if ('err' in data) p.reject(new Error(data.err));
      else p.resolve(data.ok);
    };
    worker.onerror = (e) => reject(new Error(e.message || 'the wallet worker failed'));
    worker.postMessage({ type: 'init', module, memory });
  });
}

// Build the proving key on the helper, once; nothing waits for it.
function warmUp() {
  if (S.warm) return;
  S.warm = true;
  const id = nextId++;
  pending.set(id, { resolve() {}, reject() {} });
  S.helper.postMessage({ id, op: 'warmUp', args: [] });
}

// ------------------------------------------------------------ storage and keys

const DB_NAME = 'zsettle-wallet';
function idb() {
  return new Promise((resolve, reject) => {
    const r = indexedDB.open(DB_NAME, 1);
    r.onupgradeneeded = () => r.result.createObjectStore('wallets');
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}
async function idbGet(key) {
  const db = await idb();
  return new Promise((resolve, reject) => {
    const r = db.transaction('wallets').objectStore('wallets').get(key);
    r.onsuccess = () => resolve(r.result || null);
    r.onerror = () => reject(r.error);
  });
}
async function idbPut(key, value) {
  const db = await idb();
  return new Promise((resolve, reject) => {
    const t = db.transaction('wallets', 'readwrite');
    t.objectStore('wallets').put(value, key);
    t.oncomplete = () => resolve();
    t.onerror = () => reject(t.error);
  });
}
async function idbDelete(key) {
  const db = await idb();
  return new Promise((resolve, reject) => {
    const t = db.transaction('wallets', 'readwrite');
    t.objectStore('wallets').delete(key);
    t.oncomplete = () => resolve();
    t.onerror = () => reject(t.error);
  });
}
// A wallet per network; the one last opened comes first.
const KEY = (network) => 'wallet-' + network;
async function storedWallets() {
  const out = [];
  for (const n of ['test', 'main']) {
    const r = await idbGet(KEY(n));
    if (r) out.push(r);
  }
  return out.sort((a, b) => (b.opened || 0) - (a.opened || 0));
}

const enc = new TextEncoder();
const dec = new TextDecoder();
const b64u = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const unb64u = (s) => {
  const p = s.replace(/-/g, '+').replace(/_/g, '/');
  return Uint8Array.from(atob(p + '==='.slice((p.length + 3) % 4)), (c) => c.charCodeAt(0));
};

const PBKDF2_ROUNDS = 600000;
async function keyFromPassphrase(pass, salt) {
  const base = await crypto.subtle.importKey('raw', enc.encode(pass), 'PBKDF2', false, ['deriveKey']);
  return crypto.subtle.deriveKey({ name: 'PBKDF2', salt, iterations: PBKDF2_ROUNDS, hash: 'SHA-256' }, base, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
}
async function keyFromSecret(secret) {
  const base = await crypto.subtle.importKey('raw', secret, 'HKDF', false, ['deriveKey']);
  return crypto.subtle.deriveKey({ name: 'HKDF', hash: 'SHA-256', salt: enc.encode('zsettle web wallet'), info: enc.encode('wallet key v1') }, base, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
}
async function seal(key, bytes) {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = await crypto.subtle.encrypt({ name: 'AES-GCM', iv }, key, bytes);
  return { iv: b64u(iv), ct: new Uint8Array(ct) };
}
async function open_(key, box) {
  const pt = await crypto.subtle.decrypt({ name: 'AES-GCM', iv: unb64u(box.iv) }, key, box.ct);
  return new Uint8Array(pt);
}

// Passkeys: the PRF output, never stored, is the key's secret.
const NO_PRF = 'This passkey cannot derive a key. Use a passphrase, or a passkey saved in Chrome, Edge, Safari or a recent Firefox.';
const passkeyUsable = () => !!window.PublicKeyCredential && window.isSecureContext;

async function passkeyCreate(network) {
  const salt = crypto.getRandomValues(new Uint8Array(32));
  const cred = await navigator.credentials.create({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rp: { name: 'zsettle', id: location.hostname },
      user: { id: crypto.getRandomValues(new Uint8Array(16)), name: 'zsettle wallet (' + network + ')', displayName: 'zsettle wallet (' + network + 'net)' },
      pubKeyCredParams: [
        { type: 'public-key', alg: -7 },
        { type: 'public-key', alg: -257 },
      ],
      authenticatorSelection: { residentKey: 'preferred', userVerification: 'required' },
      extensions: { prf: { eval: { first: salt } } },
    },
  });
  const ext = cred.getClientExtensionResults().prf || {};
  const lock = { kind: 'passkey', id: b64u(cred.rawId), salt: b64u(salt) };
  let secret = ext.results && ext.results.first;
  if (!secret) {
    if (ext.enabled === false) throw new Error(NO_PRF);
    secret = await passkeySecret(lock);
  }
  return { lock, key: await keyFromSecret(secret) };
}
async function passkeySecret(lock) {
  const a = await navigator.credentials.get({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rpId: location.hostname,
      allowCredentials: [{ type: 'public-key', id: unb64u(lock.id) }],
      userVerification: 'required',
      extensions: { prf: { eval: { first: unb64u(lock.salt) } } },
    },
  });
  const r = a.getClientExtensionResults().prf;
  if (!r || !r.results || !r.results.first) throw new Error(NO_PRF);
  return r.results.first;
}

// The key of the stored wallet, from its passkey or passphrase.
async function unlockKey(record, passphrase) {
  if (record.lock.kind === 'passkey') return keyFromSecret(await passkeySecret(record.lock));
  return keyFromPassphrase(passphrase, unb64u(record.lock.salt));
}

// The recovery phrase, for one signing call. A passkey wallet asks the passkey again, so every
// payment needs the owner's touch; a passphrase wallet uses the key it was unlocked with, after
// the confirm step.
async function phraseForSigning() {
  const key = S.record.lock.kind === 'passkey' ? await unlockKey(S.record) : S.key;
  return dec.decode(await open_(key, S.record.phrase));
}

async function saveDb() {
  const bytes = await call('save');
  S.record.db = await seal(S.key, bytes);
  S.record.opened = Date.now();
  await idbPut(KEY(S.record.network), S.record);
}

// ------------------------------------------------------------ screens

function render(...kids) {
  $app.replaceChildren(...kids.flat().filter(Boolean));
}

async function welcome() {
  const wallets = await storedWallets();
  if (wallets.length) return lockScreen(wallets[0], wallets);
  render(
    h('div', { class: 'top' }, h('h1', { class: 'grow' }, 'zsettle wallet')),
    h('div', { class: 'card stack' },
      h('p', null, 'A Zcash wallet that runs in this page. Your recovery phrase is made here and never leaves this device; the server only passes public blocks along.'),
      h('p', { class: 'muted small' }, 'Buy items for ZEC in one shielded transaction: the item and the payment move together, or not at all.'),
    ),
    h('div', { class: 'stack' },
      h('button', { class: 'btn primary block', onclick: () => createFlow() }, 'Create a wallet'),
      h('button', { class: 'btn block', onclick: () => restoreFlow() }, 'Restore from a recovery phrase'),
    ),
  );
}

function networkPicker() {
  const sel = h('select', { id: 'net' }, h('option', { value: 'test' }, 'Testnet (test coins, free)'), h('option', { value: 'main' }, 'Mainnet (real ZEC)'));
  const warn = h('p', { class: 'note', hidden: true }, 'Mainnet uses real money. Write the recovery phrase on paper: if this browser loses its data, the phrase is the only way back.');
  sel.onchange = () => (warn.hidden = sel.value !== 'main');
  return { el: h('div', { class: 'stack' }, h('label', { for: 'net' }, 'Network'), sel, warn), get: () => sel.value };
}

async function createFlow() {
  const phrase = await call('newMnemonic');
  const net = networkPicker();
  const words = phrase.split(' ');
  const ok = h('input', { type: 'checkbox', id: 'wrote' });
  const next = h('button', { class: 'btn primary block', disabled: true }, 'Next');
  ok.onchange = () => (next.disabled = !ok.checked);
  const sh = sheet('New wallet', [
    net.el,
    h('h3', null, 'Your recovery phrase'),
    h('ol', { class: 'words' }, words.map((w) => h('li', null, w))),
    h('p', { class: 'note' }, 'Write these 24 words on paper, in order. Anyone with them can spend this wallet; without them, a lost browser means lost funds.'),
    h('label', { class: 'check' }, ok, 'I wrote the 24 words down.'),
    next,
  ]);
  next.onclick = () => protectFlow(sh, phrase, net.get(), 0);
}

async function restoreFlow() {
  const net = networkPicker();
  const text = h('textarea', { id: 'phrase', autocomplete: 'off', autocapitalize: 'none', spellcheck: 'false', placeholder: '24 words, separated by spaces' });
  const bday = h('input', { id: 'bday', inputmode: 'numeric', placeholder: 'optional' });
  const err = h('p', { class: 'note err', hidden: true });
  const next = h('button', { class: 'btn primary block' }, 'Next');
  const sh = sheet('Restore a wallet', [
    net.el,
    h('label', { for: 'phrase' }, 'Recovery phrase'),
    text,
    h('label', { for: 'bday' }, 'Block height the wallet was made at'),
    bday,
    h('p', { class: 'muted small' }, 'Leave it empty for a wallet made in the last day. An older wallet needs its height, or it misses its first payments.'),
    err,
    next,
  ]);
  next.onclick = async () => {
    const phrase = text.value.trim().toLowerCase().split(/\s+/).join(' ');
    try {
      await call('checkMnemonic', [phrase]);
      const b = bday.value.trim() ? Number(bday.value.trim()) : 0;
      if (!Number.isInteger(b) || b < 0) throw new Error('The height is a whole number');
      protectFlow(sh, phrase, net.get(), b);
    } catch (e) {
      err.textContent = human(e.message);
      err.hidden = false;
    }
  };
}

function protectFlow(sh, phrase, network, birthday) {
  const pass = h('input', { type: 'password', id: 'newpass', autocomplete: 'new-password' });
  const pass2 = h('input', { type: 'password', id: 'newpass2', autocomplete: 'new-password' });
  const err = h('p', { class: 'note err', hidden: true });
  const usePasskey = h('button', { class: 'btn primary block', hidden: !passkeyUsable() }, 'Lock with a passkey');
  const usePass = h('button', { class: 'btn block' }, 'Lock with a passphrase');
  sh.set(
    h('p', null, 'How should this browser lock the wallet? You unlock it the same way each visit, and confirm each payment.'),
    usePasskey,
    h('h3', null, 'Or a passphrase'),
    h('label', { for: 'newpass' }, 'Passphrase (12 characters or more)'),
    pass,
    h('label', { for: 'newpass2' }, 'Repeat it'),
    pass2,
    err,
    usePass,
  );
  const go = async (getLock) => {
    err.hidden = true;
    sh.lock(true);
    try {
      const { lock, key } = await getLock();
      sh.set(h('p', null, 'Creating the wallet…'), h('p', { class: 'muted small' }, 'This fetches the chain state for its first block.'));
      const name = 'w' + b64u(crypto.getRandomValues(new Uint8Array(6)));
      await call('create', [name, network, phrase, birthday]);
      const view = await call('view');
      S.key = key;
      S.record = { v: 1, name, network, birthday: view.birthday, lock, phrase: await seal(key, enc.encode(phrase)), db: null };
      await saveDb();
      sh.close();
      home();
      sync();
    } catch (e) {
      sh.lock(false);
      err.textContent = human(e.name === 'NotAllowedError' ? 'The passkey prompt was cancelled.' : e.message);
      err.hidden = false;
    }
  };
  usePasskey.onclick = () => go(() => passkeyCreate(network));
  usePass.onclick = () =>
    go(async () => {
      if (pass.value.length < 12) throw new Error('Use 12 characters or more');
      if (pass.value !== pass2.value) throw new Error('The two passphrases differ');
      const salt = crypto.getRandomValues(new Uint8Array(16));
      return { lock: { kind: 'passphrase', salt: b64u(salt) }, key: await keyFromPassphrase(pass.value, salt) };
    });
}

function lockScreen(record, all) {
  const err = h('p', { class: 'note err', hidden: true });
  const pass = h('input', { type: 'password', id: 'pass', autocomplete: 'current-password' });
  const btn = h('button', { class: 'btn primary block' }, record.lock.kind === 'passkey' ? 'Unlock with passkey' : 'Unlock');
  const other = all.filter((w) => w !== record);
  btn.onclick = async () => {
    err.hidden = true;
    btn.disabled = true;
    try {
      const key = await unlockKey(record, pass.value);
      let db;
      try {
        db = await open_(key, record.db);
      } catch {
        throw new Error(record.lock.kind === 'passkey' ? 'This passkey does not open this wallet.' : 'Wrong passphrase.');
      }
      await call('open', [record.name, record.network, record.birthday, db]);
      S.key = key;
      S.record = record;
      home();
      sync();
    } catch (e) {
      err.textContent = human(e.name === 'NotAllowedError' ? 'The passkey prompt was cancelled.' : e.message);
      err.hidden = false;
      btn.disabled = false;
    }
  };
  render(
    h('div', { class: 'top' }, h('h1', { class: 'grow' }, 'zsettle wallet'), netChip(record.network)),
    h('div', { class: 'card stack' },
      h('p', null, 'This browser holds a ' + (record.network === 'main' ? 'mainnet' : 'testnet') + ' wallet.'),
      record.lock.kind === 'passphrase' ? [h('label', { for: 'pass' }, 'Passphrase'), pass] : null,
      err,
      btn,
    ),
    other.length ? h('button', { class: 'btn link', onclick: () => lockScreen(other[0], all) }, 'Open the ' + (other[0].network === 'main' ? 'mainnet' : 'testnet') + ' wallet instead') : null,
    all.length < 2 ? h('button', { class: 'btn link', onclick: () => newBesides(record) }, 'Add a ' + (record.network === 'main' ? 'testnet' : 'mainnet') + ' wallet') : null,
  );
  if (record.lock.kind === 'passphrase') pass.onkeydown = (e) => e.key === 'Enter' && btn.click();
}

function newBesides(record) {
  const sh = sheet('Another wallet', [
    h('p', null, 'This browser keeps one wallet per network. Create or restore the ' + (record.network === 'main' ? 'testnet' : 'mainnet') + ' one.'),
    h('button', { class: 'btn primary block', onclick: () => (sh.close(), createFlow()) }, 'Create'),
    h('button', { class: 'btn block', onclick: () => (sh.close(), restoreFlow()) }, 'Restore'),
  ]);
}

const netChip = (n) => h('span', { class: 'chip ' + n }, n === 'main' ? 'Mainnet' : 'Testnet');

// ------------------------------------------------------------ home

function home() {
  const v = S.view;
  const syncLine = h('p', { class: 'muted small', id: 'syncline' }, syncText());
  const bar = h('div', { class: 'progress', id: 'syncbar', hidden: !S.syncing }, h('div', { style: 'width:' + (S.syncing ? S.syncing.pct : 0) + '%' }));
  const items = v ? v.items : [];
  render(
    h('div', { class: 'top' }, h('h1', { class: 'grow' }, 'zsettle wallet'), netChip(S.record.network), h('button', { class: 'btn link', onclick: lockNow }, 'Lock')),
    h('div', { class: 'card' },
      h('div', { class: 'muted small' }, 'Balance'),
      h('div', { class: 'balance' }, v ? zec(v.balance_zat) : '…'),
      v && pendingNote(v),
      bar,
      syncLine,
    ),
    h('div', { class: 'row' },
      h('button', { class: 'btn', onclick: receive }, 'Receive'),
      h('button', { class: 'btn', onclick: send }, 'Send'),
      h('button', { class: 'btn primary', onclick: shops }, 'Shops'),
    ),
    h('h3', null, 'Your items'),
    h('div', { class: 'card' },
      items.length
        ? h('ul', { class: 'list' }, items.map((it) =>
            h('li', null,
              h('span', { class: 'icon' }, it.icon || '🎟️'),
              h('div', { class: 'grow' }, h('div', { class: 'name' }, it.name || 'Item'), h('div', { class: 'muted small' }, it.mined ? 'In your wallet' : 'Arriving (waiting for a block)')),
              it.mined ? h('button', { class: 'btn', onclick: () => prove(it) }, 'Prove') : null,
            )))
        : h('p', { class: 'muted' }, 'No items yet. Buy one in a shop.'),
    ),
    h('h3', null, 'This wallet'),
    h('div', { class: 'card stack' },
      h('p', { class: 'small muted' }, 'Keys stay in this browser, encrypted. The server relays public chain data only.'),
      h('button', { class: 'btn block', onclick: showPhrase }, 'Show recovery phrase'),
      h('button', { class: 'btn block danger', onclick: removeWallet }, 'Remove from this browser'),
    ),
  );
}

function pendingNote(v) {
  const waiting = v.notes.filter((n) => !n.mined || n.reserved);
  if (!waiting.length) return null;
  const sum = waiting.reduce((a, n) => a + n.value_zat, 0);
  return h('div', { class: 'muted small' }, zec(sum) + ' on its way or held for an offer');
}

function syncText() {
  if (S.syncing) return S.syncing.text;
  if (S.syncError) return 'Sync failed: ' + human(S.syncError) + ' It tries again in a minute.';
  const v = S.view;
  if (!v || !v.scanned_to) return 'Not synced yet';
  return 'Synced to block ' + v.scanned_to.toLocaleString('en');
}

function updateSyncUi() {
  const line = document.getElementById('syncline');
  const bar = document.getElementById('syncbar');
  if (line) line.textContent = syncText();
  if (bar) {
    bar.hidden = !S.syncing;
    bar.firstChild.style.width = (S.syncing ? S.syncing.pct : 0) + '%';
  }
}

let syncTimer = null;
async function sync() {
  if (S.syncing || !S.record) return;
  S.syncing = { text: 'Syncing…', pct: 2 };
  S.syncError = null;
  updateSyncUi();
  const from = S.view && S.view.scanned_to;
  try {
    await call('sync', [], (p) => {
      if (p && p.scanned && p.tip) {
        const start = from || p.scanned;
        const pct = p.tip > start ? Math.round((100 * (p.scanned - start)) / (p.tip - start)) : 100;
        S.syncing = { text: 'Syncing: block ' + p.scanned.toLocaleString('en') + ' of ' + p.tip.toLocaleString('en'), pct: Math.max(2, Math.min(100, pct)) };
        updateSyncUi();
      }
    });
    S.view = await call('view');
    await saveDb();
    warmUp();
  } catch (e) {
    S.syncError = e.message;
  } finally {
    S.syncing = null;
    if (S.record && !document.querySelector('.scrim')) home();
    else updateSyncUi();
    clearTimeout(syncTimer);
    syncTimer = setTimeout(sync, 60000);
  }
}

async function refresh() {
  S.view = await call('view');
  await saveDb();
  if (!document.querySelector('.scrim')) home();
}

async function lockNow() {
  clearTimeout(syncTimer);
  S.key = null;
  S.record = null;
  S.view = null;
  await welcome();
  // The wallet leaves the worker's memory after whatever call is running.
  call('close').catch(() => {});
}

// Confirm a transaction. A passkey wallet asks for the passkey here.
async function confirmStep(sh, lines, label) {
  return new Promise((resolve) => {
    const go = h('button', { class: 'btn primary block' }, S.record.lock.kind === 'passkey' ? label + ' with passkey' : label);
    const back = h('button', { class: 'btn block', onclick: () => resolve(null) }, 'Back');
    const err = h('p', { class: 'note err', hidden: true });
    sh.set(h('div', { class: 'card stack' }, lines), err, go, back);
    go.onclick = async () => {
      go.disabled = true;
      try {
        resolve(await phraseForSigning());
      } catch (e) {
        err.textContent = human(e.name === 'NotAllowedError' ? 'The passkey prompt was cancelled.' : e.message);
        err.hidden = false;
        go.disabled = false;
      }
    };
  });
}

// ------------------------------------------------------------ receive, send

async function receive() {
  const addr = await call('address');
  await saveDb();
  const qr = h('div', { class: 'qr' });
  try {
    qr.append(window.QR.svg(addr));
  } catch {
    qr.hidden = true;
  }
  sheet('Receive', [
    qr,
    h('p', { class: 'mono' }, addr),
    h('button', { class: 'btn primary block', onclick: () => copy(addr, 'Address copied') }, 'Copy address'),
    h('p', { class: 'muted small' }, 'A fresh address each time, so payers cannot link your payments. Money can be spent after 10 blocks.'),
    S.record.network === 'test' ? faucetButton(addr) : null,
  ]);
}

// Free test coins from this server's faucet, once per address.
function faucetButton(addr) {
  const note = h('p', { class: 'muted small' }, 'Test coins have no value. This server gives some once to each new wallet.');
  const b = h('button', { class: 'btn block' }, 'Get free test coins');
  b.onclick = async () => {
    b.disabled = true;
    try {
      const r = await fetch('/api/wallet-faucet', { method: 'POST', headers: { 'X-Zsettle': '1', 'Content-Type': 'application/json' }, body: JSON.stringify({ address: addr }) });
      const j = await r.json();
      if (!r.ok) throw new Error(j.error || 'the faucet did not answer');
      note.textContent = zec(j.amount_zat) + ' is on its way. It shows after the next sync and can be spent after 10 blocks.';
    } catch (e) {
      note.textContent = human(e.message);
      b.disabled = false;
    }
  };
  return h('div', { class: 'stack' }, b, note);
}

function send() {
  const to = h('input', { id: 'to', autocomplete: 'off', autocapitalize: 'none', spellcheck: 'false', placeholder: 'u1… or utest1…' });
  const amount = h('input', { id: 'amt', inputmode: 'decimal', placeholder: '0.001' });
  const memo = h('input', { id: 'memo', maxlength: '200', placeholder: 'optional' });
  const err = h('p', { class: 'note err', hidden: true });
  const next = h('button', { class: 'btn primary block' }, 'Next');
  const sh = sheet('Send', [h('label', { for: 'to' }, 'To (unified address with an Orchard receiver)'), to, h('label', { for: 'amt' }, 'Amount in ZEC'), amount, h('label', { for: 'memo' }, 'Memo'), memo, err, next]);
  next.onclick = async () => {
    let zat;
    try {
      zat = toZat(amount.value);
      if (!to.value.trim()) throw new Error('Paste the address to pay');
    } catch (e) {
      err.textContent = e.message;
      err.hidden = false;
      return;
    }
    const phrase = await confirmStep(sh, [h('div', null, 'Send ', h('b', null, zec(zat))), h('div', { class: 'mono' }, to.value.trim()), h('div', { class: 'muted small' }, 'Plus a network fee of about 0.0001 ZEC.')], 'Send');
    if (!phrase) return send();
    sh.lock(true);
    sh.set(h('p', null, 'Building and proving the payment…'), h('p', { class: 'muted small' }, 'This takes up to a minute on a phone. Keep this page open.'));
    try {
      const r = await call('pay', [phrase, to.value.trim(), zat, memo.value.trim()]);
      await refresh();
      sh.lock(false);
      sh.set(h('p', null, 'Sent. It arrives with the next block.'), h('p', { class: 'mono' }, r.txid), h('button', { class: 'btn block', onclick: () => sh.close() }, 'Done'));
    } catch (e) {
      sh.lock(false);
      sh.set(h('p', { class: 'note err' }, human(e.message)), h('button', { class: 'btn block', onclick: () => sh.close() }, 'Close'));
    }
  };
}

// ------------------------------------------------------------ shops and buying

async function shops() {
  const sh = sheet('Shops', [h('p', { class: 'muted' }, 'Loading…')]);
  try {
    const r = await fetch('/api/shops', { headers: { 'X-Zsettle': '1' } });
    const list = r.ok ? await r.json() : null;
    // A server without the shop list still has its house shop.
    const all = Array.isArray(list) ? list : [{ code: 'house', name: 'House shop', house: true, items: 1 }];
    const open = all.filter((s) => s.items > 0);
    sh.set(
      open.length
        ? h('ul', { class: 'list' }, open.map((s) =>
            h('li', null, h('span', { class: 'icon' }, s.house ? '🏛️' : '🏪'), h('div', { class: 'grow' }, h('div', { class: 'name' }, s.name || s.code), Array.isArray(list) ? h('div', { class: 'muted small' }, s.items + ' item' + (s.items === 1 ? '' : 's')) : null), h('button', { class: 'btn', onclick: () => (sh.close(), shop(s)) }, 'Open'))))
        : h('p', { class: 'muted' }, 'No shop is open right now.'),
      S.record.network === 'main' ? h('p', { class: 'note' }, 'These shops sell on the testnet; a mainnet wallet cannot buy there.') : null,
    );
  } catch (e) {
    sh.set(h('p', { class: 'note err' }, human(e.message)));
  }
}

async function shop(s) {
  const path = '/shop/' + s.code;
  const sh = sheet(s.name || s.code, [h('p', { class: 'muted' }, 'Loading…')]);
  try {
    const r = await fetch(path + '/listings');
    if (!r.ok) throw new Error('the shop is closed');
    const l = await r.json();
    const items = (l.items || []).slice().sort((a, b) => a.price_zat - b.price_zat);
    // Take the shop's item key now and scan for its items in the background, so a purchase
    // does not wait for that scan.
    if (l.network === S.record.network && items.length) {
      call('prepareShop', [path])
        .then(() => sync())
        .catch(() => {});
    }
    sh.set(
      l.network !== S.record.network ? h('p', { class: 'note' }, 'This shop sells on the ' + (l.network === 'main' ? 'mainnet' : 'testnet') + '; this wallet is on the other network.') : null,
      h('ul', { class: 'list' }, items.map((it) =>
        h('li', null,
          h('span', { class: 'icon' }, it.icon || '🎟️'),
          h('div', { class: 'grow' }, h('div', { class: 'name' }, it.name || 'Item'), it.description ? h('div', { class: 'muted small' }, it.description) : null, h('div', { class: 'small' }, zec(it.price_zat))),
          l.network === S.record.network ? h('button', { class: 'btn primary', onclick: () => (sh.close(), buy(path, it)) }, 'Buy') : null,
        ))),
    );
  } catch (e) {
    sh.set(h('p', { class: 'note err' }, human(e.message)));
  }
}

const STEPS = [
  ['listings', 'Checking the shop’s signed price list'],
  ['import', 'Adding the shop’s item key'],
  ['sync', 'Finding the item on chain'],
  ['prove', 'Building and proving the trade'],
  ['offer', 'Handing the shop its copy'],
  ['wait', 'Waiting for the shop’s signature'],
  ['sign', 'Signing your payment'],
  ['send', 'Sending the transaction'],
];

async function buy(path, it) {
  const sh = sheet('Buy ' + (it.name || 'item'), []);
  const phrase = await confirmStep(
    sh,
    [
      h('div', { class: 'name' }, (it.icon || '🎟️') + ' ' + (it.name || 'Item')),
      h('div', null, 'Price ', h('b', null, zec(it.price_zat)), ', plus a network fee of about 0.00015 ZEC.'),
      h('p', { class: 'muted small' }, 'The item and your payment move in one transaction: either both happen or neither does. Your payment is signed here, in this page.'),
    ],
    'Buy',
  );
  if (!phrase) return sh.close();
  sh.lock(true);
  const list = h('ul', { class: 'steps' }, STEPS.map(([k, t]) => h('li', { 'data-k': k }, t)));
  const note = h('p', { class: 'muted small' }, 'Keep this page open. Proving takes up to a minute or two on a phone.');
  sh.set(list, note);
  let seen = [];
  const mark = (k) => {
    seen.push(k);
    for (const li of list.children) {
      const key = li.getAttribute('data-k');
      li.className = key === k ? 'now' : seen.includes(key) ? 'done' : '';
    }
  };
  try {
    const syncLi = list.querySelector('[data-k="sync"]');
    const r = await call('buy', [phrase, path, it.note], (p) => {
      if (p && p.step) mark(p.step);
      else if (p && p.scanned && p.tip && syncLi) syncLi.textContent = 'Finding the item on chain: block ' + p.scanned.toLocaleString('en') + ' of ' + p.tip.toLocaleString('en');
    });
    await refresh();
    sh.lock(false);
    sh.set(h('p', null, 'Bought ' + (r.name || 'the item') + '. It arrives with the next block.'), h('p', { class: 'mono' }, r.txid), h('button', { class: 'btn primary block', onclick: () => sh.close() }, 'Done'));
  } catch (e) {
    sh.lock(false);
    sh.set(h('p', { class: 'note err' }, human(e.message)), h('button', { class: 'btn block', onclick: () => sh.close() }, 'Close'));
    refresh().catch(() => {});
  }
}

// ------------------------------------------------------------ ownership proofs

function prove(it) {
  const label = h('input', { id: 'label', maxlength: '80', placeholder: 'optional, for example @yourname' });
  const challenge = h('input', { id: 'challenge', maxlength: '200', placeholder: 'only if the checker gave you one' });
  const go = h('button', { class: 'btn primary block' }, 'Make the proof');
  const sh = sheet('Prove you hold ' + (it.name || 'this item'), [
    h('p', { class: 'muted small' }, 'The proof shows you hold this item now: it names a recent block, so it expires after about 50 minutes. It reveals this item key’s history, nothing about your payments.'),
    h('label', { for: 'label' }, 'Label'),
    label,
    h('label', { for: 'challenge' }, 'Live check code'),
    challenge,
    go,
  ]);
  go.onclick = async () => {
    const phrase = await confirmStep(sh, [h('div', null, 'Sign an ownership proof for ', h('b', null, it.name || 'this item'), '.')], 'Sign');
    if (!phrase) return sh.close();
    sh.lock(true);
    sh.set(h('p', null, 'Syncing, then signing…'));
    try {
      await sync();
      const proof = await call('prove', [phrase, it.note, label.value.trim(), challenge.value.trim()]);
      const qr = h('div', { class: 'qr' });
      try {
        qr.append(window.QR.svg(proof, { ecl: 'L' }));
      } catch {
        qr.hidden = true;
      }
      sh.lock(false);
      sh.set(
        qr,
        h('button', { class: 'btn primary block', onclick: () => copy(proof, 'Proof copied') }, 'Copy proof'),
        navigator.share ? h('button', { class: 'btn block', onclick: () => navigator.share({ text: proof }).catch(() => {}) }, 'Share') : null,
        h('p', { class: 'mono small' }, proof),
      );
    } catch (e) {
      sh.lock(false);
      sh.set(h('p', { class: 'note err' }, human(e.message)), h('button', { class: 'btn block', onclick: () => sh.close() }, 'Close'));
    }
  };
}

// ------------------------------------------------------------ backup, remove

async function showPhrase() {
  const sh = sheet('Recovery phrase', []);
  const phrase = await confirmStep(sh, [h('p', null, 'Show the 24 words on screen. Make sure nobody can see it.')], 'Show');
  if (!phrase) return sh.close();
  sh.set(h('ol', { class: 'words' }, phrase.split(' ').map((w) => h('li', null, w))), h('p', { class: 'muted small' }, 'Birthday (first block): ' + (S.record.birthday || 0).toLocaleString('en')), h('button', { class: 'btn block', onclick: () => sh.close() }, 'Done'));
}

function removeWallet() {
  const ok = h('input', { type: 'checkbox', id: 'sure' });
  const go = h('button', { class: 'btn danger block', disabled: true }, 'Remove');
  ok.onchange = () => (go.disabled = !ok.checked);
  const sh = sheet('Remove from this browser', [
    h('p', null, 'This deletes the wallet from this browser. The money stays on chain; only the recovery phrase brings it back.'),
    h('label', { class: 'check' }, ok, 'I have the 24 words written down.'),
    go,
  ]);
  go.onclick = async () => {
    await idbDelete(KEY(S.record.network));
    sh.close();
    lockNow();
  };
}

// ------------------------------------------------------------ start

(async () => {
  try {
    await startEngine();
    await welcome();
  } catch (e) {
    render(h('div', { class: 'card stack' }, h('p', { class: 'note err' }, human(e.message))));
  }
})();
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible' && S.record && !S.syncing) sync();
});
