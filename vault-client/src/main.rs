//! # vault-client
//!
//! Exercises only the vault side of the program on devnet: `Deposit` and `Withdraw`.
//! `Initialize`/`Increment` belong to `counter-client`; this client does not create a counter,
//! it only expects the payer's counter PDA to exist already.
//!
//! Deposit and withdraw are sent as **v1 transactions** (SIMD-0385), so the compute-unit limit,
//! loaded-accounts-data limit, and priority fee live in the message header. The SPL Token setup
//! transactions use legacy messages, where those knobs are not interesting.
//!
//! ## Run
//!
//! ```text
//! KEYPAIR      default ~/.config/solana/id.json (fee payer + counter authority)
//! PROGRAM_ID   default: LmGSyCLnSD14kNmpgvsHzM2fN6dxQ8eUyoVhPtw4Ff2 (deployed on devnet)
//! ```
//!
//! On start the client asks for the cluster (`cluster.rs`): localnet (surfpool at
//! 127.0.0.1:8899, must be running with the program deployed) or devnet (`DEVNET_RPC_URL` from
//! `../.env`).
//!
//! ## What happens, step by step
//!
//! ### 0. Configuration (`main`)
//!
//! Ask for the cluster, read `KEYPAIR` and `PROGRAM_ID` (both optional; the devnet program ID is
//! the default), open a `confirmed` RPC client, check the endpoint answers, and load the payer
//! keypair. The payer is both the fee payer and the counter authority, so its keypair signs
//! deposit and withdraw.
//!
//! ### 1. Derive and check the counter PDA (`main` + `fetch_count`)
//!
//! The counter is `find_program_address(["counter", payer], program_id)`. The same seeds are used
//! by `counter-client` and by the program itself. Before doing anything token-related we fetch the
//! account, check it is owned by the program, check the 48-byte layout and discriminator `1`, and
//! read `count` from bytes `40..48`. If it is missing we bail with "run counter-client first":
//! `Deposit` needs the counter account, but this client must not initialize it.
//!
//! ### 2. Create the token accounts (`setup_token_accounts`, one legacy transaction)
//!
//! Everything the vault flow needs is created fresh on every run, so the test is self-contained:
//!
//! 1. `mint`: a new keypair. `CreateAccount` funds it with rent-exempt lamports for 82 bytes,
//!    owned by the SPL Token program, then `InitializeMint2` makes it a mint with `DECIMALS` (6)
//!    decimals, the payer as mint authority, and no freeze authority.
//! 2. `source`: a new token account (165 bytes) owned by the payer. This is the depositor's
//!    account and the withdraw destination.
//! 3. `vault`: a new token account (165 bytes) whose token **owner is the counter PDA**. That is
//!    the only property the program checks: any SPL Token account owned by the counter works, and
//!    creating it with the counter PDA as owner means only the program can move tokens out.
//! 4. `MintTo`: mint exactly `DEPOSIT_AMOUNT` (1 token with 6 decimals) to `source`.
//!
//! `CreateAccount` needs the new account to sign, which is why the mint and both token accounts
//! are generated locally as keypairs. The seven instructions above go in one legacy transaction
//! signed by the payer plus the three new keypairs.
//!
//! ### 3. Deposit (`deposit_ix` + `send_v1`, v1 transaction)
//!
//! Instruction data is `[2, amount as u64 LE]`. Accounts, in program order:
//! `[signer] depositor` (payer), `[writable] source`, `[] counter`, `[writable] vault`,
//! `[] token program`. The program checks the counter and the vault, then does a `Transfer` CPI
//! signed by the depositor, so tokens move `source -> vault`.
//!
//! The transaction is compiled with `CONFIG` (50k CU, 256 KiB loaded accounts, 1000 lamports
//! total priority fee), preflighted, sent, and confirmed. Afterwards the client:
//! - reads the transaction back with `verify_v1_on_chain` and asserts it landed as
//!   `VersionedMessage::V1` with exactly this `CONFIG`;
//! - checks the vault balance equals `DEPOSIT_AMOUNT`;
//! - checks the source balance is now 0, proving the tokens actually moved.
//!
//! ### 4. Withdraw (`withdraw_ix` + `send_v1`, v1 transaction)
//!
//! Instruction data is just `[3]` (no arguments: withdraw takes the whole vault balance). Accounts:
//! `[signer, writable] authority` (payer), `[] counter`, `[writable] vault`,
//! `[writable] destination` (the source account), `[] token program`. The program signs as the
//! counter PDA with `["counter", authority, bump]` and runs a p-token batch of `Transfer` (vault ->
//! destination) plus `CloseAccount`, so the vault is drained and closed and its rent goes back to
//! the authority.
//!
//! The client verifies the transaction is v1, checks the destination balance is back to
//! `DEPOSIT_AMOUNT`, and checks the vault account no longer exists (closed).
//!
//! ## Helpers
//!
//! - `send_v1` / `send_legacy`: build, sign, send and confirm the two transaction flavours.
//! - `verify_v1_on_chain`: fetch the confirmed transaction and compare its header `CONFIG`.
//! - `fetch_count`: read and validate the counter account.
//! - `setup_token_accounts`: build the mint/source/vault setup listed in step 2.
//! - `create_account_ix`, `initialize_mint2_ix`, `initialize_account3_ix`, `mint_to_ix`: hand-built
//!   System/SPL Token instructions (no extra crates), each documenting its data layout.
//! - `get_token_balance`: SPL Token balance via RPC; `account_exists`: closed-account check.
//!
//! Note: each run pays rent for a new mint and source account (~0.004 SOL on devnet). The vault's
//! rent is recovered by `Withdraw`; the source account is left open.

