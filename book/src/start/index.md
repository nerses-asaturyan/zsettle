# Quick start

There are three ways to try zsettle:

1. [Use the hosted testnet demo](demo.md). Nothing to install: the wallet runs in your
   browser and keeps the keys there. Test coins are free.
2. [Run the app on your own computer](app.md). You keep your own wallet. First
   [install or build](install.md) the programs.
3. [Do a trade on the command line](first-trade.md), with two wallets in two terminals.

If you only want to see that it works, you can trace a real mainnet item without a wallet.
From a release archive or a checkout of the repository:

```
zsettle list   --listings examples/market.json
zsettle verify --listings examples/market.json --listings examples/market2.json --item af09c2cf525d2bb6e553b61a1d3cc2674ebdf09726be1e3959ee36f5143f5804:0
```

The two signed listings files in `examples/` come from the mainnet trades listed in
[Project status](../reference/status.md). `verify` reads mainnet compact blocks from the
default light server and needs nothing else.
