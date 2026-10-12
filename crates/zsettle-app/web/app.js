'use strict';
// zsettle local page. Everything a seller or the chain supplies is shown as text
// (textContent), never as HTML.

const S = {
  token: null,
  status: null,
  tab: 'wallet',
  wallet: null,
  market: null,
  listings: null,
  shop: null,
  activity: [],
  shopTimer: null,
};

const NO_PRF =
  'This passkey cannot derive a key (the PRF feature). Use a passphrase, or a passkey from Chrome, Edge, Safari or a recent Firefox.';

// ------------------------------------------------------------ helpers

function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (v === null || v === undefined || v === false) continue;
      if (k === 'class') el.className = v;
      else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
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
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
function zec(zat) {
  if (zat === null || zat === undefined) return '';
  const n = Number(zat) / 1e8;
  return n.toLocaleString(undefined, { maximumFractionDigits: 8 }) + ' ZEC';
}
function short(s, n = 8) {
  s = String(s || '');
  return s.length > 2 * n + 1 ? s.slice(0, n) + '…' + s.slice(-n) : s;
}
function ago(sec) {
  if (!sec) return '';
  const d = Math.max(0, Math.floor(Date.now() / 1000 - sec));
  if (d < 60) return d + ' s ago';
  if (d < 3600) return Math.floor(d / 60) + ' min ago';
  if (d < 86400) return Math.floor(d / 3600) + ' h ago';
  return new Date(sec * 1000).toLocaleString();
}
function toast(text) {
  const t = h('div', { class: 'toast' }, text);
  document.body.append(t);
  setTimeout(() => t.remove(), 2600);
}
async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast('Copied');
  } catch {
    toast('Copy failed: select the text instead');
  }
}
// A colour per item, from its name or note, for the card art.
function art(seed, label) {
  let x = 0;
  for (const c of String(seed)) x = (x * 31 + c.charCodeAt(0)) >>> 0;
  const a = x % 360;
  const b = (a + 40 + (x % 80)) % 360;
  const el = h('div', { class: 'art' }, (label || '?').trim().slice(0, 2).toUpperCase());
  el.style.background = `linear-gradient(135deg, hsl(${a} 70% 45%), hsl(${b} 70% 30%))`;
  return el;
}

async function api(path, body) {
  const opt = { method: body === undefined ? 'GET' : 'POST', headers: { 'X-Zsettle-Token': S.token } };
  if (body !== undefined) {
    opt.headers['Content-Type'] = 'application/json';
    opt.body = JSON.stringify(body);
  }
  const r = await fetch(path, opt);
  let data;
  try {
    data = await r.json();
  } catch {
    data = { error: 'unexpected answer ' + r.status };
  }
  if (!r.ok) throw new Error(data.error || 'error ' + r.status);
  return data;
}

// ------------------------------------------------------------ modal and jobs

function modal(title, content) {
  const closeBtn = h('button', { class: 'btn ghost', disabled: true }, 'Close');
  const box = h('div', { class: 'modal' }, h('h2', null, title, h('span', { class: 'spacer' })), content, h('div', { class: 'row' }, h('span', { class: 'spacer' }), closeBtn));
  const shade = h('div', { class: 'shade' }, box);
  const layer = document.getElementById('layer');
  layer.append(shade);
  const close = () => shade.remove();
  closeBtn.addEventListener('click', close);
  return {
    box,
    close,
    closable() {
      closeBtn.disabled = false;
      closeBtn.focus();
    },
  };
}

// Start a job and follow its steps in a modal. Returns the result, or null on failure.
async function runJob(title, path, body, opts = {}) {
  const steps = h('ol', { class: 'steps' });
  const msg = h('div', { class: 'muted' }, 'Starting…');
  const extra = h('div');
  const m = modal(title, [opts.intro ? h('p', { class: 'note' }, opts.intro) : null, steps, msg, extra]);
  try {
    const { job } = await api(path, body);
    for (;;) {
      await sleep(1000);
      const j = await api('/api/jobs/' + job);
      const n = j.steps.length;
      steps.replaceChildren(
        ...j.steps.map((s, i) => {
          let cls = 'done';
          if (i === n - 1 && j.state === 'running') cls = 'active';
          if (i === n - 1 && j.state === 'failed') cls = 'failed';
          return h('li', { class: cls }, s);
        }),
      );
      if (j.state === 'running') {
        msg.textContent = '';
        continue;
      }
      if (j.state === 'failed') {
        msg.replaceChildren(h('div', { class: 'err' }, j.error || 'failed'));
        m.closable();
        return null;
      }
      msg.replaceChildren(h('div', { class: 'ok' }, opts.done ? opts.done(j.result) : 'Done.'));
      if (opts.render) extra.replaceChildren(opts.render(j.result));
      m.closable();
      return j.result;
    }
  } catch (e) {
    msg.replaceChildren(h('div', { class: 'err' }, e.message));
    m.closable();
    return null;
  }
}

