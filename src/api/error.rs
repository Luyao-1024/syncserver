use crate::Error;
use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;

pub(crate) struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub reason: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    server_error_code: &'static str,
    reason: &'static str,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, reason: &'static str) -> Self {
        Self {
            status,
            code,
            reason,
        }
    }
    pub fn bad_request() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "BAD_REQUEST",
            "Invalid JSON request or unsupported request parameter",
        )
    }
    pub fn unsupported() -> Self {
        Self::new(
            StatusCode::NOT_IMPLEMENTED,
            "UNSUPPORTED_CAPABILITY",
            "This capability is not implemented in M2",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ErrorBody {
                server_error_code: self.code,
                reason: self.reason,
            }),
        )
            .into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
        }
        if self.status == StatusCode::SERVICE_UNAVAILABLE {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, "1".parse().unwrap());
        }
        response
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        match error {
            Error::Busy => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVER_BUSY",
                "Database queue is full; retry later",
            ),
            Error::Unavailable | Error::WorkerFailed => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                "Database executor is unavailable",
            ),
            _ => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Database operation failed",
            ),
        }
    }
}

impl From<crate::domain::records::Failure> for ApiError {
    fn from(error: crate::domain::records::Failure) -> Self {
        Self::new(
            StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            error.code,
            error.reason,
        )
    }
}
