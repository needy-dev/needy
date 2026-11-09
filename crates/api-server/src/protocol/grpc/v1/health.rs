//! `HealthService`: who this server is. The only method that needs no
//! token.

use needy_api_proto::v1::health_service_server::HealthService;
use needy_api_proto::v1::{CheckRequest, CheckResponse};
use tonic::{Request, Response, Status};

pub struct Health {
  pub instance_id: String,
  /// `control` or `executor`.
  pub role: &'static str,
}

#[tonic::async_trait]
impl HealthService for Health {
  async fn check(&self, _: Request<CheckRequest>) -> Result<Response<CheckResponse>, Status> {
    Ok(Response::new(CheckResponse {
      instance_id: self.instance_id.clone(),
      version: env!("CARGO_PKG_VERSION").into(),
      role: self.role.into(),
      platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
    }))
  }
}
