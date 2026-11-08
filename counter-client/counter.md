# counter-client

`counter-client` is a Rust binary that exercises the **counter side** of the program: it derives
the authority's counter PDA, initializes it if needed, increments it, and proves that a different
signer cannot increment it.

It does **not** touch the vault. `Deposit` and `Withdraw` belong to
[`vault-client`](../vault-client/vault.md), which expects the counter created here to already
exist.

- Source: [`src/main.rs`](src/main.rs)
- Program logic: [`../program/src/instructions/initialize.rs`](../program/src/instructions/initialize.rs),
  [`../program/src/instructions/increment.rs`](../program/src/instructions/increment.rs),
  [`../program/src/state/counter.rs`](../program/src/state/counter.rs)

---

## 1. Run

```sh
cd counter-client
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
| `PROGRAM_ID` | pubkey in `../target/deploy/auction-keypair.json` | Program to call |

The program is deployed on devnet at `LmGSyCLnSD14kNmpgvsHzM2fN6dxQ8eUyoVhPtw4Ff2`, which is also
the pubkey in the local keypair file, so the default `PROGRAM_ID` works on both clusters.

---

## 2. The counter PDA

```text
counter = find_program_address(["counter", authority], program_id)
```

- Seeds are the ASCII string `"counter"` and the authority's 32-byte pubkey.
- The address is deterministic, so one authority has exactly one counter.
- The **canonical bump** is found by the program at `Initialize` and stored in the account. The
  bump is never accepted from the client.
- At that address only the program itself can create the account (via `invoke_signed`), so a
  look-alike account cannot be substituted.

The account is 48 bytes, alignment 1 (`program/src/state/counter.rs`):

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | discriminator (`1`) |
| 1 | 1 | bump |
| 2 | 6 | padding |
| 8 | 32 | authority |
| 40 | 8 | count (`u64` little-endian) |

The client always checks the account owner, the 48-byte length, and the discriminator before
reading `count` from bytes `40..48`.

---

## 3. End-to-end flow

`main` runs these phases in order:

```text
0. configure        read env vars, load payer, build confirmed RPC client
1. derive counter   find counter PDA, print cluster/program/payer/counter
2. initialize       only if the counter does not exist yet
3. increment        authority increments once and the client checks count + 1
4. unauthorized     another signer tries to increment and must be rejected
5. final check      count is unchanged by the rejected call
```

### Step 0 — Configuration

- The cluster prompt picks the RPC endpoint (`src/cluster.rs`); `KEYPAIR` and `PROGRAM_ID` are
  read with the defaults above.
- The payer keypair is the fee payer and the counter authority.
- The RPC client uses `CommitmentConfig::confirmed()`, and `getVersion` checks the endpoint
  answers before anything else is sent.

### Step 1 — Derive the counter

`main` derives the counter PDA with `["counter", payer]`, then prints the cluster, program ID,
payer, and counter address.

### Step 2 — Initialize (conditional)

`fetch_count` reads the account first:

- **Exists:** initialization is skipped and the current count is printed. This makes the client
  safe to run repeatedly.
- **Missing:** send `Initialize` and then assert the result.

Instruction sent by the client:

```text
data:     [0]
accounts: 0 [signer, writable] authority = payer
          1 [writable]        counter   = PDA ["counter", authority]
          2 []                system_program
```

On-chain (`program/src/instructions/initialize.rs`):

1. Data must be empty after the discriminator `0` → otherwise `InvalidInstructionData`.
2. Need at least 3 accounts → otherwise `NotEnoughAccountKeys`.
3. `authority.is_signer()` → otherwise `MissingRequiredSignature`.
4. Account 2 is the System program → otherwise `IncorrectProgramId`.
5. `counter.owned_by(program_id)` → if true, `AccountAlreadyInitialized`. This is what makes a
   second `Initialize` fail.
6. The program derives the canonical PDA itself and compares it with the passed counter address
   → mismatch gives `InvalidSeeds`.
7. `CreateAccount::with_minimum_balance(authority, counter, 48, program_id)` as a signed CPI
   using seeds `["counter", authority, bump]`: the Program account creates the 48-byte,
   rent-exempt account owned by this program.
8. Write the state: discriminator `1`, stored `bump`, `authority`, `count = 0`.
9. Log `counter initialized`.

The client then verifies the transaction on-chain as v1, re-fetches the counter, and asserts
`count == 0`.

### Step 3 — Increment

Instruction sent by the client (`increment_ix`):

```text
data:     [1]
accounts: 0 [signer]   authority = payer
          1 [writable] counter   = counter PDA
