# vault-client

`vault-client` is a Rust binary that exercises the **vault side** of the counter program on
devnet: it creates SPL Token accounts, calls `Deposit`, checks the vault balance, calls
`Withdraw`, and checks that the tokens came back and the vault was closed.

`Initialize` and `Increment` are **not** part of this client. They belong to
[`counter-client`](../counter-client/counter.md), which creates the counter PDA that the vault
logic depends on.

- Program (devnet): `LmGSyCLnSD14kNmpgvsHzM2fN6dxQ8eUyoVhPtw4Ff2`
- Source: [`src/main.rs`](src/main.rs)
- Program logic: [`../program/src/instructions/deposit.rs`](../program/src/instructions/deposit.rs),
  [`../program/src/instructions/withdraw.rs`](../program/src/instructions/withdraw.rs),
  [`../program/src/state/vault.rs`](../program/src/state/vault.rs)

---

## 1. Run

```sh
cd vault-client
cargo run -q
```

On start the client asks for the cluster:

| Choice | RPC endpoint | Notes |
|---|---|---|
| `1` localnet | `http://127.0.0.1:8899` | Prints a warning; surfpool must be running with the program deployed, otherwise the client stops with the command to start it |
| `2` devnet | `DEVNET_RPC_URL` | Loaded from `../.env` (copy `../.env.example`); a shell variable wins. Only the host is printed |

| Variable | Default | Meaning |
|---|---|---|
| `KEYPAIR` | `~/.config/solana/id.json` | Fee payer **and** counter authority |
| `PROGRAM_ID` | `LmGSyCLnSD14kNmpgvsHzM2fN6dxQ8eUyoVhPtw4Ff2` | Deployed program |

The payer must already have a counter PDA, i.e. `counter-client` must have run at least once with
the same keypair. If it has not, the client stops with:

```text
counter <PDA> is not initialized; run counter-client first
```

---

## 2. The two accounts that matter

### Counter PDA (read-only here)

```text
counter = find_program_address(["counter", authority], program_id)
```

It stores the counter state (see `counter.md`) and, importantly, it is the **token owner** of the
vault. `vault-client` only reads and validates it; it never initializes or increments it.

### Vault (created by this client)

The vault is **any SPL Token account whose token owner field is the counter PDA**. The program
does not derive it, store it, or care how it was created. It only checks
(`program/src/state/vault.rs`):

1. the account is owned by the SPL Token program,
2. the account data is exactly 165 bytes,
3. `token.owner() == counter PDA`.

That owner field is what makes the vault safe: only the counter PDA can authorize a transfer out
of it, and only this program can sign as the counter PDA (`invoke_signed` with
`["counter", authority, bump]`). Users and the client cannot move the vault's tokens.

Interchangeable options for the vault address:

| Option | Address | Notes |
|---|---|---|
| Plain token account **(what this client does)** | random keypair created with `CreateAccount` | Needs the new account to sign at creation; address is not predictable |
| ATA of the counter PDA | `PDA([counter, TOKEN_PROGRAM, mint], ATA_PROGRAM)` | Deterministic; created through the Associated Token Account program |

Both satisfy the program's check. The current client uses a plain token account to avoid depending
on the ATA program.

---

## 3. End-to-end flow

`main` runs these phases in order. Everything is asserted, so a silent failure is not possible:
any mismatch returns a non-zero exit code.

```text
0. configure        read env vars, load payer, build confirmed RPC client
1. derive counter   find counter PDA, fetch/validate it, print count
2. setup tokens     one legacy tx: create mint/source/vault + mint 1 token
3. deposit          one v1 tx: source -> vault, then check balances
4. withdraw         one v1 tx: vault -> source + close vault, then check balances
```

### Step 0 — Configuration

- The cluster prompt picks the RPC endpoint (`src/cluster.rs`); `KEYPAIR` and `PROGRAM_ID` are
  read with the defaults above.
- The payer keypair is the fee payer and the counter authority. It is the only required signer
  for deposit and withdraw.
- The RPC client uses `CommitmentConfig::confirmed()`, and `getVersion` checks the endpoint
  answers before anything else is sent.

### Step 1 — Derive and validate the counter

1. `Pubkey::find_program_address(&[b"counter", payer], program_id)`.
2. `fetch_count` fetches the account and checks:
   - account exists (otherwise: "run counter-client first"),
   - `account.owner == program_id`,
   - `account.data.len() == 48`,
   - `account.data[0] == 1` (counter discriminator),
   - reads `count` from bytes `40..48` (u64 LE).
3. Prints `count` for context. The counter is not modified by this client.

### Step 2 — Create the token accounts (legacy transaction)

`setup_token_accounts` creates three fresh keypairs and sends **seven instructions in one legacy
transaction**, signed by the payer plus the three new keypairs:

