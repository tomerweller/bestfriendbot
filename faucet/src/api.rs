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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const VALID_G: &str = "GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7";

    fn test_state(max_size: usize) -> Arc<AppState> {
        Arc::new(AppState {
            queue: Arc::new(FaucetQueue::new(max_size)),
            contract_address: "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC".into(),
            token_address: "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC".into(),
            funding_account: VALID_G.into(),
            max_batch_size: 65,
            amount: "10000000".into(),
        })
    }

    fn test_app(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/", get(fund_handler))
            .route("/health", get(health_handler))
            .with_state(state)
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let state = test_state(10);
        let app = test_app(state);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "healthy");
        assert_eq!(json["queue_size"], 0);
        assert_eq!(json["max_batch_size"], 65);
    }

    #[tokio::test]
    async fn test_fund_valid_address() {
        let state = test_state(10);
        let queue = state.queue.clone();
        let app = test_app(state);

        // Spawn a task that drains the queue and sends a success response
        tokio::spawn(async move {
            // Wait briefly for the request to enqueue
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let entries = queue.drain();
            assert_eq!(entries.len(), 1);
            for entry in entries {
                let _ = entry.responder.send(Ok(SuccessResponse {
                    successful: true,
                    hash: "abc123".into(),
                    envelope_xdr: "AAAA".into(),
                }));
            }
        });

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/?addr={VALID_G}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 200);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["successful"], true);
        assert_eq!(json["hash"], "abc123");
        assert_eq!(json["envelope_xdr"], "AAAA");
    }

    #[tokio::test]
    async fn test_fund_invalid_address() {
        let state = test_state(10);
        let app = test_app(state);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/?addr=NOTAVALIDADDRESS")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 400);
    }

    #[tokio::test]
    async fn test_fund_missing_addr() {
        let state = test_state(10);
        let app = test_app(state);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 400);
    }

    #[tokio::test]
    async fn test_fund_duplicate_address() {
        let state = test_state(10);
        // Pre-enqueue the address so it's already pending
        let _rx = state.queue.enqueue(VALID_G.into()).unwrap();

        let app = test_app(state);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/?addr={VALID_G}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 409);
    }

    #[tokio::test]
    async fn test_fund_queue_full() {
        let state = test_state(1); // capacity of 1
        // Fill the queue
        let _rx = state.queue.enqueue("GDUMMY000000000000000000000000000000000000000000000000000".into()).unwrap();

        let app = test_app(state);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/?addr={VALID_G}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), 503);
    }
}
