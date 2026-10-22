// zsettle in the browser: the wallet behind the app, with the keys on this device.
//
// The app (app.js) asks for the same /api/... calls it always made; on a site that runs the
// wallet in the browser, this module answers them here. The recovery phrase is made here and
// stays here, encrypted with a key from a passkey or a passphrase, like the wallet database.
// The server relays public chain data (/api/chain/...) and passes offers to shops (/shop/...);
// it never sees a key.
import init, { initThreadPool } from '/pkg/zsettle_web.js';

const E = {
  worker: null,
  helper: null,
  record: null, // the open wallet's stored record
  key: null, // its AES key, while it is open
  cfg: null, // its app data: activity, item details, shops opened, last sync
  phrase: null, // a phrase confirmed for the next signing call only
  jobs: new Map(),
  nextJob: 1,
  dead: null,
  warm: false,
};

// ------------------------------------------------------------ the wasm worker

let nextId = 1;
const pending = new Map();
let queue = Promise.resolve();

// Calls run one after another: the wallet database has one writer.
function call(op, args = [], onProgress) {
  const run = () =>
    new Promise((resolve, reject) => {
      if (E.dead) return reject(new Error(E.dead));
      const id = nextId++;
      pending.set(id, { resolve, reject, onProgress, worker: E.worker });
      E.worker.postMessage({ id, op, args });
    });
  const p = queue.then(run, run);
  queue = p.catch(() => {});
  return p;
}

export async function start() {
  if (!self.crossOriginIsolated) throw new Error('This browser cannot run the wallet here (the page is not isolated). Try a current Chrome, Safari, Firefox or Edge.');
  const module = await WebAssembly.compileStreaming(fetch('/pkg/zsettle_web_bg.wasm'));
  const exports = await init({ module_or_path: module });
  await initThreadPool(Math.max(2, Math.min(navigator.hardwareConcurrency || 4, 8)));
  // The wallet's worker, and a helper that builds the proving key in the background: they
  // share the module and the memory, so the key the helper builds is the one trades use.
  E.worker = await spawn(module, exports.memory);
  E.helper = await spawn(module, exports.memory);
}

function spawn(module, memory) {
  const worker = new Worker('/worker.js', { type: 'module' });
  return new Promise((resolve, reject) => {
    worker.onmessage = ({ data }) => {
      if (data.type === 'ready') return resolve(worker);
      if (data.type === 'failed') return reject(new Error(data.err));
      if (data.type === 'crash') {
        // A worker cannot go on after a panic. The helper only builds the proving key; a
        // crashed wallet worker fails its calls and takes no more until the page reloads.
        console.warn('wallet worker:', data.err);
        if (worker !== E.worker) return;
        E.dead = 'The wallet stopped: ' + data.err.split('\n')[0] + ' Reload the page.';
        for (const [id, p] of pending) {
          if (p.worker !== worker) continue;
          pending.delete(id);
          p.reject(new Error(E.dead));
        }
        return;
      }
      const p = pending.get(data.id);
      if (!p) return;
      if ('progress' in data) {
        if (data.db) saveBytes(data.db).catch(() => {});
        return p.onProgress && p.onProgress(data.progress);
      }
      pending.delete(data.id);
      if ('err' in data) p.reject(new Error(data.err));
      else p.resolve(data.ok);
    };
    worker.onerror = (e) => reject(new Error(e.message || 'the wallet worker failed'));
    worker.postMessage({ type: 'init', module, memory });
  });
}

function warmUp() {
  if (E.warm) return;
  E.warm = true;
  const id = nextId++;
  pending.set(id, { resolve() {}, reject() {} });
  E.helper.postMessage({ id, op: 'warmUp', args: [] });
}

// ------------------------------------------------------------ storage and keys