// ------------------------------------------------------------ passkeys

const b64u = (buf) =>
  btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const unb64u = (s) => {
  const p = s.replace(/-/g, '+').replace(/_/g, '/');
  return Uint8Array.from(atob(p + '==='.slice((p.length + 3) % 4)), (c) => c.charCodeAt(0));
};
const hex = (buf) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, '0')).join('');

function passkeyUsable() {
  return !!window.PublicKeyCredential && location.hostname === 'localhost';
}

// The passkey's PRF output, never stored anywhere, is the wallet passphrase.
async function passkeyCreate() {
  if (!passkeyUsable()) throw new Error('Passkeys need this page opened as http://localhost:…');
  const salt = crypto.getRandomValues(new Uint8Array(32));
  const cred = await navigator.credentials.create({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rp: { name: 'zsettle', id: 'localhost' },
      user: {
        id: crypto.getRandomValues(new Uint8Array(16)),
        name: 'zsettle-' + S.status.profile,
        displayName: 'zsettle wallet (' + S.status.profile + ')',
      },
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
  return { pk, passphrase: 'passkey:' + hex(secret) };
}

async function passkeySecret(pk) {
  const a = await navigator.credentials.get({
    publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)),
      rpId: 'localhost',
      allowCredentials: [{ type: 'public-key', id: unb64u(pk.id) }],
      userVerification: 'required',
      extensions: { prf: { eval: { first: unb64u(pk.salt) } } },
    },
  });
  const r = a.getClientExtensionResults().prf;
  if (!r || !r.results || !r.results.first) throw new Error(NO_PRF);
  return r.results.first;
}

// ------------------------------------------------------------ boot

async function boot() {
  const m = location.hash.match(/token=([0-9a-f]{32})/);
  if (m) {
    sessionStorage.setItem('zsettle-token', m[1]);
    history.replaceState(null, '', location.pathname);
  }
  S.token = sessionStorage.getItem('zsettle-token');
  if (!S.token) {
    return render(
      h('div', { class: 'welcome' }, h('div', { class: 'card stack' }, h('p', { class: 'hero' }, 'zsettle'), h('p', null, 'Open the link that zsettle-app printed in its terminal. It carries the key for this session.'))),
    );
  }
  await refresh();
}

function render(node) {
  document.getElementById('app').replaceChildren(node);
}

async function refresh() {
  try {
    S.status = await api('/api/status');
  } catch (e) {
    return render(h('div', { class: 'welcome' }, h('div', { class: 'card' }, h('p', { class: 'err' }, e.message))));
  }
  if (!S.status.wallet) return render(viewCreate());
  if (!S.status.unlocked) return render(viewUnlock());
  await loadTab();
}

async function loadTab() {
  try {
    if (S.tab === 'wallet') S.wallet = await api('/api/wallet');
    if (S.tab === 'sell') {
      S.wallet = await api('/api/wallet');
      S.shop = await api('/api/shop');
    }
    if (S.tab === 'activity') S.activity = await api('/api/activity');
  } catch (e) {
    toast(e.message);
  }
  render(viewShell());
  clearInterval(S.shopTimer);
  if (S.tab === 'sell') S.shopTimer = setInterval(updateFeed, 3000);
}

// ------------------------------------------------------------ create and unlock

