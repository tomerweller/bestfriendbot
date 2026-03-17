use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;
use tracing::{error, info};

use crate::config::Config;
use crate::error::ProblemDetail;
use crate::queue::{FaucetQueue, PendingEntry, SuccessResponse};
use crate::tx;

pub async fn batch_loop(
    queue: Arc<FaucetQueue>,
    rpc: stellar_rpc_client::Client,
    config: Arc<Config>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        let entries = queue.drain();
        if entries.is_empty() {
            continue;
        }
        info!(count = entries.len(), "Dispatching batch");
        dispatch_batch(entries, &rpc, &config).await;
    }
}

async fn dispatch_batch(
    entries: Vec<PendingEntry>,
    rpc: &stellar_rpc_client::Client,
    config: &Config,
) {
    let receivers: Vec<(String, i128)> = entries
        .iter()
        .map(|e| (e.address.clone(), config.amount))
        .collect();

    let result = tx::invoke_batch_transfer(
        rpc,
        &config.network_passphrase,
        &config.funding_secret,
        &config.funding_public,
        &config.contract_address,
        &config.token_address,
        &receivers,
    )
    .await;

    match result {
        Ok((tx_hash, envelope_xdr, results)) => {
            info!(tx_hash = %tx_hash, count = entries.len(), "Batch transfer succeeded");

            // If we got per-entry results, use them; otherwise assume all succeeded
            let has_per_entry = !results.is_empty() && results.len() == entries.len();

            for (i, entry) in entries.into_iter().enumerate() {
                let succeeded = if has_per_entry { results[i] } else { true };
                let response = if succeeded {
                    Ok(SuccessResponse {
                        successful: true,
                        hash: tx_hash.clone(),
                        envelope_xdr: envelope_xdr.clone(),
                    })
                } else {
                    Err(ProblemDetail::transfer_failed(&entry.address))
                };
                let _ = entry.responder.send(response);
            }
        }
        Err(e) => {
            error!(error = %e, "Batch transfer failed");
            for entry in entries {
                let _ = entry
                    .responder
                    .send(Err(ProblemDetail::internal(format!(
                        "Batch transfer failed: {e}"
                    ))));
            }
        }
    }
}
