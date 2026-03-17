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
            r#type: "https://stellar.org/friendbot-errors/bad_request".into(),
            title: "Bad Request".into(),
            status: 400,
            detail: reason.to_string(),
        }
    }

    pub fn already_pending(addr: &str) -> Self {
        Self {
            r#type: "https://stellar.org/friendbot-errors/bad_request".into(),
            title: "Bad Request".into(),
            status: 409,
            detail: format!("Address {addr} is already in the pending queue"),
        }
    }

    pub fn queue_full() -> Self {
        Self {
            r#type: "https://stellar.org/friendbot-errors/bad_request".into(),
            title: "Bad Request".into(),
            status: 503,
            detail: "The faucet queue is full, please try again later".into(),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            r#type: "https://stellar.org/friendbot-errors/internal_server_error".into(),
            title: "Internal Server Error".into(),
            status: 500,
            detail: msg.into(),
        }
    }

    pub fn transfer_failed(addr: &str) -> Self {
        Self {
            r#type: "https://stellar.org/friendbot-errors/transaction_failed".into(),
            title: "Transaction Failed".into(),
            status: 400,
            detail: format!("Transfer to {addr} returned false"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    #[test]
    fn test_invalid_address_fields() {
        let p = ProblemDetail::invalid_address("bad addr");
        assert_eq!(p.status, 400);
        assert_eq!(p.title, "Bad Request");
        assert!(p.detail.contains("bad addr"));
        assert!(p.r#type.contains("friendbot-errors/bad_request"));
    }

    #[test]
    fn test_already_pending_fields() {
        let p = ProblemDetail::already_pending("GABC");
        assert_eq!(p.status, 409);
        assert_eq!(p.title, "Bad Request");
        assert!(p.detail.contains("GABC"));
        assert!(p.r#type.contains("friendbot-errors/bad_request"));
    }

    #[test]
    fn test_queue_full_fields() {
        let p = ProblemDetail::queue_full();
        assert_eq!(p.status, 503);
        assert_eq!(p.title, "Bad Request");
        assert!(p.r#type.contains("friendbot-errors/bad_request"));
    }

    #[test]
    fn test_internal_fields() {
        let p = ProblemDetail::internal("something broke");
        assert_eq!(p.status, 500);
        assert_eq!(p.title, "Internal Server Error");
        assert!(p.detail.contains("something broke"));
    }

    #[test]
    fn test_transfer_failed_fields() {
        let p = ProblemDetail::transfer_failed("GXYZ");
        assert_eq!(p.status, 400);
        assert_eq!(p.title, "Transaction Failed");
        assert!(p.detail.contains("GXYZ"));
    }

    #[tokio::test]
    async fn test_into_response_status_and_content_type() {
        let p = ProblemDetail::invalid_address("test");
        let response = p.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/problem+json"
        );

        // Verify body is valid JSON with expected fields
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], 400);
        assert_eq!(json["title"], "Bad Request");
    }

    #[tokio::test]
    async fn test_into_response_503() {
        let p = ProblemDetail::queue_full();
        let response = p.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
