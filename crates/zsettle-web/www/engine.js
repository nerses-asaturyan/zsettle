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
  started: null, // the engine starting: calls wait for it
  syncs: 0, // syncs running now
  syncProgress: null, // the running sync's { scanned, tip }
  pendingPasskey: null, // a passkey made but not yet read: { network, lock }
  locks: {}, // how each stored wallet is locked, read with the list of wallets
  shop: { open: false, code: null, timer: null, events: [], busy: false, lastSync: 0 },
};

// ------------------------------------------------------------ the wasm worker

let nextId = 1;
const pending = new Map();
let queue = Promise.resolve();

// Calls run one after another: the wallet database has one writer.
function call(op, args = [], onProgress) {
  const run = async () => {
    await E.started;
    return new Promise((resolve, reject) => {
      if (E.dead) return reject(new Error(E.dead));
      const id = nextId++;
      pending.set(id, { resolve, reject, onProgress, worker: E.worker });
      E.worker.postMessage({ id, op, args });
    });
  };
  const p = queue.then(run, run);
  queue = p.catch(() => {});
  return p;
}

/**
 * Start the engine: download and compile the module, start its threads and workers. The page
 * can show the unlock screen meanwhile (stored wallets are read without the engine); every
 * call into the wallet waits for this.
 */
export function start() {
  if (!E.started) {
    E.started = (async () => {
      if (!self.crossOriginIsolated) throw new Error('This browser cannot run the wallet here (the page is not isolated). Try a current Chrome, Safari, Firefox or Edge.');
      const module = await WebAssembly.compileStreaming(fetch('/pkg/zsettle_web_bg.wasm'));
      const exports = await init({ module_or_path: module });
      await initThreadPool(Math.max(2, Math.min(navigator.hardwareConcurrency || 4, 8)));
      // The wallet's worker, and a helper that builds the proving key in the background:
      // they share the module and the memory, so the key the helper builds is the one
      // trades use.
      E.worker = await spawn(module, exports.memory);
      E.helper = await spawn(module, exports.memory);
    })();
  }
  return E.started;
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

// The proving key takes a while to build and competes with every other wallet call for the
// same memory, so it is built only once a shop is open, the wallet is idle and the page is
// in front (a page in the background gets little time, and the build would hold up the
// wallet for minutes).
function warmUp() {
  if (E.warm || !document.hasFocus() || E.syncs || pending.size) return;
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
const NO_PRF = 'This passkey cannot make a key for the wallet. Save the passkey in Google Password Manager (Android) or iCloud Keychain (iPhone), or use a passphrase.';

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
      // A discoverable passkey: on Android only those are kept by the password manager,
      // which is what derives a key (PRF); others go to a module that cannot.
      authenticatorSelection: { residentKey: 'required', requireResidentKey: true, userVerification: 'required' },
      extensions: { prf: { eval: { first: salt } } },
    },
  });
  const ext = cred.getClientExtensionResults().prf || {};
  const lock = { kind: 'passkey', id: b64u(cred.rawId), salt: b64u(salt) };
  let secret = ext.results && ext.results.first;
  if (!secret) {
    if (ext.enabled === false) throw new Error(NO_PRF);
    // Some browsers derive the key only when the passkey is used, and Safari lets a page
    // use it only right after a tap: keep it, and read it on the next tap.
    E.pendingPasskey = { network, lock };
    try {
      secret = await passkeySecret(lock);
    } catch (e) {
      if (e.name === 'NotAllowedError') throw new Error(PASSKEY_AGAIN);
      throw e;
    }
  }
  E.pendingPasskey = null;
  return { lock, key: await keyFromSecret(secret) };
}
const PASSKEY_AGAIN = 'Your passkey is saved. Tap “Lock with a passkey” once more to finish.';
// The passkey made by a first tap whose key could not be read then.
async function passkeyAgain(network) {
  const { lock } = E.pendingPasskey;
  const key = await keyFromSecret(await passkeySecret(lock));
  E.pendingPasskey = null;
  return { lock, key };
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
    if (r) {
      E.locks[n] = r.lock;
      out.push({ network: r.network, lock: r.lock.kind, opened: r.opened || 0, birthday: r.birthday });
    }
  }
  return out.sort((a, b) => b.opened - a.opened);
}
export const isOpen = () => !!E.record;
export const current = () => (E.record ? { network: E.record.network, lock: E.record.lock.kind } : null);
export const newMnemonic = () => call('newMnemonic');
export const checkMnemonic = (p) => call('checkMnemonic', [p]);
/** Whether this wallet can pay `address` (a shielded address of its network). */
export const checkAddress = (address) => call('checkAddress', [E.record ? E.record.network : 'main', address]);

