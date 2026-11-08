//! Protocol adapters for serving the Needy daemon API.
//!
//! The daemon implements the generated service traits re-exported from
//! [`proto`]. This crate adds what every listener needs: bearer
//! authentication in front of all methods except pairing.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tonic::Status;
use tonic::body::Body;
use tower::{Layer, Service};

pub use needy_api_proto as proto;

/// Methods that a client may call before it is paired.
pub const PUBLIC_METHODS: &[&str] = &["/needy.api.daemon.v1.PairingService/PairClient"];

/// Checks a bearer token. The daemon owns token storage and revocation.
pub trait Authenticator: Send + Sync + 'static {
    fn authenticate(&self, token: &str) -> bool;
}

/// Tower layer that rejects requests without a valid bearer token.
#[derive(Clone)]
pub struct AuthLayer {
    auth: Arc<dyn Authenticator>,
}

impl AuthLayer {
    pub fn new(auth: impl Authenticator) -> Self {
        Self {
            auth: Arc::new(auth),
        }
    }
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthService {
            inner,
            auth: self.auth.clone(),
        }
    }
}

#[derive(Clone)]
pub struct AuthService<S> {
    inner: S,
    auth: Arc<dyn Authenticator>,
}

impl<S> AuthService<S> {
    fn allows<B>(&self, request: &http::Request<B>) -> bool {
        if PUBLIC_METHODS.contains(&request.uri().path()) {
            return true;
        }
        request
            .headers()
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|token| self.auth.authenticate(token))
    }
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

impl<S, B> Service<http::Request<B>> for AuthService<S>
where
    S: Service<http::Request<B>, Response = http::Response<Body>>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        if self.allows(&request) {
            Box::pin(self.inner.call(request))
        } else {
            let response = Status::unauthenticated("missing or invalid bearer token").into_http();
            Box::pin(async move { Ok(response) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use needy_api_client::Client;
    use proto::daemon::v1::health_service_server::{HealthService, HealthServiceServer};
    use proto::daemon::v1::pairing_service_server::{PairingService, PairingServiceServer};
    use proto::daemon::v1::{
        CheckRequest, CheckResponse, HealthStatus, ListClientsRequest, ListClientsResponse,
        PairClientRequest, PairClientResponse, RevokeClientRequest, RevokeClientResponse,
    };
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::{Code, Request, Response};

    struct Token;

    impl Authenticator for Token {
        fn authenticate(&self, token: &str) -> bool {
            token == "secret"
        }
    }

    struct Health;

    #[tonic::async_trait]
    impl HealthService for Health {
        async fn check(&self, _: Request<CheckRequest>) -> Result<Response<CheckResponse>, Status> {
            Ok(Response::new(CheckResponse {
                status: HealthStatus::Serving.into(),
                ..Default::default()
            }))
        }
    }

    struct Pairing;

    #[tonic::async_trait]
    impl PairingService for Pairing {
        async fn pair_client(
            &self,
            _: Request<PairClientRequest>,
        ) -> Result<Response<PairClientResponse>, Status> {
            Ok(Response::new(PairClientResponse {
                client_id: "c1".into(),
                token: "secret".into(),
            }))
        }

        async fn list_clients(
            &self,
            _: Request<ListClientsRequest>,
        ) -> Result<Response<ListClientsResponse>, Status> {
            Ok(Response::new(ListClientsResponse::default()))
        }

        async fn revoke_client(
            &self,
            _: Request<RevokeClientRequest>,
        ) -> Result<Response<RevokeClientResponse>, Status> {
            Ok(Response::new(RevokeClientResponse {}))
        }
    }

    #[tokio::test]
    async fn pairing_is_public_and_other_methods_need_a_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(
            tonic::transport::Server::builder()
                .layer(AuthLayer::new(Token))
                .add_service(HealthServiceServer::new(Health))
                .add_service(PairingServiceServer::new(Pairing))
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );

        let client = Client::connect(url).await.unwrap();
        let err = client.health().check(CheckRequest {}).await.unwrap_err();
        assert_eq!(err.code(), Code::Unauthenticated);

        let paired = client
            .pairing()
            .pair_client(PairClientRequest::default())
            .await
            .unwrap()
            .into_inner();

        let client = client.with_token(&paired.token).unwrap();
        let health = client.health().check(CheckRequest {}).await.unwrap();
        assert_eq!(health.into_inner().status(), HealthStatus::Serving);

        let client = client.with_token("wrong").unwrap();
        let err = client.health().check(CheckRequest {}).await.unwrap_err();
        assert_eq!(err.code(), Code::Unauthenticated);
    }
}