| # | Instruction | Program | Accounts | Data |
|---|---|---|---|---|
| 1 | `CreateAccount` | System | `[signer, writable] payer`, `[signer, writable] mint` | tag `0u32`, lamports (rent for 82 B), space `82`, owner = Token program |
| 2 | `InitializeMint2` | Token | `[writable] mint` | `[20, decimals=6, mint_authority=payer, freeze=None]` |
| 3 | `CreateAccount` | System | `[signer, writable] payer`, `[signer, writable] source` | tag `0u32`, rent for 165 B, space `165`, owner = Token program |
| 4 | `InitializeAccount3` | Token | `[writable] source`, `[] mint` | `[18, owner=payer]` |
| 5 | `CreateAccount` | System | `[signer, writable] payer`, `[signer, writable] vault` | tag `0u32`, rent for 165 B, space `165`, owner = Token program |
| 6 | `InitializeAccount3` | Token | `[writable] vault`, `[] mint` | `[18, owner=counter PDA]` |
| 7 | `MintTo` | Token | `[writable] mint`, `[writable] source`, `[signer] mint_authority=payer` | `[7, amount=DEPOSIT_AMOUNT]` |

Details:

- `DEPOSIT_AMOUNT = 1_000_000` = 1 token with `DECIMALS = 6`.
- Rent is fetched from the cluster with `get_minimum_balance_for_rent_exemption`.
- Instruction 6 is the important one: it sets the vault's **token owner** to the counter PDA.
  Nothing else distinguishes the vault from the source account.
- `CreateAccount` requires the new account to sign, hence the three `Keypair::new()` accounts.
- This is a **legacy** transaction (`Message::new_with_blockhash`); the v1 header limits are not
  needed for setup.

After setup the client asserts `source.balance == DEPOSIT_AMOUNT` before going further.

### Step 3 — Deposit (v1 transaction)

Client-side (`deposit_ix`):

```text
data:     [2][amount: u64 little-endian]
accounts: 0 [signer]            depositor  = payer
          1 [writable]          source     = depositor's token account
          2 []                  counter    = counter PDA
          3 [writable]          vault      = token account owned by counter PDA
          4 []                  token_program
```

On-chain (`program/src/instructions/deposit.rs`), in order:

1. Decode `amount` from the 8 data bytes. Wrong length → `InvalidInstructionData`.
2. `amount == 0` → `0x3 ZeroAmount`.
3. Need at least 5 accounts.
4. `depositor.is_signer()` → otherwise `MissingRequiredSignature`.
5. Account 4 is the SPL Token program → otherwise `IncorrectProgramId`.
6. `counter.owned_by(program_id)` → otherwise `InvalidAccountOwner`.
7. Counter layout: 48 bytes, discriminator `1` → otherwise `InvalidAccountData`.
8. Validate the vault with `vault_balance(vault, counter)`:
   - owned by the SPL Token program and 165 bytes,
   - token owner == counter PDA,
   - otherwise `0x2 InvalidVault`.
9. CPI `Transfer(source -> vault, authority = depositor, amount)` through the SPL Token program.
   The Token program enforces: same mint on both accounts, source authority == depositor,
   sufficient source balance.
10. Log `deposited {amount}`.

What changes: **only the token balances**. Deposit is permissionless — any signer with a source
token account of the right mint can deposit. The counter account is not written.

The client then asserts:

- the transaction landed as `VersionedMessage::V1` with exactly its `CONFIG`,
- `vault.balance == DEPOSIT_AMOUNT`,
- `source.balance == 0` (the whole source balance moved).

### Step 4 — Withdraw (v1 transaction)

Client-side (`withdraw_ix`):

```text
data:     [3]                       (no arguments; withdraw takes the whole balance)
accounts: 0 [signer, writable] authority = payer
          1 []                counter   = counter PDA
          2 [writable]        vault
          3 [writable]        destination = source token account
          4 []                token_program
```

On-chain (`program/src/instructions/withdraw.rs`), in order:

1. Data must be empty after the discriminator → otherwise `InvalidInstructionData`.
2. Need at least 5 accounts.
3. `authority.is_signer()` → otherwise `MissingRequiredSignature`.
4. Account 4 is the SPL Token program → otherwise `IncorrectProgramId`.
5. `counter.owned_by(program_id)` → otherwise `InvalidAccountOwner`.
6. Counter layout check → otherwise `InvalidAccountData`.
7. `counter.authority == signer` → otherwise `0x0 Unauthorized`.
8. Vault check through `vault_balance`: token account owned by the counter PDA, otherwise
   `0x2 InvalidVault`. Reads the whole vault balance.
9. Signs as the counter PDA (seeds `["counter", authority, bump]`, canonical bump stored at
   initialize) and issues one p-token `Batch` CPI containing:
   - `Transfer(vault -> destination, amount)`, skipped when the balance is 0,
   - `CloseAccount(vault, rent_destination = authority)`.

   The batch instruction (discriminator `255`, SIMD-0266) is a p-token feature: on devnet and
   mainnet p-token runs at the SPL Token address, so the client still passes the normal
   `Tokenkeg...` program ID. The legacy SPL Token program would reject the batch.
10. Log `withdrew {amount} and closed the vault`.

Closing the vault returns its rent to the authority and removes the vault account. Because the
vault is closed, re-running the client creates a new vault next time.