/**
 * Create (or restore, with a birthday) the wallet of a network and open it. `lock` is
 * 'passkey' or 'passphrase'.
 */
export async function create({ network, phrase, birthday = 0, lock, passphrase }) {
  let made;
  if (lock === 'passkey') made = E.pendingPasskey && E.pendingPasskey.network === network ? await passkeyAgain(network) : await passkeyCreate(network);
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
  // The passkey prompt comes first, straight from the tap: Safari shows it only then. The
  // lock was read with the list of wallets.
  const lock = E.locks[network] || ((await idbGet(KEY(network))) || {}).lock;
  if (!lock) throw new Error('this browser holds no wallet for that network');
  const key = await keyOf({ lock }, passphrase);
  const record = await idbGet(KEY(network));
  if (!record) throw new Error('this browser holds no wallet for that network');
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
  E.shop.code = E.cfg.listings ? await call('listingsCode', [E.cfg.listings]).catch(() => null) : null;
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

const blocks = (p) => 'block ' + p.scanned.toLocaleString('en') + ' of ' + p.tip.toLocaleString('en');
const CATCHING_UP = 'Catching up with the chain';

// What the buyer sees of a purchase: five steps, some made of several of the wallet's.
const BUY_STEPS = {
  listings: 'Checking the shop',
  import: 'Checking the shop',
  sync: CATCHING_UP,
  prove: 'Building the trade on your device',
  offer: 'Waiting for the shop to sign',
  wait: 'Waiting for the shop to sign',
  sign: 'Sending',
  send: 'Sending',
};

async function syncNow(onProgress) {
  E.syncs++;
  let r;
  try {
    r = await call('sync', [], (p) => {
      if (p && p.scanned && p.tip) {
        E.syncProgress = p;
        if (onProgress) onProgress(p);
      }
    });
  } finally {
    E.syncs--;
    if (!E.syncs) E.syncProgress = null;
  }
  E.cfg.last_sync = { report: r, at: now() };
  await saveDb();
  await saveCfg();
  await reconcileSales();
  return r;
}

// Items of the listings the wallet has seen spent: each sale goes into the activity once, and
// the listings drop it (signed again now if the shop is open, else when it next opens).
async function reconcileSales() {
  if (!E.cfg.listings) return;
  try {
    const rec = await call('shopReconcile', [E.cfg.listings]);
    E.cfg.sold = E.cfg.sold || [];
    for (const s of rec.sold || []) {
      if (E.cfg.sold.includes(s.note)) continue;
      E.cfg.sold = [s.note, ...E.cfg.sold].slice(0, 500);
      if (E.shop.open) shopEvent({ event: 'sold', name: s.name, txid: s.txid });
      await addActivity({ kind: 'sale', txid: s.txid, price_zat: s.price_zat, text: 'Sold ' + (s.name || 'an item') });
    }
    if (rec.listings) await setListings(rec.listings);
  } catch {
    /* judged again after the next sync */
  }
}

async function walletView() {
  const v = await call('view');
  const notes = v.notes.map((n) => ({ note: n.note, value_zat: n.value_zat, mined: n.mined, change: n.change, reserved: n.reserved }));
  const listed = {};
  for (const it of listedItems()) listed[it.note] = it;
  const items = v.items.map((i) => {
    const d = E.cfg.details[i.note] || {};
    const l = listed[i.note] || {};
    return { note: i.note, value_zat: i.value_zat, mined: i.mined, name: i.name || l.name || d.name || null, listed_zat: l.price_zat || null, icon: i.icon || l.icon || d.icon || null, description: i.description || l.description || d.description || null };
  });
  // Notes held for an offer are not part of the balance; those whose offer can no longer
  // settle can be moved to a fresh note (movable_zat).
  return { balance_zat: v.balance_zat, movable_zat: v.movable_zat || 0, waiting_zat: v.waiting_zat || 0, waiting_until: v.waiting_until || 0, notes, items };
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
    shop: { running: E.shop.open, listen: null, public: shopUrl() },
    public_shop: shopUrl(),
    last_sync: E.cfg.last_sync,
    // A sync running now, the page's own or one started here (a shop's key to scan).
    syncing: E.syncs > 0,
    sync_progress: E.syncProgress,
    has_listings: !!E.cfg.listings,
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
  // The visitor may buy here: build the proving key once the shop's items are found.
  bgSync.then(warmUp);
  return { base, network: l.network, fingerprint: l.item_ufvk.slice(-24), payout_address: l.payout_address, issued_at: l.issued_at, items: l.items, own: false };
}

// A call that needs the wallet waits for a sync in progress; say so, with the blocks left.
// Returns whether it waited.
async function afterSync(step) {
  if (!bgSync) return false;
  step(CATCHING_UP);
  const t = setInterval(() => {
    const p = E.syncProgress;
    if (p) step.update(CATCHING_UP + ': ' + blocks(p));
  }, 1000);
  try {
    await bgSync;
  } finally {
    clearInterval(t);
    step.update(CATCHING_UP);
  }
  return true;
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
    const shops = r.ok ? await r.json() : [];
    // Only the shops of the open wallet's network.
    return E.record ? shops.filter((s) => !s.network || s.network === E.record.network) : shops;
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
        return syncNow((p) => step.update('Syncing with the network: ' + blocks(p)));
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
        let last = (await afterSync(step)) ? CATCHING_UP : null;
        const path = shopPath(body.url);
        // The price and address the buyer confirmed: the purchase stops if the shop's signed
        // list says otherwise by the time the trade is built.
        const l = await call('prepareShop', [path]);
        const it = (l.items || []).find((i) => i.note === body.item);
        if (!it) throw new Error('the shop no longer lists this item');
        // The price shown when the buyer confirmed: a shop that changed it since then is not paid
        // the new price unseen.
        if (body.price_zat != null && it.price_zat !== Number(body.price_zat)) {
          const now = (it.price_zat / 1e8).toFixed(8).replace(/\.?0+$/, '');
          throw new Error('the seller changed the price to ' + now + ' ZEC since you opened the shop; open it again to see the new price');
        }
        let r;
        try {
          r = await call('buy', [phrase, path, body.item, it.price_zat, l.payout_address], (p) => {
            if (!p) return;
            const label = BUY_STEPS[p.step];
            if (label && label !== last) {
              last = label;
              step(label);
            } else if (p.scanned && p.tip && last === CATCHING_UP) {
              step.update(CATCHING_UP + ': ' + blocks(p));
            }
          });
        } catch (e) {
          // A failed purchase may have moved the held coins: keep that.
          await saveDb();
          if (/moved to a fresh note/.test(String(e.message || e))) await addActivity({ kind: 'rotate', text: 'Coins moved to a fresh note after a failed offer' });
          throw e;
        }
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
    case 'POST /api/verify':
      return job('verify', async (step) => {
        await afterSync(step);
        // The shops whose keys may have held the item: the one named, else the one it was
        // bought from, else the shops opened lately, tried in turn. This wallet's own item key
        // is always added.
        const mine = (await walletView()).items.find((i) => i.note === body.item);
        const named = body.url ? [shopPath(body.url)] : mine && mine.shop ? ['/shop/' + mine.shop] : E.cfg.sellers.slice(0, 5).map((s) => new URL(s).pathname);
        const shops = [];
        for (const p of named) {
          const r = await fetch(p + '/listings', { headers: { 'X-Zsettle': '1' } }).catch(() => null);
          if (r && r.ok) shops.push({ label: 'shop', text: await r.text() });
        }
        step('Tracing the item through the chain with viewing keys only, in your browser');
        const tries = shops.length ? shops.map((s) => [s]) : [[]];
        let last = null;
        for (const t of tries) {
          try {
            return await call('history', [body.item, JSON.stringify(t)], (p) => {
              if (p && p.scanned && p.tip) step.update('Tracing the item in your browser: block ' + p.scanned.toLocaleString('en') + ' of ' + p.tip.toLocaleString('en'));
            });
          } catch (e) {
            last = e;
          }
        }
        throw last || new Error('no shop could trace this item');
      });
    case 'POST /api/check-proof':
      return job('check-proof', async (step) => {
        const proof = String(body.proof || '');
        if (proof.length > 16 * 1024) throw new Error('that proof is too large');
        step('Checking the signature and the block the proof names');
        step('Looking on the chain whether the item is still held, in your browser');
        return call('checkProof', [proof, body.challenge || ''], (p) => {
          if (p && p.scanned && p.tip) step.update('Looking on the chain whether the item is still held: block ' + p.scanned.toLocaleString('en') + ' of ' + p.tip.toLocaleString('en'));
        });
      });
    case 'GET /api/shop':
      return { running: E.shop.open, listen: null, public: shopUrl(), events: E.shop.events };
    case 'GET /api/listings': {
      // Sold items stay in the signed copy until the shop next opens; they are not shown.
      const l = E.cfg.listings ? JSON.parse(E.cfg.listings) : { items: [] };
      l.items = (l.items || []).filter((i) => !(E.cfg.sold || []).includes(i.note));
      return l;
    }
    case 'POST /api/mint':
      return job('mint', async (step) => {
        const phrase = await takePhrase();
        await afterSync(step);
        step('Syncing with the network');
        await syncNow();
        step('Minting the items in one shielded transaction, in your browser');
        const names = (body.names || []).map((n) => String(n).trim()).filter(Boolean);
        const r = await call('mint', [phrase, names]);
        await saveDb();
        await addActivity({ kind: 'mint', txid: r.txid, text: 'Minted ' + names.join(', ') });
        return r;
      });
    case 'POST /api/publish':
      return job('publish', async (step) => {
        const phrase = await takePhrase();
        await afterSync(step);
        step('Signing the listings with your item key, in your browser');
        const listings = await call('publish', [phrase, E.cfg.listings || '', body.note, body.name || '', zat(body.price), body.description || '', body.icon || '']);
        await saveDb();
        await setListings(listings);
        await addActivity({ kind: 'list', text: 'Listed ' + (body.name || 'an item') + ' for ' + String(body.price).replace(/\s*zec$/i, '') + ' ZEC' });
        return { items: JSON.parse(listings).items.length };
      });
    case 'POST /api/delist':
      return job('delist', async (step) => {
        const phrase = await takePhrase();
        step('Removing the item from your listings');
        const listings = await call('delist', [phrase, E.cfg.listings || '', body.note]);
        await setListings(listings);
        await addActivity({ kind: 'delist', text: 'Delisted an item' });
        return { ok: true };
      });
    case 'POST /api/rotate':
      return job('rotate', async (step) => {
        const phrase = await takePhrase();
        await afterSync(step);
        step('Syncing with the network');
        await syncNow();
        step('Moving the held coins to a fresh note, in your browser');
        const r = await call('rotate', [phrase]);
        await saveDb();
        for (const txid of r.moved || []) await addActivity({ kind: 'rotate', txid, text: 'Coins moved to a fresh note after a failed offer' });
        return r;
      });
    case 'POST /api/cancel':
      return job('cancel', async (step) => {
        const phrase = await takePhrase();
        await afterSync(step);
        step('Syncing with the network');
        await syncNow();
        step('Moving the item to a fresh note, so no signed offer can settle');
        const CANCEL_STEPS = { prove: 'Proving the move in your browser', send: 'Sending the transaction', record: 'Recording it in your wallet', name: 'Keeping the item’s name' };
        const r = await call('cancel', [phrase, body.note], (p) => p && CANCEL_STEPS[p.step] && step(CANCEL_STEPS[p.step]));
        step('Sent; saving the wallet');
        await saveDb();
        if (listedItems().some((i) => i.note === body.note)) {
          step('Taking the item off your listings');
          try {
            await setListings(await call('delist', [phrase, E.cfg.listings, body.note]));
          } catch {
            /* delisted when the shop next sees the note spent */
          }
        }
        await addActivity({ kind: 'cancel', txid: r.txid, text: 'Cancelled the offers on an item' });
        return r;
      });
    case 'POST /api/shop/start':
      await shopStart();
      return api('/api/shop');
    case 'POST /api/shop/stop':
      await shopStop();
      return api('/api/shop');
    case 'POST /api/lock':
      await lock();
      return { locked: true };
    default:
      throw new Error('this part is not available in the browser wallet yet');
  }
}