mod cluster;

use anyhow::{Context, Result, bail, ensure};
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::{Keypair, read_keypair_file};
use solana_message::{Message, VersionedMessage, v1};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::{RpcTransactionConfig, UiTransactionEncoding};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

const SYSTEM_PROGRAM: Pubkey = Pubkey::new_from_array([0; 32]);
const TOKEN_PROGRAM: Pubkey = Pubkey::new_from_array([
    6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172, 28, 180, 133, 237,
    95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169,
]); // TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA
/// Program ID deployed on devnet (also the pubkey in ../target/deploy/auction-keypair.json).
const DEVNET_PROGRAM_ID: &str = "LmGSyCLnSD14kNmpgvsHzM2fN6dxQ8eUyoVhPtw4Ff2";

/// SPL Token account/mint sizes, used for rent and `CreateAccount`.
const TOKEN_ACCOUNT_LEN: usize = 165;
const MINT_LEN: usize = 82;

const DECIMALS: u8 = 6;
/// Amount minted to the depositor and moved into the vault. The whole vault is withdrawn later.
const DEPOSIT_AMOUNT: u64 = 1_000_000; // 1 token with 6 decimals

/// v1 resource limits live in the message header. Unset limits are ZERO, so set both:
/// - CU limit: token deposit/withdraw CPIs to the SPL Token program need more than initialize.
/// - Loaded accounts data: deposit/withdraw also load the SPL Token program, so use the 256 KiB
///   limit that `tests/` uses for token transactions. Too small fails with
///   `MaxLoadedAccountsDataSizeExceeded`.
/// - Priority fee: TOTAL lamports for the transaction, not micro-lamports per CU.
const CONFIG: v1::TransactionConfig = v1::TransactionConfig::empty()
    .with_compute_unit_limit(50_000)
    .with_loaded_accounts_data_size_limit(256 * 1024)
    .with_priority_fee(1_000);