function viewCreate() {
  const net = h('select', null, h('option', { value: 'test' }, 'Testnet (free test coins)'), h('option', { value: 'main' }, 'Mainnet (real ZEC)'));
  const err = h('div', { class: 'err small' });
  const pass1 = h('input', { type: 'password', placeholder: 'At least ten characters', autocomplete: 'new-password' });
  const pass2 = h('input', { type: 'password', placeholder: 'Repeat it', autocomplete: 'new-password' });
  const passBox = h('div', { class: 'stack', hidden: true }, h('div', null, h('label', null, 'Passphrase'), pass1), h('div', null, h('label', null, 'Repeat'), pass2), h('button', { class: 'btn', onclick: () => create(null) }, 'Create with this passphrase'));

  async function create(viaPasskey) {
    err.textContent = '';
    let body = { network: net.value };
    try {
      if (viaPasskey) {
        const { pk, passphrase } = await passkeyCreate();
        body = { ...body, passphrase, passkey: pk };
      } else {
        if (pass1.value !== pass2.value) throw new Error('The passphrases differ.');
        body.passphrase = pass1.value;
      }
      const r = await api('/api/wallet/create', body);
      S.tab = 'wallet';
      await refresh();
      showReceive(r.fund, true);
    } catch (e) {
      err.textContent = e.message;
    }
  }

  return h(
    'div',
    { class: 'welcome' },
    h(
      'div',
      { class: 'card stack' },
      h('p', { class: 'hero' }, 'Trade on Zcash, privately'),
      h('p', { class: 'muted' }, 'Items and ZEC change hands in one shielded transaction that both sides sign. No escrow, no one in the middle.'),
      h('div', null, h('label', null, 'Network'), net),
      h('button', { class: 'btn primary', onclick: () => create(true), disabled: !passkeyUsable() }, 'Create wallet with a passkey'),
      passkeyUsable() ? null : h('p', { class: 'small muted' }, 'Passkeys need this page opened as http://localhost.'),
      h('button', { class: 'link', onclick: () => (passBox.hidden = !passBox.hidden) }, 'Use a passphrase instead'),
      passBox,
      err,
      h('p', { class: 'note' }, 'The wallet lives on this computer only. A passkey unlocks it with your fingerprint, face or device PIN; lose the passkey and the funds are gone, so keep amounts small.'),
    ),
  );
}

function viewUnlock() {
  const err = h('div', { class: 'err small' });
  const pass = h('input', { type: 'password', placeholder: 'Passphrase', autocomplete: 'current-password' });
  const pk = S.status.passkey;
  async function unlock(viaPasskey) {
    err.textContent = '';
    try {
      const passphrase = viaPasskey ? 'passkey:' + hex(await passkeySecret(pk)) : pass.value;
      await api('/api/unlock', { passphrase });
      await refresh();
    } catch (e) {
      err.textContent = e.message;
    }
  }
  pass.addEventListener('keydown', (e) => e.key === 'Enter' && unlock(false));
  return h(
    'div',
    { class: 'welcome' },
    h(
      'div',
      { class: 'card stack' },
      h('p', { class: 'hero' }, 'Unlock'),
      h('p', { class: 'muted' }, 'Profile ' + S.status.profile + ' · ' + (S.status.network === 'main' ? 'Mainnet' : 'Testnet')),
      pk ? h('button', { class: 'btn primary', onclick: () => unlock(true), disabled: !passkeyUsable() }, 'Unlock with passkey') : null,
      pk ? null : h('div', { class: 'stack' }, pass, h('button', { class: 'btn primary', onclick: () => unlock(false) }, 'Unlock')),
      err,
    ),
  );
}

// ------------------------------------------------------------ shell

function viewShell() {
  const st = S.status;
  const synced = st.last_sync && st.last_sync.report;
  const tabs = [
    ['wallet', 'Wallet'],
    ['market', 'Market'],
    ['sell', 'Sell'],
    ['activity', 'Activity'],
  ];
  let body;
  if (S.tab === 'wallet') body = viewWallet();
  else if (S.tab === 'market') body = viewMarket();
  else if (S.tab === 'sell') body = viewSell();
  else body = viewActivity();
  return h(
    'div',
    null,
    h(
      'div',
      { class: 'top' },
      h('div', { class: 'brand' }, 'z', h('span', null, 'settle')),
      h('span', { class: 'pill ' + st.network }, st.network === 'main' ? 'Mainnet' : 'Testnet'),
      h('span', { class: 'pill' }, st.profile),
      h('span', { class: 'spacer' }),
      h('span', { class: 'small muted' }, synced ? 'Synced ' + ago(st.last_sync.at) : 'Not synced yet'),
      h('button', { class: 'btn', onclick: doSync }, 'Sync'),
      h('button', { class: 'btn ghost', onclick: lock }, 'Lock'),
    ),
    h('div', { class: 'tabs' }, tabs.map(([k, label]) => h('button', { class: 'tab' + (S.tab === k ? ' on' : ''), onclick: () => switchTab(k) }, label))),
    h('main', null, h('div', { class: 'sheet' }, body)),
  );
}