The client then asserts:

- the transaction landed as v1 with `CONFIG`,
- `source.balance == DEPOSIT_AMOUNT` (everything came back),
- the vault account no longer exists.

### Balance timeline

| Phase | source | vault | vault exists |
|---|---|---|---|
| after setup | `DEPOSIT_AMOUNT` | `0` | yes |
| after deposit | `0` | `DEPOSIT_AMOUNT` | yes |
| after withdraw | `DEPOSIT_AMOUNT` | — | no (closed) |

---

## 4. Deposit does not create anything

A common misconception: `Deposit` does **not** create an ATA, a PDA, or a token account. It is a
single SPL Token `Transfer` CPI into an account that must already exist. The creation work happens
in step 2, before deposit, in the client:

```text
System CreateAccount      creates the raw 165-byte account (owned by Token program)
Token InitializeAccount3  writes mint + token owner = counter PDA into it
```

If the vault is missing, is not owned by the Token program, is not 165 bytes, or has a different
token owner, deposit fails (`InvalidVault` / account errors). If the vault exists but was created
for a different mint, the Token program's `Transfer` fails. There is no registry of vaults: the
program trusts whichever valid vault account is passed in, so multiple vaults per counter are
possible; `Withdraw` drains the one it is given and closes it.

---

## 5. v1 transactions (SIMD-0385)

Deposit and withdraw use versioned messages with version `1`. In v1 the fee/resource knobs live in
the message header as a `TransactionConfig`, and **unset limits are zero**, so both are set
explicitly:

```rust
const CONFIG: v1::TransactionConfig = v1::TransactionConfig::empty()
    .with_compute_unit_limit(50_000)
    .with_loaded_accounts_data_size_limit(256 * 1024)
    .with_priority_fee(1_000);
```

- `50_000` CU: enough for the token CPIs.
- `256 KiB` of loaded account data: deposit/withdraw load the SPL Token program on top of the
  counter and token accounts. The unit tests use the same limit for token transactions.
- `1_000` lamports: a **total** priority fee, not a per-CU price.

`send_v1` compiles, signs, sends (preflight simulation on) and confirms the transaction.
`verify_v1_on_chain` then fetches it with `max_supported_transaction_version: Some(1)` — required
to decode v1 transactions — and asserts the on-chain message is `VersionedMessage::V1` and its
config equals `CONFIG` exactly. That is how the client proves the v1 transaction format is
actually being used, not just v0/legacy.

---

## 6. Errors you can see

Custom errors are `ProgramError::Custom(code)` from `program/src/error.rs`, shown by RPC as
`custom program error: 0x<code>`:

| Code | Name | Cause on the vault path |
|---|---|---|
| `0x0` | `Unauthorized` | `Withdraw` signer is not the counter authority |
| `0x1` | `Overflow` | `count` would exceed `u64::MAX` (increment, not used here) |
| `0x2` | `InvalidVault` | Vault is not a 165-byte SPL Token account owned by the counter PDA |
| `0x3` | `ZeroAmount` | `Deposit` amount is 0 |

Framework errors from `pinocchio::error::ProgramError` that show up on this path:
`InvalidInstructionData`, `NotEnoughAccountKeys`, `MissingRequiredSignature`,
`IncorrectProgramId`, `InvalidAccountOwner`, `InvalidAccountData`.

Client-side errors (anyhow) include: unreadable keypair, bad `PROGRAM_ID`, counter not initialized,
token setup failure, and any failed `ensure!` assertion.

---

## 7. Cost notes

Each run creates a fresh mint and source token account:

- mint rent (~0.0015 SOL) stays locked in the mint,
- source account rent (~0.002 SOL) stays locked,
- vault rent comes back when `Withdraw` closes it.

On devnet this is spent devnet SOL. If you want to reuse accounts instead, the client would need
to derive a stable mint (or accept a mint address via env) and derive the source/vault ATAs.

---

## 8. Code map

| Function | Purpose |
|---|---|
| `main` | Config, counter check, setup, deposit, withdraw, assertions |
| `deposit_ix` | Build the program's `Deposit` instruction (discriminator `2`) |
| `withdraw_ix` | Build the program's `Withdraw` instruction (discriminator `3`) |
| `send_v1` | Compile/sign/send a v1 transaction |
| `send_legacy` | Compile/sign/send the SPL Token setup transaction |
| `verify_v1_on_chain` | Fetch the tx and compare its v1 config with `CONFIG` |
| `fetch_count` | Read + validate the counter account |
| `setup_token_accounts` | Create mint/source/vault and mint tokens to source |
| `create_account_ix` | System `CreateAccount` (tag `0`) |
| `initialize_mint2_ix` | Token `InitializeMint2` (tag `20`) |
| `initialize_account3_ix` | Token `InitializeAccount3` (tag `18`) |
| `mint_to_ix` | Token `MintTo` (tag `7`) |
| `get_token_balance` | SPL Token balance through RPC |
| `account_exists` | Check whether the vault was closed |
