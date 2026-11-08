//! Asks on stdin which cluster to run against.
//!
//! - localnet: surfpool at `http://127.0.0.1:8899`, which must already be running.
//! - devnet: `DEVNET_RPC_URL`, from the environment or the project's `.env` file. A variable
//!   already set in the shell wins over `.env`.
//!
//! The same file is in `counter-client/src` and `vault-client/src`; keep them identical.

use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, bail};
use solana_rpc_client::rpc_client::RpcClient;

const LOCALNET_RPC_URL: &str = "http://127.0.0.1:8899";
const ENV_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../.env");

pub struct Cluster {
    pub name: &'static str,
    pub rpc_url: String,
}

impl Cluster {
    /// Name and host only. A provider URL can carry its API key in the path or query, so the
    /// full URL is never printed.
    pub fn label(&self) -> String {
        let host = self.rpc_url.split("://").nth(1).unwrap_or(&self.rpc_url);
        let host = host.split(['/', '?']).next().unwrap_or_default();
        format!("{} ({host})", self.name)
    }

    /// Fails with a readable message when the RPC endpoint does not answer.
    pub fn check_reachable(&self, rpc: &RpcClient) -> Result<()> {
        rpc.get_version().map(|_| ()).with_context(|| match self.name {
            "localnet" => format!(
                "surfpool is not reachable at {LOCALNET_RPC_URL}; start it in the project root \
                 with `surfpool start --offline --no-deploy` and deploy the program"
            ),
            _ => format!("{} is not reachable", self.label()),
        })
    }
}

/// Prompts until the user picks localnet or devnet.
pub fn select() -> Result<Cluster> {
    match dotenvy::from_path(ENV_FILE) {
        Err(e) if !e.not_found() => return Err(e).context("cannot read .env"),
        _ => {}
    }

    let stdin = io::stdin();
    let mut line = String::new();
    loop {
        print!(
            "Run against:\n  1) localnet  {LOCALNET_RPC_URL} (surfpool)\n  2) devnet    \
             DEVNET_RPC_URL from .env\nchoice [1/2]: "
        );
        io::stdout().flush()?;
        line.clear();
        if stdin.lock().read_line(&mut line)? == 0 {
            bail!("no cluster selected: stdin closed");
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "1" | "l" | "localnet" => {
                println!(
                    "\nWARNING: localnet needs surfpool running in the background, with the \
                     program deployed:\n  surfpool start --offline --no-deploy   (project root, \
                     separate terminal)\n"
                );
                return Ok(Cluster {
                    name: "localnet",
                    rpc_url: LOCALNET_RPC_URL.into(),
                });
            }
            "2" | "d" | "devnet" => {
                let rpc_url = std::env::var("DEVNET_RPC_URL")
                    .ok()
                    .filter(|url| !url.trim().is_empty())
                    .context("DEVNET_RPC_URL is not set; add it to .env (see .env.example)")?;
                println!();
                return Ok(Cluster {
                    name: "devnet",
                    rpc_url,
                });
            }
            other => println!("unknown choice {other:?}, type 1 or 2\n"),
        }
    }
}