function switchTab(k) {
  S.tab = k;
  loadTab();
}

async function doSync() {
  await runJob('Sync', '/api/sync', {}, { done: (r) => 'Up to date' + (r && r.scanned_to ? ' at block ' + r.scanned_to : '') + '.' });
  await refresh();
}

async function lock() {
  await api('/api/lock', {});
  await refresh();
}

// ------------------------------------------------------------ wallet tab

function viewWallet() {
  const w = S.wallet || { balance_zat: 0, notes: [], items: [] };
  const pending = w.notes.filter((n) => !n.mined).length;
  return h(
    'div',
    null,
    h(
      'div',
      { class: 'cols' },
      h(
        'div',
        { class: 'card stack' },
        h('div', { class: 'muted small' }, 'Balance'),
        h('div', { class: 'balance' }, zec(w.balance_zat)),
        pending ? h('div', { class: 'small muted' }, pending + ' incoming, not mined yet') : null,
        h('p', { class: 'small muted' }, 'Money from another wallet can be spent after ten confirmations, about 13 minutes on mainnet.'),
        h('div', { class: 'row' }, h('button', { class: 'btn primary', onclick: () => showReceive() }, 'Receive'), h('button', { class: 'btn', onclick: showSend }, 'Send')),
      ),
      h(
        'div',
        { class: 'card' },
        h('h3', null, 'Notes'),
        w.notes.length
          ? h('table', null, h('tr', null, h('th', null, 'Note'), h('th', null, 'Amount'), h('th', null, 'Block')), w.notes.map((n) => h('tr', null, h('td', { class: 'mono' }, short(n.note, 6)), h('td', null, zec(n.value_zat)), h('td', null, n.mined || 'pending'))))
          : h('p', { class: 'muted small' }, 'Nothing yet. Press Receive and send ZEC to that address.'),
      ),
    ),
    h('div', { class: 'section' }, h('h2', null, 'My items'), itemGrid(w.items, (it) => [h('button', { class: 'btn', onclick: () => showHistory(null, it.note, it.name) }, 'History')])),
  );
}

function itemGrid(items, actions) {
  if (!items || !items.length) return h('p', { class: 'muted' }, 'No items yet.');
  return h(
    'div',
    { class: 'grid' },
    items.map((it) =>
      h(
        'div',
        { class: 'card item' },
        art(it.name || it.note, it.name || '#'),
        h('h3', null, it.name || 'Unnamed item'),
        h('div', { class: 'mono muted' }, short(it.note, 8)),
        it.listed_zat ? h('div', null, h('span', { class: 'badge' }, 'Listed · ' + zec(it.listed_zat))) : null,
        !it.mined ? h('div', null, h('span', { class: 'badge warn' }, 'Not mined yet')) : null,
        h('div', { class: 'row' }, actions(it)),
      ),
    ),
  );
}

async function showReceive(given, first) {
  let addr = given;
  if (!addr) {
    try {
      addr = (await api('/api/address', {})).address;
    } catch (e) {
      return toast(e.message);
    }
  }
  const m = modal(first ? 'Your wallet is ready' : 'Receive ZEC', [
    first ? h('p', null, 'Fund it by sending ZEC to this address from any Zcash wallet.') : h('p', null, 'A fresh address: each one looks unrelated to the others.'),
    h('div', { class: 'card mono' }, addr),
    h('div', { class: 'row' }, h('button', { class: 'btn primary', onclick: () => copy(addr) }, 'Copy address')),
    h('p', { class: 'note' }, S.status.network === 'test' ? 'Testnet coins are free: paste this address into a Zcash testnet faucet.' : 'Only shielded (Orchard) payments reach this address.'),
  ]);
  m.closable();
}