fn main() -> Result<()> {
    let cluster = cluster::select()?;
    let keypair_path = std::env::var("KEYPAIR").unwrap_or_else(|_| {
        format!(
            "{}/.config/solana/id.json",
            std::env::var("HOME").unwrap_or_default()
        )
    });
    let payer = read_keypair_file(&keypair_path)
        .map_err(|e| anyhow::anyhow!("failed to read keypair {keypair_path}: {e}"))?;
    let program_id = match std::env::var("PROGRAM_ID") {
        Ok(id) => id.parse().context("PROGRAM_ID is not a valid pubkey")?,
        Err(_) => DEVNET_PROGRAM_ID
            .parse()
            .context("DEVNET_PROGRAM_ID is not a valid pubkey")?,
    };
    let rpc = RpcClient::new_with_commitment(cluster.rpc_url.clone(), CommitmentConfig::confirmed());
    cluster.check_reachable(&rpc)?;
    let (counter, _) =
        Pubkey::find_program_address(&[b"counter", payer.pubkey().as_ref()], &program_id);

    println!("cluster: {}", cluster.label());
    println!("program: {program_id}");
    println!("payer:   {}", payer.pubkey());
    println!("counter: {counter}\n");

    // The counter is created by counter-client; Deposit needs it to exist.
    let count = fetch_count(&rpc, &counter, &program_id)?.with_context(|| {
        format!("counter {counter} is not initialized; run counter-client first")
    })?;
    println!("[counter]    count = {count} (initialized by counter-client)\n");

    // 1. A fresh mint plus two token accounts: the depositor's source and the counter PDA's vault.
    let (mint, source, vault) = setup_token_accounts(&rpc, &payer, counter)?;
    println!("[setup]      mint:   {mint}");
    println!("[setup]      source: {source} (owner: {})", payer.pubkey());
    println!("[setup]      vault:  {vault} (owner: counter PDA)");
    let funded = get_token_balance(&rpc, &source)?;
    ensure!(
        funded == DEPOSIT_AMOUNT,
        "expected source balance {DEPOSIT_AMOUNT}, got {funded}"
    );

    // 2. deposit the whole source balance into the counter's vault.
    let sig = send_v1(
        &rpc,
        deposit_ix(
            program_id,
            payer.pubkey(),
            source,
            counter,
            vault,
            DEPOSIT_AMOUNT,
        ),
        &payer,
        &[&payer],
    )
    .context("deposit failed")?;
    println!("[deposit]    tx: {sig}");
    verify_v1_on_chain(&rpc, &sig)?;
    let vault_balance = get_token_balance(&rpc, &vault)?;
    let source_balance = get_token_balance(&rpc, &source)?;
    ensure!(
        vault_balance == DEPOSIT_AMOUNT,
        "expected vault balance {DEPOSIT_AMOUNT}, got {vault_balance}"
    );
    ensure!(
        source_balance == 0,
        "expected source balance 0 after deposit, got {source_balance}"
    );
    println!("[deposit]    ok: vault balance = {vault_balance}");

    // 3. withdraw everything: the vault is drained and closed, rent returns to the authority.
    let sig = send_v1(
        &rpc,
        withdraw_ix(program_id, payer.pubkey(), counter, vault, source),
        &payer,
        &[&payer],
    )
    .context("withdraw failed")?;
    println!("[withdraw]   tx: {sig}");
    verify_v1_on_chain(&rpc, &sig)?;
    let returned = get_token_balance(&rpc, &source)?;
    ensure!(
        returned == DEPOSIT_AMOUNT,
        "expected source balance {DEPOSIT_AMOUNT} after withdraw, got {returned}"
    );
    ensure!(
        !account_exists(&rpc, &vault)?,
        "vault account {vault} was not closed"
    );
    println!("[withdraw]   ok: source balance = {returned}, vault closed");

    println!("\nall checks passed");
    Ok(())
}

fn deposit_ix(
    program_id: Pubkey,
    depositor: Pubkey,
    source: Pubkey,
    counter: Pubkey,
    vault: Pubkey,
    amount: u64,
) -> Instruction {
    let mut data = vec![2]; // discriminator for Deposit
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction::new_with_bytes(
        program_id,
        &data,
        vec![
            AccountMeta::new_readonly(depositor, true),
            AccountMeta::new(source, false),
            AccountMeta::new_readonly(counter, false),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        ],
    )
}

