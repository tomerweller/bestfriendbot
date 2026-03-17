use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct ProblemDetail {
    pub r#type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
}

impl IntoResponse for ProblemDetail {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = axum::Json(&self).into_response();
        *response.status_mut() = status;
        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            "application/problem+json".parse().unwrap(),
        );
        response
    }
}

impl ProblemDetail {
    pub fn invalid_address(reason: &str) -> Self {
        Self {
            r#type: "https://stellar.org/horizon-errors/bad_request".into(),
            title: "Invalid Address".into(),
            status: 400,
            detail: reason.to_string(),
        }
    }

    pub fn already_pending(addr: &str) -> Self {
        Self {
            r#type: "https://stellar.org/horizon-errors/conflict".into(),
            title: "Already Pending".into(),
            status: 409,
            detail: format!("Address {addr} is already in the pending queue"),
        }
    }

    pub fn queue_full() -> Self {
        Self {
            r#type: "https://stellar.org/horizon-errors/service_unavailable".into(),
            title: "Queue Full".into(),
            status: 503,
            detail: "The faucet queue is full, please try again later".into(),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            r#type: "https://stellar.org/horizon-errors/internal_server_error".into(),
            title: "Internal Server Error".into(),
            status: 500,
            detail: msg.into(),
        }
    }

    pub fn transfer_failed(addr: &str) -> Self {
        Self {
            r#type: "https://stellar.org/horizon-errors/transaction_failed".into(),
            title: "Transfer Failed".into(),
            status: 400,
            detail: format!("Transfer to {addr} returned false"),
        }
    }
}