function showSend() {
  const to = h('input', { placeholder: 'Unified address (u1… or utest1…)' });
  const amount = h('input', { placeholder: '0.001' });
  const memo = h('input', { placeholder: 'Optional message, encrypted for the recipient' });
  const err = h('div', { class: 'err small' });
  const m = modal('Send ZEC', [
    h('div', { class: 'stack' }, h('div', null, h('label', null, 'To'), to), h('div', null, h('label', null, 'Amount in ZEC'), amount), h('div', null, h('label', null, 'Memo'), memo), err),
    h(
      'div',
      { class: 'row' },
      h(
        'button',
        {
          class: 'btn primary',
          onclick: async () => {
            if (!to.value.trim() || !amount.value.trim()) return (err.textContent = 'Fill in the address and the amount.');
            m.close();
            const amt = amount.value.trim();
            await runJob('Sending ZEC', '/api/pay', { to: to.value.trim(), amount: /^[0-9]+$/.test(amt) ? amt + ' zec' : amt, memo: memo.value }, { done: (r) => 'Sent. Transaction ' + short(r.txid) });
            await refresh();
          },
        },
        'Send',
      ),
    ),
  ]);
  m.closable();
}

// ------------------------------------------------------------ market tab

function viewMarket() {
  const url = h('input', { placeholder: 'Seller address, e.g. http://127.0.0.1:8900', value: S.market ? S.market.base : '' });
  const err = h('div', { class: 'err small' });
  async function open(u) {
    err.textContent = '';
    try {
      S.market = await api('/api/market', { url: u || url.value });
      S.status = await api('/api/status');
      render(viewShell());
    } catch (e) {
      err.textContent = e.message;
    }
  }
  url.addEventListener('keydown', (e) => e.key === 'Enter' && open());
  const sellers = (S.status.sellers || []).filter((s) => !S.market || s !== S.market.base);
  return h(
    'div',
    null,
    h('div', { class: 'row' }, h('div', { style: null, class: 'spacer' }, url), h('button', { class: 'btn primary', onclick: () => open() }, 'Open shop')),
    sellers.length ? h('div', { class: 'chips', style: null }, h('span', { class: 'small muted' }, 'Recent:'), sellers.map((s) => h('span', { class: 'chip', onclick: () => open(s) }, s))) : null,
    err,
    S.market ? viewShop() : h('p', { class: 'muted section' }, 'Open a seller’s shop by its address. The page checks that the seller signed the list, then shows what is for sale.'),
  );
}

function viewShop() {
  const m = S.market;
  const wrongNet = S.status.network && m.network !== S.status.network;
  return h(
    'div',
    { class: 'section' },
    h(
      'div',
      { class: 'card row' },
      h('span', { class: 'badge' }, '✓ Signed by the seller'),
      h('span', { class: 'small muted' }, 'Key ' + m.fingerprint + ' · listed at block ' + m.issued_at + ' · ' + (m.network === 'main' ? 'Mainnet' : 'Testnet')),
      wrongNet ? h('span', { class: 'badge warn' }, 'Other network than your wallet') : null,
    ),
    h('p', { class: 'note' }, 'When you buy, the seller sees the price and the moment of the sale. It never sees your address or your balance.'),
    m.items.length
      ? itemGrid(
          m.items.map((i) => ({ ...i, mined: true })),
          (it) => [
            h('div', { class: 'price' }, zec(it.price_zat)),
            h('span', { class: 'spacer' }),
            h('button', { class: 'btn', onclick: () => showHistory(m.base, it.note, it.name) }, 'History'),
            h('button', { class: 'btn primary', onclick: () => confirmBuy(it), disabled: wrongNet }, 'Buy'),
          ],
        )
      : h('p', { class: 'muted' }, 'Nothing for sale right now.'),
  );
}

function confirmBuy(it) {
  const m = modal('Buy ' + (it.name || 'this item'), [
    h('div', { class: 'card stack' }, h('div', { class: 'row' }, h('span', null, 'Price'), h('span', { class: 'spacer' }), h('b', null, zec(it.price_zat))), h('div', { class: 'row small muted' }, h('span', null, 'Network fee'), h('span', { class: 'spacer' }), h('span', null, 'about 0.00015 ZEC'))),
    h('p', { class: 'note' }, 'Your payment and the item move in one transaction. If the seller sold the item to someone else first, nothing happens and your ZEC stays with you.'),
    h(
      'div',
      { class: 'row' },
      h('span', { class: 'spacer' }),
      h(
        'button',
        {
          class: 'btn primary',
          onclick: async () => {
            m.close();
            const r = await runJob('Buying ' + (it.name || 'item'), '/api/buy', { url: S.market.base, item: it.note, name: it.name }, {
              done: (r) => 'Done. The item is yours once the transaction is mined (about a minute). Transaction ' + short(r.txid),
            });
            if (r) toast('Bought');
          },
        },
        'Confirm and buy',
      ),
    ),
  ]);
  m.closable();
}