function idb() {
  return new Promise((resolve, reject) => {
    const r = indexedDB.open('zsettle-wallet', 1);
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
// One wallet per network.
const KEY = (network) => 'wallet-' + network;

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
async function unseal(key, box) {
  return new Uint8Array(await crypto.subtle.decrypt({ name: 'AES-GCM', iv: unb64u(box.iv) }, key, box.ct));
}

// Passkeys: the PRF output, never stored, is the key's secret.
const NO_PRF = 'This passkey cannot derive a key. Use a passphrase, or a passkey saved in Chrome, Edge, Safari or a recent Firefox.';

async function passkeyCreate(network) {
  const salt = crypto.getRandomValues(new Uint8Array(32));
  const cred = await navigator.credentials.create({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rp: { name: 'zsettle', id: location.hostname },
      user: { id: crypto.getRandomValues(new Uint8Array(16)), name: 'zsettle wallet (' + network + ')', displayName: 'zsettle wallet (' + (network === 'main' ? 'mainnet' : 'testnet') + ')' },
      pubKeyCredParams: [
        { type: 'public-key', alg: -7 },
        { type: 'public-key', alg: -257 },
      ],
      authenticatorSelection: { residentKey: 'discouraged', userVerification: 'required' },
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
async function keyOf(record, passphrase) {
  if (record.lock.kind === 'passkey') return keyFromSecret(await passkeySecret(record.lock));
  return keyFromPassphrase(passphrase, unb64u(record.lock.salt));
}
async function phraseWith(key) {
  try {
    return dec.decode(await unseal(key, E.record.phrase));
  } catch {
    throw new Error('wrong passphrase');
  }
}

async function saveBytes(bytes) {
  if (!E.key || !E.record) return;
  E.record.db = await seal(E.key, bytes);
  E.record.opened = Date.now();
  await idbPut(KEY(E.record.network), E.record);
}
async function saveDb() {
  await saveBytes(await call('save'));
}
async function saveCfg() {
  if (!E.key || !E.record) return;
  E.record.cfg = await seal(E.key, enc.encode(JSON.stringify(E.cfg)));
  await idbPut(KEY(E.record.network), E.record);
}
const freshCfg = () => ({ activity: [], details: {}, sellers: [], last_sync: null, faucetAt: 0 });

// ------------------------------------------------------------ one tab at a time

async function holdTab(network) {
  if (!navigator.locks) return true;
  return new Promise((resolve) => {
    navigator.locks.request('zsettle-wallet-' + network, { ifAvailable: true }, (lock) => {
      if (!lock) return resolve(false);
      resolve(true);
      return new Promise((release) => (E.releaseTab = release));
    });
  });
}
function releaseTab() {
  if (E.releaseTab) E.releaseTab();
  E.releaseTab = null;
}
const OTHER_TAB = 'this wallet is open in another tab of this browser; use that tab, or close it and try again';
const keepStorage = () => navigator.storage && navigator.storage.persist && navigator.storage.persist().catch(() => {});

// ------------------------------------------------------------ wallets in this browser

/** The wallets this browser holds: network and how each is locked, last opened first. */
export async function wallets() {
  const out = [];
  for (const n of ['test', 'main']) {
    const r = await idbGet(KEY(n));
    if (r) out.push({ network: r.network, lock: r.lock.kind, opened: r.opened || 0, birthday: r.birthday });
  }
  return out.sort((a, b) => b.opened - a.opened);
}
export const isOpen = () => !!E.record;
export const current = () => (E.record ? { network: E.record.network, lock: E.record.lock.kind } : null);
export const newMnemonic = () => call('newMnemonic');
export const checkMnemonic = (p) => call('checkMnemonic', [p]);

/**
 * Create (or restore, with a birthday) the wallet of a network and open it. `lock` is
 * 'passkey' or 'passphrase'.
 */
export async function create({ network, phrase, birthday = 0, lock, passphrase }) {
  let made;
  if (lock === 'passkey') made = await passkeyCreate(network);
  else {
    if (!passphrase || passphrase.length < 12) throw new Error('use a passphrase of 12 characters or more');
    const salt = crypto.getRandomValues(new Uint8Array(16));
    made = { lock: { kind: 'passphrase', salt: b64u(salt) }, key: await keyFromPassphrase(passphrase, salt) };
  }
  if (!(await holdTab(network))) throw new Error(OTHER_TAB);
  try {
    const name = 'w' + b64u(crypto.getRandomValues(new Uint8Array(6)));
    await call('create', [name, network, phrase, birthday]);
    keepStorage();
    const view = await call('view');
    E.key = made.key;
    E.cfg = freshCfg();
    E.cfg.activity.unshift({ kind: 'wallet', text: birthday ? 'Wallet restored' : 'Wallet created', at: now() });
    E.record = { v: 1, name, network, birthday: view.birthday, lock: made.lock, phrase: await seal(made.key, enc.encode(phrase)), db: null, cfg: null };
    await saveDb();
    await saveCfg();
  } catch (e) {
    releaseTab();
    throw e;
  }
}

export async function unlock(network, passphrase) {
  const record = await idbGet(KEY(network));
  if (!record) throw new Error('this browser holds no wallet for that network');
  const key = await keyOf(record, passphrase);
  let db;
  try {
    db = await unseal(key, record.db);
  } catch {
    throw new Error(record.lock.kind === 'passkey' ? 'this passkey does not open this wallet' : 'wrong passphrase');
  }
  if (!(await holdTab(network))) throw new Error(OTHER_TAB);
  try {
    await call('open', [record.name, record.network, record.birthday, db]);
  } catch (e) {
    releaseTab();
    throw e;
  }
  keepStorage();
  E.key = key;
  E.record = record;
  try {
    E.cfg = record.cfg ? JSON.parse(dec.decode(await unseal(key, record.cfg))) : freshCfg();
  } catch {
    E.cfg = freshCfg();
  }
}

export async function lock() {
  await shopStop();
  releaseTab();
  E.key = E.record = E.cfg = E.phrase = null;
  call('close').catch(() => {});
}

export async function remove(network) {
  if (E.record && E.record.network === network) await lock();
  await idbDelete(KEY(network));
}

/**
 * Before a signing call: a passkey wallet asks the passkey now, while the tap that asked for
 * it is fresh; a passphrase wallet uses the key it was opened with.
 */
export async function confirm() {
  const key = E.record.lock.kind === 'passkey' ? await keyOf(E.record) : E.key;
  E.phrase = await phraseWith(key);
  return {};
}
async function takePhrase() {
  if (E.phrase) {
    const p = E.phrase;
    E.phrase = null;
    return p;
  }
  if (E.record.lock.kind === 'passkey') return phraseWith(await keyOf(E.record));
  return phraseWith(E.key);
}

/** The 24 words, after the passkey or the passphrase once more. */
export async function words(passphrase) {
  const key = await keyOf(E.record, passphrase);
  return (await phraseWith(key)).split(' ');
}

// ------------------------------------------------------------ the app's calls

const now = () => Math.floor(Date.now() / 1000);

function job(kind, fn) {
  const id = E.nextJob++;
  const j = { id, kind, state: 'running', steps: [], result: null, error: null };
  E.jobs.set(id, j);
  const step = (t) => j.steps.push(t);
  // Rewrite the last step in place: a count that moves.
  step.update = (t) => {
    if (j.steps.length) j.steps[j.steps.length - 1] = t;
  };
  Promise.resolve()
    .then(() => fn(step))
    .then(
      (r) => {
        j.state = 'done';
        j.result = r;
      },
      (e) => {
        j.state = 'failed';
        j.error = String((e && e.message) || e);
      },
    );
  return { job: id };
}

async function addActivity(entry) {
  E.cfg.activity.unshift({ at: now(), ...entry });
  E.cfg.activity = E.cfg.activity.slice(0, 200);
  await saveCfg();
}

const zat = (text) => {
  const t = String(text).trim().replace(/\s*zec$/i, '');
  if (!/^\d+(\.\d{1,8})?$/.test(t)) throw new Error('write the amount like 0.001');
  const [a, b = ''] = t.split('.');
  return Number(BigInt(a) * 100000000n + BigInt((b + '00000000').slice(0, 8)));
};

// A shop of this site, as the path the wallet posts to.
function shopPath(url) {
  const u = new URL(url, location.origin);
  if (u.origin !== location.origin || !/^\/shop\/[A-Za-z0-9_-]+\/?$/.test(u.pathname)) throw new Error('this wallet buys from the shops of this site');
  return u.pathname.replace(/\/$/, '');
}

const BUY_STEPS = {
  listings: 'Checking the shop’s signed price list',
  import: 'Adding the shop’s item key',
  sync: 'Finding the item on chain',
  prove: 'Building and proving the trade in your browser',
  offer: 'Handing the shop its copy',
  wait: 'Waiting for the shop’s signature',
  sign: 'Signing your payment',
  send: 'Sending the transaction',
};

async function syncNow() {
  const r = await call('sync', [], (p) => {
    if (p && p.scanned && p.tip) E.syncProgress = p;
  });
  E.syncProgress = null;
  E.cfg.last_sync = { report: r, at: now() };
  await saveDb();
  await saveCfg();
  warmUp();
  return r;
}

async function walletView() {
  const v = await call('view');
  const notes = v.notes.map((n) => ({ note: n.note, value_zat: n.value_zat, mined: n.mined, change: n.change }));
  const items = v.items.map((i) => {
    const d = E.cfg.details[i.note] || {};
    return { note: i.note, value_zat: i.value_zat, mined: i.mined, name: i.name || d.name || null, listed_zat: null, icon: i.icon || d.icon || null, description: i.description || d.description || null };
  });
  return { balance_zat: notes.reduce((a, n) => a + n.value_zat, 0), notes, items };
}

function status() {
  const st = {
    profile: 'browser',
    browser: true,
    hosted: true,
    wallet: true,
    unlocked: !!E.record,
    network: E.record.network,
    passkey: E.record.lock.kind === 'passkey' ? { kind: 'passkey' } : null,
    sellers: E.cfg.sellers,
    shop: { running: false, listen: null, public: null },
    public_shop: null,
    last_sync: E.cfg.last_sync,
    has_listings: false,
    faucet: E.record.network === 'test' && !E.cfg.faucetAt,
  };
  return st;
}

async function market(url) {
  const path = shopPath(url);
  const l = await call('prepareShop', [path]);
  const base = location.origin + path;
  if (!E.cfg.sellers.includes(base)) {
    E.cfg.sellers = [base, ...E.cfg.sellers].slice(0, 10);
    await saveCfg();
  }
  syncInBackground();
  return { base, network: l.network, fingerprint: l.item_ufvk.slice(-24), payout_address: l.payout_address, issued_at: l.issued_at, items: l.items, own: false };
}

// A call that needs the wallet waits for a sync in progress; say so, with the blocks left.
async function afterSync(step) {
  if (!bgSync) return;
  step('First the wallet finishes syncing');
  const t = setInterval(() => {
    const p = E.syncProgress;
    if (p) step.update('First the wallet finishes syncing: block ' + p.scanned.toLocaleString('en') + ' of ' + p.tip.toLocaleString('en'));
  }, 1000);
  try {
    await bgSync;
  } finally {
    clearInterval(t);
    step.update('The wallet is in sync');
  }
}

// A sync after a shop's key was added scans for its items while the visitor looks around.
let bgSync = null;
function syncInBackground() {
  if (bgSync) return;
  bgSync = syncNow()
    .catch(() => {})
    .finally(() => (bgSync = null));
}

/** Answer one of the app's /api calls. */
export async function api(path, body) {
  const m = body === undefined ? 'GET' : 'POST';
  const jobMatch = path.match(/^\/api\/jobs\/(\d+)$/);
  if (jobMatch) {
    const j = E.jobs.get(Number(jobMatch[1]));
    if (!j) throw new Error('no such task');
    return { ...j, steps: [...j.steps] };
  }
  if (path === '/api/shops') {
    const r = await fetch('/api/shops', { headers: { 'X-Zsettle': '1' } });
    return r.ok ? r.json() : [];
  }
  if (!E.record) throw new Error('Please unlock your wallet.');
  switch (m + ' ' + path) {
    case 'GET /api/status':
      return status();
    case 'GET /api/wallet':
      return walletView();
    case 'GET /api/activity':
      return E.cfg.activity;
    case 'POST /api/address': {
      const address = await call('address');
      await saveDb();
      return { address };
    }
    case 'POST /api/sync':
      return job('sync', async (step) => {
        step('Syncing with the network');
        return syncNow();
      });
    case 'POST /api/faucet':
      return job('faucet', async (step) => {
        step('Asking this site’s faucet for test coins');
        const address = await call('address');
        await saveDb();
        const r = await fetch('/api/wallet-faucet', { method: 'POST', headers: { 'X-Zsettle': '1', 'Content-Type': 'application/json' }, body: JSON.stringify({ address }) });
        const j = await r.json().catch(() => ({}));
        if (!r.ok) throw new Error(j.error || 'the faucet did not answer');
        E.cfg.faucetAt = now();
        await addActivity({ kind: 'faucet', amount_zat: j.amount_zat, text: 'Free test coins on their way' });
        return { amount_zat: j.amount_zat };
      });
    case 'POST /api/pay':
      return job('pay', async (step) => {
        const phrase = await takePhrase();
        await afterSync(step);
        step('Syncing with the network');
        await syncNow();
        step('Building, proving and sending the payment in your browser');
        const amount = zat(body.amount);
        const r = await call('pay', [phrase, body.to, amount, body.memo || '']);
        await saveDb();
        await addActivity({ kind: 'pay', txid: r.txid, amount_zat: amount, text: 'Sent ZEC' });
        return { txid: r.txid, amount_zat: amount };
      });
    case 'POST /api/market':
      return market(body.url);
    case 'POST /api/buy':
      return job('buy', async (step) => {
        const phrase = await takePhrase();
        await afterSync(step);
        const path = shopPath(body.url);
        // The price and address the buyer confirmed: the purchase stops if the shop's signed
        // list says otherwise by the time the trade is built.
        const l = await call('prepareShop', [path]);
        const it = (l.items || []).find((i) => i.note === body.item);
        if (!it) throw new Error('the shop no longer lists this item');
        let last = null;
        const r = await call('buy', [phrase, path, body.item, it.price_zat, l.payout_address], (p) => {
          if (p && p.step && p.step !== last && BUY_STEPS[p.step]) {
            last = p.step;
            step(BUY_STEPS[p.step]);
          }
        });
        await saveDb();
        await addActivity({ kind: 'buy', txid: r.txid, price_zat: r.price_zat, fee_zat: r.fee_zat, text: 'Bought ' + (r.name || 'an item') });
        return r;
      });
    case 'POST /api/prove':
      return job('prove', async (step) => {
        const phrase = await takePhrase();
        await afterSync(step);
        step('Syncing with the network');
        await syncNow();
        step('Signing the proof with the item’s key, in your browser');
        const proof = JSON.parse(await call('prove', [phrase, body.note, body.label || '', body.challenge || '']));
        const name = (await walletView()).items.find((i) => i.note === body.note);
        await addActivity({ kind: 'prove', text: 'Made an ownership proof for ' + ((name && name.name) || 'an item') });
        return { proof };
      });
    case 'GET /api/shop':
      return { running: false, listen: null, public: null, events: [] };
    case 'GET /api/listings':
      return { items: [] };
    case 'POST /api/lock':
      await lock();
      return { locked: true };
    default:
      throw new Error('this part is not available in the browser wallet yet');
  }
}

async function shopStop() {
  /* the browser shop comes with selling */
}
