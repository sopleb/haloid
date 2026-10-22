//! The sidecar's API: nothing without the token, and nothing unsigned.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use haloid_updater::{Config, Updater, http};
use std::time::Duration;
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn app() -> axum::Router {
    // Never reached: every request here is refused before Docker is asked.
    let docker =
        bollard::Docker::connect_with_http("http://127.0.0.1:9", 1, bollard::API_DEFAULT_VERSION)
            .unwrap();
    http::router(Updater::new(
        docker,
        Config {
            token: TOKEN.into(),
            container: Some("haloid".into()),
            trusted: vec![haloid_release::public_key(&[4; 32])],
            health_timeout: Duration::from_secs(1),
        },
    ))
}

async fn send(auth: Option<&str>, path: &str, body: Option<&str>) -> StatusCode {
    let mut request =
        Request::builder()
            .uri(path)
            .method(if body.is_some() { "POST" } else { "GET" });
    if let Some(auth) = auth {
        request = request.header(header::AUTHORIZATION, auth);
    }
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let request = request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    app().oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn only_haloid_with_the_token_may_ask_and_only_for_a_signed_release() {
    assert_eq!(send(None, "/healthz", None).await, StatusCode::OK);
    assert_eq!(
        send(None, "/v1/status", None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(Some("Bearer nope"), "/v1/status", None).await,
        StatusCode::UNAUTHORIZED
    );
    let bearer = format!("Bearer {TOKEN}");
    assert_eq!(
        send(Some(&bearer), "/v1/status", None).await,
        StatusCode::OK
    );

    let unsigned = r#"{"release": "HALOIDIMG1.e30.AAAA"}"#;
    assert_eq!(
        send(None, "/v1/update", Some(unsigned)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(Some(&bearer), "/v1/update", Some(unsigned)).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