async function showHistory(base, note, name) {
  await runJob('History of ' + (name || 'item'), '/api/verify', base ? { url: base, item: note } : { item: note }, {
    intro: 'Read from the chain with the sellers’ viewing keys only. Nobody is asked who owns what.',
    done: (r) => {
      const hops = r.hops || [];
      const last = hops[hops.length - 1];
      if (!base && last && last.txid === String(note).split(':')[0]) return 'It is in your wallet now.';
      return statusText(r.status);
    },
    render: (r) =>
      h(
        'div',
        { class: 'timeline' },
        (r.hops || []).map((hop, i) =>
          h('div', { class: 'hop' }, h('div', null, h('b', null, i === 0 ? 'Created' : 'Changed hands'), ' · block ' + hop.height), h('div', { class: 'mono muted' }, short(hop.txid, 10)), h('div', { class: 'small muted' }, hop.holder ? 'held under ' + String(hop.holder).split('/').pop() : 'held by someone whose key you do not have')),
        ),
      ),
  });
}

function statusText(s) {
  const st = s && (s.state || s);
  if (st === 'held') return 'Still held by the last known owner.';
  if (st === 'transferred_to_unknown_holder') return 'Sold on to someone whose listings you do not have.';
  if (st === 'lineage_broken') return 'The trail stops: a transfer did not follow the rules.';
  return 'Traced.';
}

// ------------------------------------------------------------ sell tab

function viewSell() {
  const w = S.wallet || { items: [] };
  const shop = S.shop || { running: false, events: [] };
  const listen = h('input', { value: shop.listen || S.status.shop.listen || '127.0.0.1:8900' });
  const names = h('textarea', { placeholder: 'One item name per line, e.g.\nGolden ticket #1\nGolden ticket #2' });
  const shopUrl = 'http://' + (shop.listen || listen.value).replace('0.0.0.0', location.hostname);
  return h(
    'div',
    null,
    h(
      'div',
      { class: 'cols' },
      h(
        'div',
        { class: 'card stack' },
        h('h3', null, 'Your shop'),
        h('div', { class: 'row' }, h('span', { class: shop.running ? 'badge' : 'badge warn' }, shop.running ? 'Open' : 'Closed'), shop.running ? h('span', { class: 'mono small' }, shopUrl) : null, shop.running ? h('button', { class: 'btn ghost', onclick: () => copy(shopUrl) }, 'Copy') : null),
        h('div', null, h('label', null, 'Listen on (0.0.0.0 to let other machines in)'), listen),
        h('div', { class: 'row' }, shop.running ? h('button', { class: 'btn', onclick: () => shopCmd('/api/shop/stop', {}) }, 'Close shop') : h('button', { class: 'btn primary', onclick: () => shopCmd('/api/shop/start', { listen: listen.value }) }, 'Open shop')),
        h('p', { class: 'small muted' }, 'While open, the shop checks every offer, signs only the item’s half of the trade, and takes sold items off the list by itself.'),
      ),
      h('div', { class: 'card stack' }, h('h3', null, 'Create items'), names, h('button', { class: 'btn primary', onclick: () => mint(names.value) }, 'Mint'), h('p', { class: 'small muted' }, 'Each item is a real shielded note of 0.00001 ZEC, named in its encrypted memo. Minting costs about 0.0001 to 0.0002 ZEC.')),
    ),
    h(
      'div',
      { class: 'section' },
      h('h2', null, 'Items'),
      itemGrid(w.items, (it) =>
        it.listed_zat
          ? [h('button', { class: 'btn', onclick: () => job('Delisting', '/api/delist', { note: it.note }) }, 'Delist'), h('button', { class: 'btn danger', onclick: () => cancelItem(it) }, 'Cancel offers')]
          : [priceBox(it)],
      ),
    ),
    h('div', { class: 'section' }, h('h2', null, 'Offers'), h('div', { class: 'card feed', id: 'feed' }, feedRows(shop.events))),
  );
}

