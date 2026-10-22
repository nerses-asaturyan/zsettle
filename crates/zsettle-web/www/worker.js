// The wallet's worker: every call into the wallet runs here, off the page's thread, so the
// page stays responsive while it syncs and proves. The page sends the compiled module and the
// shared memory its thread pool already uses; then calls arrive as { id, op, args } and are
// answered as { id, ok } or { id, err }, with { id, progress } in between.
import init, * as zw from './pkg/zsettle_web.js';

let wallet = null;
let ready = null;

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
  buy: (progress, mnemonic, shop, note) => need().buy(mnemonic, shop, note, progress),
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
    const a = WITH_PROGRESS.has(op) ? [(p) => self.postMessage({ id, progress: p }), ...args] : args;
    let ok = await fn(...a);
    // Bytes leave as their own copy: the shared wasm memory cannot be posted.
    if (ok instanceof Uint8Array) ok = ok.slice();
    self.postMessage({ id, ok }, ok instanceof Uint8Array ? [ok.buffer] : []);
  } catch (e) {
    self.postMessage({ id, err: String(e && e.message ? e.message : e) });
  }
};
