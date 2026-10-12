# Install or build

You need three programs: `zsettle` (buyer), `zsettle-seller` (seller) and, for the web page,
`zsettle-app`. Keep them in the same directory: `zsettle-app` looks for the other two next to
itself (or in `--bin-dir`).

## Release archives

The release workflow builds archives for Linux x86_64, macOS (Apple silicon and Intel) and
Windows x86_64. It runs for every `v*` tag and attaches the archives to a GitHub Release, and
it can also be started by hand from the Actions tab. Each archive holds the three binaries,
`README.md`, `CHANGELOG.md`, the licence files, and the `docs/`, `spec/` and `examples/`
folders.

Releases: <https://github.com/nerses-asaturyan/zsettle/releases>

```
tar xzf zsettle-linux-x86_64.tar.gz    # Windows: unzip zsettle-windows-x86_64.zip
./zsettle --version
```

Release builds come from the `main` branch, so they are for **mainnet**. For the testnet you
need a build of the `nu7-testnet` branch. See
[Networks and the NU7 testnet branch](../reference/networks.md).

## Build from source

The toolchain is pinned in `rust-toolchain.toml` (Rust 1.93.1). With `rustup` installed,
`cargo` picks it up by itself.

```
git clone https://github.com/nerses-asaturyan/zsettle
cd zsettle
cargo build --release --locked
```

The binaries land in `target/release/`: `zsettle`, `zsettle-seller` and `zsettle-app`.
The first build downloads the Sapling parameters for the bundled prover. The Orchard circuit
keys are built when a command first needs them, which takes a few seconds.

For a testnet build, build the `nu7-testnet` branch the same way.

## Check the install

```
zsettle --help
zsettle-seller --help
zsettle-app --help
```

Every command has `--help`, which also shows the environment variable behind each flag.