fn withdraw_ix(
    program_id: Pubkey,
    authority: Pubkey,
    counter: Pubkey,
    vault: Pubkey,
    destination: Pubkey,
) -> Instruction {
    Instruction::new_with_bytes(
        program_id,
        &[3], // discriminator for Withdraw
        vec![
            AccountMeta::new(authority, true),
            AccountMeta::new_readonly(counter, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        ],
    )
}

/// Compile a v1 message, sign, send (with preflight simulation) and confirm.
fn send_v1(
    rpc: &RpcClient,
    ix: Instruction,
    payer: &Keypair,
    signers: &[&Keypair],
) -> Result<String> {
    let blockhash = rpc.get_latest_blockhash()?;
    let message = v1::Message::try_compile_with_config(&payer.pubkey(), &[ix], blockhash, CONFIG)?;
    let tx = VersionedTransaction::try_new(VersionedMessage::V1(message), signers)?;
    Ok(rpc.send_and_confirm_transaction(&tx)?.to_string())
}

/// Compile a legacy message, sign, send and confirm. Used for SPL Token setup, where the v1
/// fee/limit knobs are not interesting.
fn send_legacy(
    rpc: &RpcClient,
    ixs: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
) -> Result<String> {
    let blockhash = rpc.get_latest_blockhash()?;
    let message = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &blockhash);
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(message), signers)?;
    Ok(rpc.send_and_confirm_transaction(&tx)?.to_string())
}

/// Read the transaction back and check it landed as v1 with our config.
fn verify_v1_on_chain(rpc: &RpcClient, sig: &str) -> Result<()> {
    let tx = rpc.get_transaction_with_config(
        &sig.parse()?,
        RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::Base64),
            commitment: Some(CommitmentConfig::confirmed()),
            max_supported_transaction_version: Some(1), // required to read v1 transactions
            min_context_slot: None,                     // field added in rpc-client-api 4.4
        },
    )?;
    let decoded = tx
        .transaction
        .transaction
        .decode()
        .context("cannot decode transaction")?;
    match decoded.message {
        VersionedMessage::V1(m) if m.config == CONFIG => Ok(()),
        VersionedMessage::V1(m) => bail!("v1 config on-chain differs: {:?}", m.config),
        other => bail!("expected a v1 transaction, got {other:?}"),
    }
}

/// Count at offset 40..48 of the 48-byte counter account, after checking owner and layout.
fn fetch_count(rpc: &RpcClient, counter: &Pubkey, program_id: &Pubkey) -> Result<Option<u64>> {
    let Some(account) = rpc
        .get_account_with_commitment(counter, CommitmentConfig::confirmed())?
        .value
    else {
        return Ok(None);
    };
    ensure!(
        account.owner == *program_id,
        "counter is owned by {}, not the program",
        account.owner
    );
    ensure!(
        account.data.len() == 48 && account.data[0] == 1,
        "not a counter account"
    );
    Ok(Some(u64::from_le_bytes(account.data[40..48].try_into()?)))
}

/// Create a mint, the depositor's source token account, and the vault token account owned by the
/// counter PDA, then mint [`DEPOSIT_AMOUNT`] to the source.
///
/// A fresh mint per run keeps the deposit/withdraw check self-contained. The vault is a plain
/// SPL Token account (any account owned by the counter PDA works); it is closed by `Withdraw`.
fn setup_token_accounts(
    rpc: &RpcClient,
    payer: &Keypair,
    counter: Pubkey,
) -> Result<(Pubkey, Pubkey, Pubkey)> {
    let mint = Keypair::new();
    let source = Keypair::new();
    let vault = Keypair::new();
    let mint_rent = rpc.get_minimum_balance_for_rent_exemption(MINT_LEN)?;
    let account_rent = rpc.get_minimum_balance_for_rent_exemption(TOKEN_ACCOUNT_LEN)?;

    let ixs = [
        create_account_ix(
            payer.pubkey(),
            mint.pubkey(),
            mint_rent,
            MINT_LEN,
            TOKEN_PROGRAM,
        ),
        initialize_mint2_ix(mint.pubkey(), DECIMALS, payer.pubkey()),
        create_account_ix(
            payer.pubkey(),
            source.pubkey(),
            account_rent,
            TOKEN_ACCOUNT_LEN,
            TOKEN_PROGRAM,
        ),
        initialize_account3_ix(source.pubkey(), mint.pubkey(), payer.pubkey()),
        create_account_ix(
            payer.pubkey(),
            vault.pubkey(),
            account_rent,
            TOKEN_ACCOUNT_LEN,
            TOKEN_PROGRAM,
        ),
        initialize_account3_ix(vault.pubkey(), mint.pubkey(), counter),
        mint_to_ix(
            mint.pubkey(),
            source.pubkey(),
            payer.pubkey(),
            DEPOSIT_AMOUNT,
        ),
    ];
    send_legacy(rpc, &ixs, payer, &[payer, &mint, &source, &vault])
        .context("token setup failed")?;

    Ok((mint.pubkey(), source.pubkey(), vault.pubkey()))
}

