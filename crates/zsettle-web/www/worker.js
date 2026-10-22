// The wallet's worker: every call into the wallet runs here, off the page's thread, so the
// page stays responsive while it syncs and proves. The page sends the compiled module and the
// shared memory its thread pool already uses; then calls arrive as { id, op, args } and are
// answered as { id, ok } or { id, err }, with { id, progress } in between.
import init, * as zw from './pkg/zsettle_web.js';

let wallet = null;
let ready = null;

// A Rust panic ends in a trap that no promise reports: tell the page, which fails the call
// in flight instead of waiting for ever.
const crash = (text) => self.postMessage({ type: 'crash', err: String(text).slice(0, 2000) });
const consoleError = console.error;
console.error = (...a) => {
  const text = a.map(String).join(' ');
  if (/panicked at/.test(text)) crash(text);
  consoleError(...a);
};
self.addEventListener('error', (e) => crash(e.message || 'the wallet stopped'));
self.addEventListener('unhandledrejection', (e) => crash((e.reason && e.reason.message) || e.reason || 'the wallet stopped'));

const ops = {
  newMnemonic: () => zw.new_mnemonic(),
  checkMnemonic: (phrase) => zw.check_mnemonic(phrase),
  warmUp: () => zw.warm_up(),
  async create(name, network, mnemonic, birthday) {
    if (wallet) wallet.free();
    wallet = await zw.WebWallet.create(name, network, mnemonic, birthday);
    return true;
  },
  open(name, network, birthday, bytes) {
    if (wallet) wallet.free();
    wallet = zw.WebWallet.open(name, network, birthday, bytes);
    return true;
  },
  close() {
    if (wallet) wallet.free();
    wallet = null;
    return true;
  },
  save: () => need().save(),
  sync: (progress) => need().sync(progress),
  view: () => need().view(),
  address: () => need().address(),
  pay: (progress, mnemonic, to, amount, memo) => need().pay(mnemonic, to, BigInt(amount), memo || undefined),
  buy: (progress, mnemonic, shop, note, price, payout) => need().buy(mnemonic, shop, note, BigInt(price), payout, progress),
  prepareShop: (shop) => need().prepare_shop(shop),
  prove: (mnemonic, note, label, challenge) => need().prove(mnemonic, note, label || undefined, challenge || undefined),
  checkPhrase: (mnemonic) => need().check_phrase(mnemonic),
};

// Calls that report progress get a callback as their first argument.
const WITH_PROGRESS = new Set(['sync', 'pay', 'buy']);

function need() {
  if (!wallet) throw new Error('no wallet is open');
  return wallet;
}

self.onmessage = async ({ data }) => {
  if (data.type === 'init') {
    ready = init({ module_or_path: data.module, memory: data.memory });
    try {
      await ready;
      self.postMessage({ type: 'ready' });
    } catch (e) {
      self.postMessage({ type: 'failed', err: String(e && e.message ? e.message : e) });
    }
    return;
  }
  const { id, op, args } = data;
  try {
    await ready;
    const fn = ops[op];
    if (!fn) throw new Error('unknown call ' + op);
    const report = (p, db) => self.postMessage({ id, progress: p, db: db instanceof Uint8Array ? db.slice() : undefined });
    const a = WITH_PROGRESS.has(op) ? [report, ...args] : args;
    let ok = await fn(...a);
    // Bytes leave as their own copy: the shared wasm memory cannot be posted.
    if (ok instanceof Uint8Array) ok = ok.slice();
    self.postMessage({ id, ok }, ok instanceof Uint8Array ? [ok.buffer] : []);
  } catch (e) {
    self.postMessage({ id, err: String(e && e.message ? e.message : e) });
  }
};
