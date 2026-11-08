# Counter with a vault

One Pinocchio 0.12 program on Solana with two features: a **counter** (`Initialize`, `Increment`)
and an SPL Token **vault** owned by that counter (`Deposit`, `Withdraw`). It is built as
**SBPF v3**, called with **v1 transactions** (SIMD-0385), and is the scaffold for the auction
program. [`guide.md`](guide.md) has the full from-scratch setup, version reasoning, and
troubleshooting.

![Client running against devnet: increment 3 -> 4, unauthorized call rejected, all checks passed](client.png)

## Devnet

| | |
|---|---|
| Program ID | [`LmGSyCLnSD14kNmpgvsHzM2fN6dxQ8eUyoVhPtw4Ff2`](https://explorer.solana.com/address/LmGSyCLnSD14kNmpgvsHzM2fN6dxQ8eUyoVhPtw4Ff2?cluster=devnet) |
| Binary | 14,352 bytes, SBPF v3 (`0300 0000` at `0x30`), all four instructions. Same bytes as `target/deploy/auction.so` |
| Upgrade authority | `5GHnVhqZ6Yn8mQmM43CwMRTNWZofeyExMkP6PDGCAc9d` |

To check that devnet runs your local build:

```sh
solana program dump -u devnet LmGSyCLnSD14kNmpgvsHzM2fN6dxQ8eUyoVhPtw4Ff2 /tmp/devnet.so
cmp -n $(stat -f%z target/deploy/auction.so) /tmp/devnet.so target/deploy/auction.so && echo same
```

The dump is padded with zeros after the ELF, so compare only the local file's length.

## Tokens

- **Token program**: the vault is an account of the real SPL Token program
  (`TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`). Since SIMD-0266 ("Efficient Token program",
  feature `ptokFjwyJtrwCa9Kgo9xoDS59V4QccBGEaRFnRPnSdP`, active on devnet since epoch 1044), that
  address runs **p-token**, the Pinocchio rewrite of SPL Token. It accepts the same
  instructions and uses fewer compute units. LiteSVM 0.18 and surfpool 1.7 also run p-token at
  that address.
- **p-token batch instruction**: `Withdraw` sends Transfer and CloseAccount to the Token program
  as one `Batch` CPI (discriminator 255), so the program pays the CPI base cost once. Only
  p-token has `Batch`; the old SPL Token program rejects it.
- **Mints**: the tests and `vault-client` create a new mint on every run (6 decimals, your
  keypair is the mint authority) and mint 1 token. These are real SPL Token accounts, but the
  tokens have no value. Any SPL Token mint works, including USDC.
- **Token-2022 is not supported.** The program requires `token_program == pinocchio_token::ID`.
  Supporting it would mean adding `pinocchio-token-2022` and checking which token program owns
  the vault.

## Versions

Checked against crates.io on 2026-11-08. Every crate is at its latest release.

| Crate | Version | Used by |
|---|---|---|
| `pinocchio` | 0.12.0 | program |
| `pinocchio-system` | 0.7.0 | program (`CreateAccount`) |
| `pinocchio-token` | 0.8.0 | program (`Transfer`, `CloseAccount`, `Batch`, token account reader) |
| `solana-program-log` | 1.2.0 | program |
| `litesvm`, `litesvm-token` | 0.18.2 | `tests/` |
| `solana-rpc-client` | 4.4.1 | `counter-client/`, `vault-client/` |

## Program

Counter PDA seeds: `["counter", authority]`. The vault is any SPL Token account whose owner is
the counter PDA (a plain token account or the PDA's ATA). The client creates the vault; the
program only checks it.

Instruction data is `[discriminator, args...]`. Each processor in `program/src/instructions/`
documents its accounts, arguments, and errors.

| Discriminator | Instruction | Args | Accounts |
|---|---|---|---|
| `0` | `Initialize` | none | `[signer, writable] authority`, `[writable] counter PDA`, `[] system program` |
| `1` | `Increment` | none | `[signer] authority`, `[writable] counter PDA` |
| `2` | `Deposit` | `amount: u64` LE | `[signer] depositor`, `[writable] source`, `[] counter`, `[writable] vault`, `[] token program` |
| `3` | `Withdraw` | none | `[signer, writable] authority`, `[] counter`, `[writable] vault`, `[writable] destination`, `[] token program` |

Anyone can deposit into any counter's vault. Only the counter's authority can withdraw.
`Withdraw` signs as the counter PDA to transfer the whole vault balance to `destination`, then
closes the vault and returns its rent to the authority.

Counter account (48 bytes, alignment 1):

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | discriminator (`1`) |
| 1 | 1 | bump |
| 2 | 6 | padding |
| 8 | 32 | authority |
| 40 | 8 | count (`u64` little-endian) |

Errors (`custom program error: 0x<code>`):

| Code | Name | When |
|---|---|---|
| `0x0` | `Unauthorized` | Signer is not the counter's authority |
| `0x1` | `Overflow` | `count` would exceed `u64::MAX` |
| `0x2` | `InvalidVault` | Vault is not an SPL Token account owned by the counter PDA |
| `0x3` | `ZeroAmount` | Deposit amount is 0 |

## Layout

```
auction/
├── rust-toolchain.toml   host rustc 1.99.1
├── Cargo.toml            workspace: program only
├── program/              on-chain code (pinocchio, no Agave crates)
├── tests/                own workspace + Cargo.lock: LiteSVM 0.18.2, Agave 4.3.x
├── counter-client/       own workspace + Cargo.lock: Initialize + Increment over RPC, Agave 4.4.x
└── vault-client/         own workspace + Cargo.lock: Deposit + Withdraw over RPC, Agave 4.4.x
```

`tests/` and the clients are separate workspaces because LiteSVM and the RPC client need
incompatible Agave versions (`guide.md` §2). Each client has a walkthrough:
[`counter-client/counter.md`](counter-client/counter.md),
[`vault-client/vault.md`](vault-client/vault.md).

## Requirements

| Tool | Version |
|---|---|
| Agave CLI / `cargo build-sbf` | 4.4.0 / 4.5.0 (4.1.2 works with `--arch v3`) |
| platform-tools | v1.58 |
| host rustc | 1.99.1 (pinned by `rust-toolchain.toml`) |
| surfpool | 1.7.0 (local node) |

```sh
agave-install init 4.4.0
curl -sL https://run.surfpool.run/ | bash
```

## Build

```sh
cargo build-sbf --tools-version v1.58 --arch v3
xxd -s 0x30 -l 4 target/deploy/auction.so   # 0300 0000 = v3
```

`target/deploy/auction-keypair.json` is the program ID. `cargo clean` deletes it, so back it up.

## Test

All three environments run the same `.so` with v1 transactions.

| Environment | What runs | Counter | Vault |
|---|---|---|---|
| LiteSVM | `tests/`, 12 tests | 7 tests | 5 tests |
| Local (surfpool) | both clients | `counter-client` | `vault-client` |
| Devnet | both clients | `counter-client` | `vault-client` |

### 1. LiteSVM

```sh
cd tests && cargo test
```

Counter tests: initialize, double initialize, foreign PDA, increment, unauthorized increment,
overflow, and the v1 compute-unit limit. Vault tests: deposit, deposit into a vault the counter
doesn't own, zero deposit, withdraw (tokens out, vault closed, rent returned), and unauthorized
withdraw. Token transactions also load the token program (~100 KB), so they use a 256 KiB
loaded-accounts limit instead of 64 KiB.

### Choosing localnet or devnet

Both clients ask which cluster to use when they start:

```text
Run against:
  1) localnet  http://127.0.0.1:8899 (surfpool)
  2) devnet    DEVNET_RPC_URL from .env
choice [1/2]:
```

- **localnet** prints a warning that surfpool must be running in the background with the program
  deployed. If nothing answers at `127.0.0.1:8899`, the client stops and prints the command that
  starts surfpool.
- **devnet** uses `DEVNET_RPC_URL`. The clients load it from `.env` in the project root; a value
  already set in your shell takes precedence. `.env` is gitignored because provider URLs contain
  an API key, and the clients print only the host, never the full URL.

```sh
cp .env.example .env   # then set DEVNET_RPC_URL
```

Run `counter-client` first on each cluster: it creates the counter PDA, which `vault-client`
needs. The first build of each client compiles about 300 crates.

### 2. Local (surfpool)

```sh
# terminal 1, project root
surfpool start --offline --no-deploy

# terminal 2, project root
solana airdrop -u localhost 100
solana program deploy -u localhost target/deploy/auction.so \
  --program-id target/deploy/auction-keypair.json
(cd counter-client && cargo run -q)   # choose 1
(cd vault-client && cargo run -q)     # choose 1
```

### 3. Devnet

```sh
# only after a program change; the deployed binary already matches the local build
solana program deploy -u devnet target/deploy/auction.so \
  --program-id target/deploy/auction-keypair.json

(cd counter-client && cargo run -q)   # choose 2
(cd vault-client && cargo run -q)     # choose 2
```

Each client prints `all checks passed` or exits non-zero. Each `vault-client` run pays about
0.004 SOL of rent for a new mint and source account; `Withdraw` returns the vault's rent.

### What the clients check

- `counter-client`: initializes the counter (skipped if it already exists), increments it,
  checks the new count, and checks that a different signer is rejected with `0x0`.
- `vault-client`: creates a mint, a source account, and a vault owned by the counter PDA; deposits
  1 token and checks both balances; withdraws and checks that the tokens came back and the vault
  is closed.
- Both read each transaction back and check that it landed as v1 with the expected config.

| Variable | `counter-client` default | `vault-client` default |
|---|---|---|
| `DEVNET_RPC_URL` | none, set in `.env` (devnet only) | none, set in `.env` (devnet only) |
| `KEYPAIR` | `~/.config/solana/id.json` | `~/.config/solana/id.json` |
| `PROGRAM_ID` | pubkey of `target/deploy/auction-keypair.json` | `LmGSyCLnSD14kNmpgvsHzM2fN6dxQ8eUyoVhPtw4Ff2` |

The keypair pays fees and is the counter authority. The screenshot at the top is
`counter-client` on devnet.