// ------------------------------------------------------------ the shop in this page
//
// The shop signs buyers' offers with the item key, so it is open only while this page is: the
// key stays in the wallet's worker meanwhile. The server keeps a mailbox for it (its signed
// listings, the offers buyers leave, the replies), and every request to it is signed with the
// item key.

const SHOP_POLL_MS = 4000;
const SHOP_SYNC_MS = 30000;

function listedItems() {
  try {
    const sold = (E.cfg && E.cfg.sold) || [];
    return E.cfg && E.cfg.listings ? (JSON.parse(E.cfg.listings).items || []).filter((i) => !sold.includes(i.note)) : [];
  } catch {
    return [];
  }
}
function shopUrl() {
  return E.cfg && E.cfg.listings && E.shop.code ? location.origin + '/shop/' + E.shop.code : null;
}
function shopEvent(e) {
  E.shop.events.push({ at: now(), ...e });
  if (E.shop.events.length > 200) E.shop.events.splice(0, E.shop.events.length - 200);
}

async function mailbox(action, payload, extra = {}) {
  const at = now();
  const what = action === 'listings' ? 'listings\n' + E.shop.code + '\n' + payload : action === 'reply' ? 'reply\n' + E.shop.code + '\n' + extra.tag + '\n' + payload : action + '\n' + E.shop.code;
  const sig = await call('shopAuth', [what + '\n' + at]);
  const r = await fetch('/api/mailbox/' + E.shop.code + '/' + action, {
    method: 'POST',
    headers: { 'X-Zsettle': '1', 'Content-Type': 'application/json' },
    body: JSON.stringify({ at, sig, ...extra }),
  });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || 'the server did not take it');
  return j;
}

