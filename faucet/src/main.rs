mod api;
mod batch;
mod config;
mod error;
mod queue;
mod tx;

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "faucet=info".parse().unwrap()),
        )
        .init();

    let config = config::Config::from_env();
    info!(
        funding_account = %config.funding_public,
        contract = %config.contract_address,
        token = %config.token_address,
        amount = %config.amount,
        max_batch_size = %config.max_batch_size,
        "Starting faucet"
    );

    let rpc = stellar_rpc_client::Client::new(&config.rpc_url)
        .expect("Failed to create RPC client");

    // Startup checks
    info!("Verifying contract is deployed...");
    tx::verify_contract_deployed(&rpc, &config.contract_address)
        .await
        .expect("Contract verification failed");

    info!("Verifying funding account exists...");
    tx::verify_account_exists(&rpc, &config.funding_public)
        .await
        .expect("Funding account verification failed");

    info!("Startup checks passed");

    let queue = Arc::new(queue::FaucetQueue::new(config.max_batch_size));
    let port = config.port;
    let config = Arc::new(config);

    let state = Arc::new(api::AppState {
        queue: queue.clone(),
        contract_address: config.contract_address.clone(),
        token_address: config.token_address.clone(),
        funding_account: config.funding_public.clone(),
        max_batch_size: config.max_batch_size,
        amount: config.amount.to_string(),
    });

    tokio::spawn(batch::batch_loop(queue, rpc, config));

    let app = Router::new()
        .route("/", get(api::fund_handler).post(api::fund_handler))
        .route("/health", get(api::health_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("Failed to bind");
    info!(port = %port, "Faucet listening");
    axum::serve(listener, app).await.expect("Server error");
}