```

On-chain (`program/src/instructions/increment.rs`):

1. Data must be empty after the discriminator `1` → otherwise `InvalidInstructionData`.
2. Need at least 2 accounts → otherwise `NotEnoughAccountKeys`.
3. `authority.is_signer()` → otherwise `MissingRequiredSignature`.
4. `counter.owned_by(program_id)` → otherwise `InvalidAccountOwner`.
5. Counter layout: 48 bytes, discriminator `1` → otherwise `InvalidAccountData`.
6. `counter.authority == signer` → otherwise `0x0 Unauthorized`.
7. `count.checked_add(1)` → if already `u64::MAX`, `0x1 Overflow` instead of wrapping to 0.
8. Store the new count and log `count = {count}`.

The client then verifies the transaction as v1, re-fetches the counter, and asserts
`count == before + 1`.

### Step 4 — Unauthorized signer

The client generates a throwaway `Keypair::new()` (the "intruder"), builds the same `Increment`
instruction with the intruder as authority, and sends a transaction signed by both the payer (fee
payer) and the intruder.

The program reaches step 6 above and returns `0x0 Unauthorized`. The client inspects the error
string and requires `custom program error: 0x0`; anything else fails the check. Because the
transaction fails preflight simulation, it is never submitted and no fee is charged.

### Step 5 — Final check

The client re-fetches the counter and asserts the count is still the value from step 3, proving the
rejected increment did not change state.

---

## 4. v1 transactions (SIMD-0385)

Both real calls use versioned messages with version `1`. The header carries a
`TransactionConfig`; because unset limits are zero, the client sets them explicitly:

```rust
const CONFIG: v1::TransactionConfig = v1::TransactionConfig::empty()
    .with_compute_unit_limit(20_000)
    .with_loaded_accounts_data_size_limit(64 * 1024)
    .with_priority_fee(1_000);
```

- `20_000` CU covers PDA search and the `CreateAccount` CPI used by `Initialize`.
- `64 KiB` loaded account data covers the accounts plus the program's `ProgramData` account. It is
  enough here because no SPL Token program is loaded; token calls need 256 KiB (`vault-client`).
- `1_000` lamports is a **total** priority fee, not a per-CU price.

`send_v1` compiles the message, signs, sends with preflight simulation, and confirms.
`verify_v1_on_chain` fetches the transaction with `max_supported_transaction_version: Some(1)` and
asserts the on-chain message is `VersionedMessage::V1` with exactly `CONFIG`. This proves the v1
format was actually used, not just accepted by the client.

---

## 5. Errors you can see

Custom errors are `ProgramError::Custom(code)` from `program/src/error.rs`, shown by RPC as
`custom program error: 0x<code>`:

| Code | Name | Cause on the counter path |
|---|---|---|
| `0x0` | `Unauthorized` | `Increment` signer is not `Counter::authority` |
| `0x1` | `Overflow` | `count` is already `u64::MAX` |
| `0x2` | `InvalidVault` | Vault error, not reachable from this client |
| `0x3` | `ZeroAmount` | Deposit error, not reachable from this client |

Framework errors from `pinocchio::error::ProgramError` seen on this path:
`InvalidInstructionData`, `NotEnoughAccountKeys`, `MissingRequiredSignature`,
`IncorrectProgramId`, `AccountAlreadyInitialized`, `InvalidSeeds`, `InvalidAccountOwner`,
`InvalidAccountData`.

Client-side errors (anyhow) include: unreadable keypair, bad `PROGRAM_ID`, "counter missing after
initialize", and any failed `ensure!` assertion.

---

## 6. Relationship to `vault-client`

| | `counter-client` | `vault-client` |
|---|---|---|
| Instructions | `Initialize` (`0`), `Increment` (`1`) | `Deposit` (`2`), `Withdraw` (`3`) |
| Accounts | counter PDA | vault token account owned by the counter PDA |
| Creates | counter PDA | mint + source + vault token accounts |
| Creates counter? | yes, if missing | no, errors if missing |
| Token program | not used | required |

Run `counter-client` first (at least once per payer), then `vault-client` with the same keypair.

---

## 7. Code map

| Function | Purpose |
|---|---|
| `main` | Config, initialize-if-needed, increment, unauthorized check, assertions |
| `increment_ix` | Build the program's `Increment` instruction (discriminator `1`; `Initialize` is built inline in `main`) |
| `send_v1` | Compile/sign/send a v1 transaction |
| `verify_v1_on_chain` | Fetch the tx and compare its v1 config with `CONFIG` |
| `fetch_count` | Read + validate the counter account, return `count` |

---

## 8. Deploying the program

The clients do not deploy anything; they call an already-deployed program. From the project root:

```sh
cargo build-sbf --tools-version v1.58 --arch v3
solana program deploy -u devnet target/deploy/auction.so \
  --program-id target/deploy/auction-keypair.json
```

`target/deploy/auction-keypair.json` holds the program ID and the upgrade authority key. Back it
up: `cargo clean` deletes it.