async function setListings(listings) {
  E.cfg.listings = listings;
  E.shop.code = await call('listingsCode', [listings]);
  await saveCfg();
  if (E.shop.open) await mailbox('listings', listings, { listings });
}

async function shopStart() {
  if (!E.cfg.listings) throw new Error('list an item first, then open the shop');
  const phrase = await takePhrase();
  await call('shopOpen', [phrase]);
  // Items sold while the shop was closed leave the listings before it opens.
  await reconcileSales();
  if (!E.cfg.listings || !listedItems().length) {
    await call('shopClose', []);
    throw new Error('every listed item has sold; list an item, then open the shop');
  }
  E.shop.code = await call('listingsCode', [E.cfg.listings]);
  try {
    await mailbox('listings', E.cfg.listings, { listings: E.cfg.listings });
  } catch (e) {
    await call('shopClose', []);
    throw e;
  }
  E.shop.open = true;
  shopEvent({ event: 'opened' });
  clearInterval(E.shop.timer);
  E.shop.timer = setInterval(shopPass, SHOP_POLL_MS);
  shopPass();
}

async function shopStop() {
  if (!E.shop.open) return;
  clearInterval(E.shop.timer);
  E.shop.timer = null;
  try {
    await mailbox('close', '');
  } catch {
    /* the server closes it by itself a little later */
  }
  E.shop.open = false;
  await call('shopClose', []).catch(() => {});
}

