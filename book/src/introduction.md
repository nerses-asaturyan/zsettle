# zsettle

zsettle trades an item for ZEC in one shielded Zcash transaction. Two parties take part: a
seller who owns the item and a buyer who pays. No one holds the money or the item in the
middle. Either the whole trade lands on chain, or nothing moves.

The item is an ordinary shielded note in the Ironwood pool (NU6.3). The trade is a PCZT
(partially created Zcash transaction) that the buyer builds and proves, the seller checks
and signs for its own half, and the buyer finishes, signs and broadcasts.

This book explains how to use zsettle and how it works:

- [What zsettle is](what-is-zsettle.md): the idea, and one trade in plain words.
- [Quick start](start/index.md): try the hosted testnet demo, or run it yourself.
- [CLI guide](cli/index.md): the `zsettle` (buyer) and `zsettle-seller` (seller) commands.
- [How it works](how/index.md): the transaction, the PCZT flow, items, listings, history and
  the code layout.
- [Privacy](privacy.md) and [Security model and limits](security.md): what each party sees,
  and what the code protects against.
- [Hosting the demo](hosting.md): run the public testnet demo on a server.
- [Reference](reference/index.md): every command and flag, the HTTP endpoints, file formats
  and networks.

## Links

- Source code: <https://github.com/nerses-asaturyan/zsettle>
- Project page: <https://nerses-asaturyan.github.io/zsettle/>
- Releases: <https://github.com/nerses-asaturyan/zsettle/releases>
- Protocol notes in the repository: `spec/PROTOCOL.md` and `spec/THREAT_MODEL.md`

## Status

This is young software. It has not been audited. It has made real trades on mainnet with
small amounts, see [Project status](reference/status.md). Report security problems
privately, as described in `SECURITY.md` in the repository.

zsettle is licensed under MIT OR Apache-2.0, at your option.
