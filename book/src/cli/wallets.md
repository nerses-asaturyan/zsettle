# Wallets

## Create a wallet

```
zsettle-seller init --wallet ~/.zsettle/seller --identity ~/.zsettle/seller.key
zsettle        init --wallet ~/.zsettle/buyer  --identity ~/.zsettle/buyer.key
```

Each `init`:

1. makes a new 24-word mnemonic;
2. encrypts it to an age identity at the `--identity` path (the file is created if it does not
   exist), or to a passphrase if `--identity` is left out;
3. writes `keys.toml` and creates the wallet database `data.sqlite`;
4. derives two accounts:
   - index 0 pays and receives payments: `payout` in a seller wallet, `buyer` in a buyer
     wallet;
   - index 1 holds items: `item`.
5. prints the account UUIDs and a fresh address to fund (`fund`), and writes `env.sh` and
   `env.ps1`.

Items live in their own ZIP 32 account, so that publishing that account's viewing key says
nothing about the rest of the wallet.

`--network` is `main` (default) or `test`. The wallet birthday is set a little below the
server's tip and rounded down to a multiple of 1,000 blocks, so the first sync is short and
the birthday is not unique to one wallet.

**Keep the key file, and keep it apart from the wallet directory and its backups.** It is the
only way to decrypt the mnemonic, and the mnemonic is the only backup of the funds.

### Passphrase instead of a key file

Leave out `--identity`. `init` asks for a passphrase twice, or reads `ZSETTLE_PASSPHRASE`.
Every later command that needs the mnemonic asks again, or reads `ZSETTLE_PASSPHRASE`. There
is then no file to lose, only a passphrase to remember. zcash-devtool cannot open
passphrase-protected wallets.

### zcash-devtool

The wallet layout is the one of [zcash-devtool](https://github.com/zcash/zcash-devtool):
`data.sqlite` and `keys.toml` with an age-encrypted mnemonic. Devtool can open wallets made
with a key file, and wallets made with devtool work here.

## Accounts and addresses

```
zsettle accounts --wallet ~/.zsettle/buyer
zsettle address  --wallet ~/.zsettle/buyer --account <UUID>
```

`accounts` lists name, UUID, ZIP 32 index and birthday as JSON. An account imported from a
viewing key has no index. `address` prints a fresh Orchard-only receiving address, a new one
on each call: reusing one address links payments. `zsettle-seller` has the same two commands.

`zsettle-seller new-item-account --name <name>` adds another item account, when an item
should have a consignment of its own (see [Selling](seller.md#one-account-per-item)).

## Sync

```
zsettle sync --wallet ~/.zsettle/buyer
zsettle-seller sync --wallet ~/.zsettle/seller
```

This is the `zcash_client_backend` synchronizer with an in-memory block cache. It refreshes
the commitment-tree roots and the chain tip, downloads the block ranges the wallet still needs
in batches of 500, scans them and drops them. Nothing besides the wallet database is written.
The seller service runs the same sync before every pass.

## Fund the wallet

Send ZEC to the address `init` printed, or to a fresh one from `address`.

- A note received from another wallet becomes spendable after ten confirmations (about 13
  minutes on mainnet before NU7, about 4 minutes from NU7 and on the testnet). The wallet's
  own change needs three.
- `mint` and `buy` say so when you ask too early.

What things cost (ZIP 317 fees, 5,000 zatoshis per logical action, at least two):

| Action | Fee |
|---|---|
| Trade, paid from a note that leaves change (three actions) | 15,000 zat, plus the price |
| Trade, paid from a note worth exactly price + 10,000 (two actions) | 10,000 zat, plus the price |
| Mint | 5,000 zat per action; the actions are the larger of inputs and outputs (items plus change), at least two. Two items from one note: 15,000 zat plus the item values. |

Prices must be above 5,000 zatoshis: a smaller payment is dust that wallets refuse to spend.
Each item note holds at least 1,000 zatoshis (the default value for `mint`).

## Take money out

```
zsettle-seller pay --to <unified address> --amount 0.001      # from the payout account, with env.sh sourced
zsettle pay --wallet ~/.zsettle/buyer --account <BUYER_ACCOUNT_UUID> --identity ~/.zsettle/buyer.key --to <unified address> --amount 0.001
```

`pay` sends from one account to any unified address with an Orchard receiver, a phone wallet's
included. `--memo` is optional. The same command funds a counterparty's wallet. `pay` refuses
recipients without an Orchard receiver.

## Network upgrades

The consensus branch id is part of what a transaction signs. Before building, minting or
broadcasting, the tools compare the branch the server reports with the one this build signs
for, and stop with a message when they differ. See
[Networks](../reference/networks.md).