function priceBox(it) {
  const price = h('input', { placeholder: 'Price in ZEC', value: '' });
  return h(
    'div',
    { class: 'row' },
    price,
    h(
      'button',
      {
        class: 'btn primary',
        disabled: !it.mined,
        onclick: () => {
          const p = price.value.trim();
          if (!p) return toast('Set a price first');
          job('Listing ' + (it.name || 'item'), '/api/publish', { note: it.note, name: it.name, price: /^[0-9.]+$/.test(p) ? p + ' zec' : p });
        },
      },
      'List',
    ),
  );
}

async function job(title, path, body) {
  await runJob(title, path, body);
  await loadTab();
}

async function mint(text) {
  const names = text.split('\n').map((s) => s.trim()).filter(Boolean);
  if (!names.length) return toast('Write at least one name');
  await runJob('Minting ' + names.length + ' item' + (names.length > 1 ? 's' : ''), '/api/mint', { names }, { done: () => 'Minted. The items can be listed once the block is mined, in about a minute.' });
  await loadTab();
}

function cancelItem(it) {
  const m = modal('Cancel offers on ' + (it.name || 'this item'), [
    h('p', null, 'Signed offers stay valid for up to 40 blocks. Cancelling moves the item to a fresh note of your own, so none of them can settle. It costs a network fee.'),
    h('div', { class: 'row' }, h('span', { class: 'spacer' }), h('button', { class: 'btn danger', onclick: () => { m.close(); job('Cancelling offers', '/api/cancel', { note: it.note }); } }, 'Cancel offers')),
  ]);
  m.closable();
}

async function shopCmd(path, body) {
  try {
    S.shop = await api(path, body);
    S.status = await api('/api/status');
  } catch (e) {
    toast(e.message);
  }
  render(viewShell());
}

const EVENT_TEXT = {
  start: () => 'Shop started',
  listening: (e) => 'Listening on ' + (e.address || ''),
  signed: (e) => 'Offer accepted and signed' + (e.name ? ' for ' + e.name : '') + (e.paid_zat ? ' · ' + zec(e.paid_zat) : ''),
  sold: (e) => 'Sold' + (e.name ? ': ' + e.name : '') + ' · removed from the list',
  rejected: (e) => 'Offer refused: ' + (e.reason || ''),
  deferred: () => 'Offer waiting for the next block',
  error: (e) => 'Problem: ' + (e.error || ''),
  stopped: () => 'Shop closed',
};

function feedRows(events) {
  const rows = (events || []).filter((e) => EVENT_TEXT[e.event]).slice(-60).reverse();
  if (!rows.length) return h('p', { class: 'muted small' }, 'No offers yet.');
  return rows.map((e) => h('div', { class: 'ev' }, h('span', { class: 'muted' }, ago(e.at) + ' · '), EVENT_TEXT[e.event](e)));
}

async function updateFeed() {
  if (S.tab !== 'sell' || document.getElementById('layer').children.length) return;
  try {
    const shop = await api('/api/shop');
    const changed = !S.shop || shop.events.length !== S.shop.events.length || shop.running !== S.shop.running;
    S.shop = shop;
    const feed = document.getElementById('feed');
    if (feed) feed.replaceChildren(...[feedRows(shop.events)].flat());
    if (changed && shop.events.some((e) => e.event === 'sold')) {
      S.wallet = await api('/api/wallet');
    }
  } catch {
    /* the next tick tries again */
  }
}

// ------------------------------------------------------------ activity tab

function viewActivity() {
  if (!S.activity.length) return h('p', { class: 'muted' }, 'Nothing yet.');
  return h(
    'table',
    null,
    h('tr', null, h('th', null, 'When'), h('th', null, 'What'), h('th', null, 'Amount'), h('th', null, 'Transaction')),
    S.activity.map((a) =>
      h(
        'tr',
        null,
        h('td', { class: 'muted' }, ago(a.at)),
        h('td', null, a.text || a.kind),
        h('td', null, a.price_zat ? zec(a.price_zat) : a.amount_zat ? zec(a.amount_zat) : ''),
        h('td', { class: 'mono' }, a.txid ? h('button', { class: 'link', onclick: () => copy(a.txid) }, short(a.txid, 6)) : ''),
      ),
    ),
  );
}

boot();
