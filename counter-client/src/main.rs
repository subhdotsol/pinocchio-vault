//! Calls the deployed counter with v1 transactions (SIMD-0385) and checks the result.
//!
//! On start it asks for the cluster (`cluster.rs`): localnet (surfpool at 127.0.0.1:8899, must
//! be running) or devnet (`DEVNET_RPC_URL` from `../.env`).
//!
//! KEYPAIR      default ~/.config/solana/id.json (fee payer + counter authority)
//! PROGRAM_ID   default: pubkey of ../target/deploy/auction-keypair.json

mod cluster;

use anyhow::{bail, ensure, Context, Result};
use solana_commitment_config::CommitmentConfig;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::{read_keypair_file, Keypair};
use solana_message::{v1, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::config::{RpcTransactionConfig, UiTransactionEncoding};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

const SYSTEM_PROGRAM: Pubkey = Pubkey::new_from_array([0; 32]);
const PROGRAM_KEYPAIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../target/deploy/auction-keypair.json"
);

/// v1 resource limits live in the message header. Unset limits are ZERO, so set both:
/// - CU limit: initialize (PDA search + CreateAccount CPI) uses a few thousand CU.
/// - Loaded accounts data: must cover every loaded account, including the ProgramData account
///   (~.so size). Too small fails with `MaxLoadedAccountsDataSizeExceeded`.
/// - Priority fee: TOTAL lamports for the transaction, not micro-lamports per CU.
const CONFIG: v1::TransactionConfig = v1::TransactionConfig::empty()
    .with_compute_unit_limit(20_000)
    .with_loaded_accounts_data_size_limit(64 * 1024)
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
        Err(_) => read_keypair_file(PROGRAM_KEYPAIR)
            .map_err(|e| anyhow::anyhow!("failed to read {PROGRAM_KEYPAIR}: {e}"))?
            .pubkey(),
    };
    let rpc = RpcClient::new_with_commitment(cluster.rpc_url.clone(), CommitmentConfig::confirmed());
    cluster.check_reachable(&rpc)?;
    let (counter, _) =
        Pubkey::find_program_address(&[b"counter", payer.pubkey().as_ref()], &program_id);

    println!("cluster: {}", cluster.label());
    println!("program: {program_id}");
    println!("payer:   {}", payer.pubkey());
    println!("counter: {counter}\n");

    // 1. initialize, unless this payer's counter already exists.
    let before = match fetch_count(&rpc, &counter, &program_id)? {
        Some(count) => {
            println!("[initialize] skipped, counter exists (count = {count})");
            count
        }
        None => {
            let ix = Instruction::new_with_bytes(
                program_id,
                &[0],
                vec![
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new(counter, false),
                    AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
                ],
            );
            let sig = send_v1(&rpc, ix, &payer, &[&payer]).context("initialize failed")?;
            println!("[initialize] tx: {sig}");
            verify_v1_on_chain(&rpc, &sig)?;
            let count = fetch_count(&rpc, &counter, &program_id)?
                .context("counter missing after initialize")?;
            ensure!(count == 0, "expected count 0 after initialize, got {count}");
            count
        }
    };

    // 2. increment by the authority.
    let sig = send_v1(
        &rpc,
        increment_ix(program_id, payer.pubkey(), counter),
        &payer,
        &[&payer],
    )
    .context("increment failed")?;
    println!("[increment]  tx: {sig}");
    verify_v1_on_chain(&rpc, &sig)?;
    let after = fetch_count(&rpc, &counter, &program_id)?.context("counter missing")?;
    ensure!(
        after == before + 1,
        "expected count {}, got {after}",
        before + 1
    );
    println!("[increment]  ok: {before} -> {after}");

    // 3. a different signer must be rejected (custom error 0 = Unauthorized).
    let intruder = Keypair::new();
    let ix = increment_ix(program_id, intruder.pubkey(), counter);
    match send_v1(&rpc, ix, &payer, &[&payer, &intruder]) {
        Ok(sig) => bail!("increment by a non-authority succeeded: {sig}"),
        Err(e) => {
            let msg = format!("{e:#}");
            ensure!(
                msg.contains("custom program error: 0x0"),
                "unexpected error: {msg}"
            );
            println!("[unauthorized] ok: rejected with custom program error 0x0");
        }
    }
    let unchanged = fetch_count(&rpc, &counter, &program_id)?;
    ensure!(
        unchanged == Some(after),
        "count changed on rejected increment"
    );

    println!("\nall checks passed");
    Ok(())
}

fn increment_ix(program_id: Pubkey, authority: Pubkey, counter: Pubkey) -> Instruction {
    Instruction::new_with_bytes(
        program_id,
        &[1],
        vec![
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new(counter, false),
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
