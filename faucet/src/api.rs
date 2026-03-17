use std::sync::Arc;

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::ProblemDetail;
use crate::queue::{FaucetQueue, SuccessResponse};

pub struct AppState {
    pub queue: Arc<FaucetQueue>,
    pub contract_address: String,
    pub token_address: String,
    pub funding_account: String,
    pub max_batch_size: usize,
    pub amount: String,
}

#[derive(Deserialize)]
pub struct FundQuery {
    pub addr: String,
}

pub async fn fund_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FundQuery>,
) -> Result<Json<SuccessResponse>, ProblemDetail> {
    let addr = query.addr.trim().to_string();

    // Validate address
    stellar_strkey::ed25519::PublicKey::from_string(&addr)
        .map_err(|e| ProblemDetail::invalid_address(&format!("Invalid Stellar address: {e}")))?;

    // Enqueue and wait
    let rx = state.queue.enqueue(addr)?;

    // Wait for batch result
    let result = rx
        .await
        .map_err(|_| ProblemDetail::internal("Batch processor dropped the request"))?;

    result.map(Json)
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub contract_address: String,
    pub token_address: String,
    pub funding_account: String,
    pub queue_size: usize,
    pub max_batch_size: usize,
    pub amount_per_funding: String,
}

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "healthy".into(),
        contract_address: state.contract_address.clone(),
        token_address: state.token_address.clone(),
        funding_account: state.funding_account.clone(),
        queue_size: state.queue.len(),
        max_batch_size: state.max_batch_size,
        amount_per_funding: state.amount.clone(),
    })
}
