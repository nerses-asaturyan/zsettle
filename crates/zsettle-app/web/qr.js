'use strict';
// A small QR code encoder (byte or alphanumeric mode, versions 1-40, error correction M by
// default), following the ISO/IEC 18004 construction. No dependencies; checked against a reference
// encoder module by module. QR.matrix(text, { ecl, mask }) returns rows of booleans;
// QR.svg(text) returns an <svg> element.
const QR = (() => {
  const ECL = { L: 0, M: 1, Q: 2, H: 3 };
  const FORMAT = [1, 0, 3, 2]; // format bits for L, M, Q, H
  const ECC_PER_BLOCK = [
    [-1, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28, 30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
    [-1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28],
    [-1, 13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26, 30, 28, 30, 30, 30, 30, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
    [-1, 17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26, 28, 30, 24, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
  ];
  const BLOCKS = [
    [-1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25],
    [-1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49],
    [-1, 1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23, 23, 25, 27, 29, 34, 34, 35, 38, 40, 43, 45, 48, 51, 53, 56, 59, 62, 65, 68],
    [-1, 1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25, 34, 30, 32, 35, 37, 40, 42, 45, 48, 51, 54, 57, 60, 63, 66, 70, 74, 77, 81],
  ];

  const bit = (x, i) => ((x >>> i) & 1) !== 0;

  function rawModules(ver) {
    let r = (16 * ver + 128) * ver + 64;
    if (ver >= 2) {
      const n = Math.floor(ver / 7) + 2;
      r -= (25 * n - 10) * n - 55;
      if (ver >= 7) r -= 36;
    }
    return r;
  }
  const dataCodewords = (ver, e) => Math.floor(rawModules(ver) / 8) - ECC_PER_BLOCK[e][ver] * BLOCKS[e][ver];

  // GF(2^8) with the polynomial 0x11D.
  function mul(x, y) {
    let z = 0;
    for (let i = 7; i >= 0; i--) {
      z = (z << 1) ^ ((z >>> 7) * 0x11d);
      z ^= ((y >>> i) & 1) * x;
    }
    return z;
  }
  function divisor(degree) {
    const r = new Array(degree).fill(0);
    r[degree - 1] = 1;
    let root = 1;
    for (let i = 0; i < degree; i++) {
      for (let j = 0; j < r.length; j++) {
        r[j] = mul(r[j], root);
        if (j + 1 < r.length) r[j] ^= r[j + 1];
      }
      root = mul(root, 0x02);
    }
    return r;
  }
  function remainder(data, div) {
    const r = div.map(() => 0);
    for (const b of data) {
      const f = b ^ r.shift();
      r.push(0);
      div.forEach((c, i) => (r[i] ^= mul(c, f)));
    }
    return r;
  }

  // Text made only of capitals, digits and these nine marks goes in alphanumeric mode, two
  // characters in 11 bits; any other text in byte mode, a byte in 8 bits.
  const ALNUM = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:';
  function segment(text) {
    if (/^[0-9A-Z $%*+\-./:]*$/.test(text)) {
      const n = text.length;
      return {
        mode: 2,
        count: n,
        countBits: (ver) => (ver < 10 ? 9 : ver < 27 ? 11 : 13),
        bits: 11 * Math.floor(n / 2) + 6 * (n % 2),
        write(put) {
          for (let i = 0; i + 1 < n; i += 2) put(ALNUM.indexOf(text[i]) * 45 + ALNUM.indexOf(text[i + 1]), 11);
          if (n % 2) put(ALNUM.indexOf(text[n - 1]), 6);
        },
      };
    }
    const bytes = Array.from(new TextEncoder().encode(text));
    return {
      mode: 4,
      count: bytes.length,
      countBits: (ver) => (ver < 10 ? 8 : 16),
      bits: 8 * bytes.length,
      write(put) {
        for (const b of bytes) put(b, 8);
      },
    };
  }

  function encode(seg, e, forceVersion) {
    let ver = forceVersion || 1;
    for (; ; ver++) {
      if (ver > 40) throw new Error('too much data for a QR code');
      const bits = 4 + seg.countBits(ver) + seg.bits;
      if (bits <= dataCodewords(ver, e) * 8) break;
      if (forceVersion) throw new Error('data does not fit the requested version');
    }
    const bb = [];
    const put = (val, len) => {
      for (let i = len - 1; i >= 0; i--) bb.push((val >>> i) & 1);
    };
    put(seg.mode, 4);
    put(seg.count, seg.countBits(ver));
    seg.write(put);
    const cap = dataCodewords(ver, e) * 8;
    put(0, Math.min(4, cap - bb.length));
    put(0, (8 - (bb.length % 8)) % 8);
    for (let pad = 0xec; bb.length < cap; pad ^= 0xec ^ 0x11) put(pad, 8);
    const data = [];
    for (let i = 0; i < bb.length; i += 8) data.push(bb.slice(i, i + 8).reduce((a, b) => (a << 1) | b, 0));

    // Split into blocks, add Reed-Solomon codewords, interleave.
    const nBlocks = BLOCKS[e][ver];
    const eccLen = ECC_PER_BLOCK[e][ver];
    const raw = Math.floor(rawModules(ver) / 8);
    const nShort = nBlocks - (raw % nBlocks);
    const shortLen = Math.floor(raw / nBlocks);
    const div = divisor(eccLen);
    const blocks = [];
    for (let i = 0, k = 0; i < nBlocks; i++) {
      const dat = data.slice(k, k + shortLen - eccLen + (i < nShort ? 0 : 1));
      k += dat.length;
      const ecc = remainder(dat, div);
      if (i < nShort) dat.push(0);
      blocks.push(dat.concat(ecc));
    }
    const out = [];
    for (let i = 0; i < blocks[0].length; i++) {
      blocks.forEach((b, j) => {
        if (i !== shortLen - eccLen || j >= nShort) out.push(b[i]);
      });
    }
    return { ver, codewords: out };
  }

  function build(ver, e, codewords, maskChoice) {
    const size = ver * 4 + 17;
    const m = Array.from({ length: size }, () => new Array(size).fill(false));
    const fn = Array.from({ length: size }, () => new Array(size).fill(false));
    const set = (x, y, dark) => {
      m[y][x] = dark;
      fn[y][x] = true;
    };
    for (let i = 0; i < size; i++) {
      set(6, i, i % 2 === 0);
      set(i, 6, i % 2 === 0);
    }
    const finder = (cx, cy) => {
      for (let dy = -4; dy <= 4; dy++) {
        for (let dx = -4; dx <= 4; dx++) {
          const d = Math.max(Math.abs(dx), Math.abs(dy));
          const x = cx + dx;
          const y = cy + dy;
          if (x >= 0 && x < size && y >= 0 && y < size) set(x, y, d !== 2 && d !== 4);
        }
      }
    };
    finder(3, 3);
    finder(size - 4, 3);
    finder(3, size - 4);
    const align = [];
    if (ver > 1) {
      const n = Math.floor(ver / 7) + 2;
      const step = ver === 32 ? 26 : Math.ceil((ver * 4 + 4) / (n * 2 - 2)) * 2;
      align.push(6);
      for (let pos = size - 7; align.length < n; pos -= step) align.splice(1, 0, pos);
    }
    const last = align.length - 1;
    align.forEach((ax, i) =>
      align.forEach((ay, j) => {
        if ((i === 0 && j === 0) || (i === 0 && j === last) || (i === last && j === 0)) return;
        for (let dy = -2; dy <= 2; dy++) for (let dx = -2; dx <= 2; dx++) set(ax + dx, ay + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
      }),
    );
    const format = (mask) => {
      const d = (FORMAT[e] << 3) | mask;
      let r = d;
      for (let i = 0; i < 10; i++) r = (r << 1) ^ ((r >>> 9) * 0x537);
      const bits = ((d << 10) | r) ^ 0x5412;
      for (let i = 0; i <= 5; i++) set(8, i, bit(bits, i));
      set(8, 7, bit(bits, 6));
      set(8, 8, bit(bits, 7));
      set(7, 8, bit(bits, 8));
      for (let i = 9; i < 15; i++) set(14 - i, 8, bit(bits, i));
      for (let i = 0; i < 8; i++) set(size - 1 - i, 8, bit(bits, i));
      for (let i = 8; i < 15; i++) set(8, size - 15 + i, bit(bits, i));
      set(8, size - 8, true);
    };
    format(0);
    if (ver >= 7) {
      let r = ver;
      for (let i = 0; i < 12; i++) r = (r << 1) ^ ((r >>> 11) * 0x1f25);
      const bits = (ver << 12) | r;
      for (let i = 0; i < 18; i++) {
        const a = size - 11 + (i % 3);
        const b = Math.floor(i / 3);
        set(a, b, bit(bits, i));
        set(b, a, bit(bits, i));
      }
    }
    // Data, in the zig-zag order.
    let i = 0;
    for (let right = size - 1; right >= 1; right -= 2) {
      if (right === 6) right = 5;
      for (let v = 0; v < size; v++) {
        for (let j = 0; j < 2; j++) {
          const x = right - j;
          const up = ((right + 1) & 2) === 0;
          const y = up ? size - 1 - v : v;
          if (!fn[y][x] && i < codewords.length * 8) {
            m[y][x] = bit(codewords[i >>> 3], 7 - (i & 7));
            i++;
          }
        }
      }
    }
    const maskFn = [
      (x, y) => (x + y) % 2 === 0,
      (x, y) => y % 2 === 0,
      (x) => x % 3 === 0,
      (x, y) => (x + y) % 3 === 0,
      (x, y) => (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0,
      (x, y) => ((x * y) % 2) + ((x * y) % 3) === 0,
      (x, y) => (((x * y) % 2) + ((x * y) % 3)) % 2 === 0,
      (x, y) => (((x + y) % 2) + ((x * y) % 3)) % 2 === 0,
    ];
    const apply = (k) => {
      for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) if (!fn[y][x] && maskFn[k](x, y)) m[y][x] = !m[y][x];
    };
    let mask = maskChoice;
    if (mask === undefined) {
      let best = Infinity;
      for (let k = 0; k < 8; k++) {
        apply(k);
        format(k);
        const p = penalty(m);
        if (p < best) {
          best = p;
          mask = k;
        }
        apply(k);
      }
    }
    apply(mask);
    format(mask);
    return m;
  }

  // A simplified penalty (runs, blocks, balance): any mask gives a valid code; this only
  // prefers an easy-to-scan one.
  function penalty(m) {
    const n = m.length;
    let p = 0;
    for (let a = 0; a < n; a++) {
      for (const row of [true, false]) {
        let run = 1;
        for (let b = 1; b < n; b++) {
          const cur = row ? m[a][b] : m[b][a];
          const prev = row ? m[a][b - 1] : m[b - 1][a];
          if (cur === prev) {
            run++;
            if (run === 5) p += 3;
            else if (run > 5) p++;
          } else run = 1;
        }
      }
    }
    let dark = 0;
    for (let y = 0; y < n; y++) {
      for (let x = 0; x < n; x++) {
        if (m[y][x]) dark++;
        if (y < n - 1 && x < n - 1 && m[y][x] === m[y][x + 1] && m[y][x] === m[y + 1][x] && m[y][x] === m[y + 1][x + 1]) p += 3;
      }
    }
    const k = Math.ceil(Math.abs(dark * 20 - n * n * 10) / (n * n)) - 1;
    return p + Math.max(0, k) * 10;
  }

  function matrix(text, opts = {}) {
    const e = ECL[opts.ecl || 'M'];
    const { ver, codewords } = encode(segment(text), e, opts.version);
    return build(ver, e, codewords, opts.mask);
  }

  function svg(text, opts = {}) {
    const m = matrix(text, opts);
    const border = 4;
    const n = m.length + border * 2;
    const NS = 'http://www.w3.org/2000/svg';
    const el = document.createElementNS(NS, 'svg');
    el.setAttribute('viewBox', `0 0 ${n} ${n}`);
    el.setAttribute('shape-rendering', 'crispEdges');
    el.setAttribute('role', 'img');
    el.setAttribute('aria-label', 'QR code of the address');
    const bg = document.createElementNS(NS, 'rect');
    bg.setAttribute('width', n);
    bg.setAttribute('height', n);
    bg.setAttribute('fill', '#ffffff');
    el.append(bg);
    let d = '';
    m.forEach((row, y) => row.forEach((dark, x) => dark && (d += `M${x + border},${y + border}h1v1h-1z`)));
    const path = document.createElementNS(NS, 'path');
    path.setAttribute('d', d);
    path.setAttribute('fill', '#000000');
    el.append(path);
    return el;
  }

  return { matrix, svg };
})();
if (typeof module !== 'undefined') module.exports = QR;