/// System `CreateAccount`: `[from, to]`, data `[0u32, lamports, space, owner]`.
fn create_account_ix(
    from: Pubkey,
    to: Pubkey,
    lamports: u64,
    space: usize,
    owner: Pubkey,
) -> Instruction {
    let mut data = Vec::with_capacity(52);
    data.extend_from_slice(&0u32.to_le_bytes()); // SystemInstruction::CreateAccount
    data.extend_from_slice(&lamports.to_le_bytes());
    data.extend_from_slice(&(space as u64).to_le_bytes());
    data.extend_from_slice(owner.as_ref());
    Instruction::new_with_bytes(
        SYSTEM_PROGRAM,
        &data,
        vec![AccountMeta::new(from, true), AccountMeta::new(to, true)],
    )
}

/// Token `InitializeMint2` (tag 20): `[mint]`, data `[20, decimals, mint_authority, no freeze]`.
fn initialize_mint2_ix(mint: Pubkey, decimals: u8, mint_authority: Pubkey) -> Instruction {
    let mut data = Vec::with_capacity(35);
    data.push(20);
    data.push(decimals);
    data.extend_from_slice(mint_authority.as_ref());
    data.push(0); // COption<Pubkey>::None for the freeze authority
    Instruction::new_with_bytes(TOKEN_PROGRAM, &data, vec![AccountMeta::new(mint, false)])
}

/// Token `InitializeAccount3` (tag 18): `[account, mint]`, data `[18, owner]`.
fn initialize_account3_ix(account: Pubkey, mint: Pubkey, owner: Pubkey) -> Instruction {
    let mut data = Vec::with_capacity(33);
    data.push(18);
    data.extend_from_slice(owner.as_ref());
    Instruction::new_with_bytes(
        TOKEN_PROGRAM,
        &data,
        vec![
            AccountMeta::new(account, false),
            AccountMeta::new_readonly(mint, false),
        ],
    )
}

/// Token `MintTo` (tag 7): `[mint, destination, mint_authority]`, data `[7, amount]`.
fn mint_to_ix(mint: Pubkey, destination: Pubkey, authority: Pubkey, amount: u64) -> Instruction {
    let mut data = Vec::with_capacity(9);
    data.push(7);
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction::new_with_bytes(
        TOKEN_PROGRAM,
        &data,
        vec![
            AccountMeta::new(mint, false),
            AccountMeta::new(destination, false),
            AccountMeta::new_readonly(authority, true),
        ],
    )
}

/// Token balance of an SPL Token account.
fn get_token_balance(rpc: &RpcClient, token_account: &Pubkey) -> Result<u64> {
    let balance = rpc
        .get_token_account_balance_with_commitment(token_account, CommitmentConfig::confirmed())?
        .value;
    balance
        .amount
        .parse()
        .with_context(|| format!("invalid token amount {:?}", balance.amount))
}

fn account_exists(rpc: &RpcClient, account: &Pubkey) -> Result<bool> {
    Ok(rpc
        .get_account_with_commitment(account, CommitmentConfig::confirmed())?
        .value
        .is_some())
}