// One pass: now and then a sync (to see sales and new blocks), then every waiting offer.
async function shopPass() {
  if (!E.shop.open || E.shop.busy || !E.record) return;
  E.shop.busy = true;
  try {
    if (Date.now() - E.shop.lastSync > SHOP_SYNC_MS) {
      E.shop.lastSync = Date.now();
      await syncNow();
    }
    let offers;
    try {
      ({ offers } = await mailbox('poll', ''));
    } catch (e) {
      // The server forgot the mailbox (it restarted): hand it the listings again.
      if (!/no such shop/i.test(String(e.message || e))) throw e;
      await mailbox('listings', E.cfg.listings, { listings: E.cfg.listings });
      shopEvent({ event: 'reopened' });
      ({ offers } = await mailbox('poll', ''));
    }
    for (const o of offers || []) {
      const bytes = Uint8Array.from(o.envelope.match(/../g).map((x) => parseInt(x, 16)));
      const r = await call('shopSign', [bytes, E.cfg.listings]);
      if (r.state === 'signed') {
        await mailbox('reply', 'signed:' + r.reply, { tag: o.tag, signed: r.reply });
        shopEvent({ event: 'signed', name: r.name, paid_zat: r.paid_zat });
      } else if (r.state === 'rejected') {
        await mailbox('reply', 'refused:' + r.reason, { tag: o.tag, refused: r.reason });
        shopEvent({ event: 'rejected', reason: r.reason });
      } else {
        shopEvent({ event: 'deferred', reason: r.reason });
      }
    }
  } catch (e) {
    shopEvent({ event: 'error', error: String(e.message || e) });
  } finally {
    E.shop.busy = false;
  }
}
