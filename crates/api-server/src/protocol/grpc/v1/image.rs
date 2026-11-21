//! `ImageService` over [`Images`].

use std::sync::Arc;

use futures_util::StreamExt as _;
use needy_api_proto::v1::image_service_server::ImageService;
use needy_api_proto::v1::{
  BuildImageRequest, BuildImageResponse, ExportImageRequest, ExportImageResponse, FinishImageEditRequest, FinishImageEditResponse,
  LogImageRequest, LogImageResponse, RollbackImageRequest, RollbackImageResponse, StartImageEditRequest, StartImageEditResponse,
};
use tonic::{Request, Response, Status};

use super::{Streamed, caller};
use crate::control::{Caller, ControlError, Images};
use crate::protocol::{domain, status};

/// Maximum bytes per `ExportImageResponse` message (1 MiB, well under gRPC's
/// default 4 MiB message limit).
const EXPORT_CHUNK: usize = 1024 * 1024;

pub struct Adapter<I> {
  pub images: Arc<I>,
  pub caller: Caller,
}

impl<I> Adapter<I> {
  fn operator<T>(&self, request: &Request<T>, refusal: &str) -> Result<Caller, Status> {
    match caller(&self.caller, request) {
      caller @ (Caller::Operator | Caller::Client { .. }) => Ok(caller),
      Caller::Actor { .. } => Err(ControlError::role_denied(refusal).into()),
      Caller::Runner { .. } => Err(ControlError::role_denied("runners cannot manage images").into()),
    }
  }
}

#[tonic::async_trait]
impl<I: Images> ImageService for Adapter<I> {
  async fn build_image(&self, request: Request<BuildImageRequest>) -> Result<Response<BuildImageResponse>, Status> {
    self.operator(&request, "actors cannot build images")?;
    let request = domain::image_build(request.into_inner()).map_err(ControlError::from)?;
    let built = self.images.build(request).await?;
    Ok(Response::new(domain::proto_image_build(built)))
  }

  async fn start_image_edit(&self, request: Request<StartImageEditRequest>) -> Result<Response<StartImageEditResponse>, Status> {
    self.operator(&request, "actors cannot edit images")?;
    let request = domain::image_edit_start(request.into_inner()).map_err(ControlError::from)?;
    let started = self.images.start_edit(request).await?;
    Ok(Response::new(domain::proto_image_edit_started(started)))
  }

  async fn finish_image_edit(&self, request: Request<FinishImageEditRequest>) -> Result<Response<FinishImageEditResponse>, Status> {
    self.operator(&request, "actors cannot edit images")?;
    let request = request.into_inner();
    let finished = self.images.finish_edit(&request.session_id, request.abort).await?;
    Ok(Response::new(domain::proto_image_edit_finished(finished)))
  }

  async fn rollback_image(&self, request: Request<RollbackImageRequest>) -> Result<Response<RollbackImageResponse>, Status> {
    self.operator(&request, "actors cannot roll back images")?;
    let request = domain::image_rollback(request.into_inner()).map_err(ControlError::from)?;
    let rolled = self.images.rollback(request).await?;
    Ok(Response::new(domain::proto_image_rollback(rolled)))
  }

  async fn log_image(&self, request: Request<LogImageRequest>) -> Result<Response<LogImageResponse>, Status> {
    self.operator(&request, "actors cannot read image history")?;
    let request = domain::image_log(request.into_inner()).map_err(ControlError::from)?;
    let logged = self.images.log(request).await?;
    Ok(Response::new(domain::proto_image_log(logged)))
  }

  type ExportImageStream = Streamed<ExportImageResponse>;

  async fn export_image(&self, request: Request<ExportImageRequest>) -> Result<Response<Self::ExportImageStream>, Status> {
    self.operator(&request, "actors cannot export images")?;
    let request = domain::image_export(request.into_inner()).map_err(ControlError::from)?;
    let chunks = self.images.export(request).await?;
    let messages = chunks.flat_map(|read| {
      let messages: Vec<Result<ExportImageResponse, Status>> = match read {
        Ok(bytes) => bytes
          .chunks(EXPORT_CHUNK)
          .map(|data| {
            Ok(ExportImageResponse {
              data: data.to_vec(),
            })
          })
          .collect(),
        Err(e) => vec![Err(status::status(e))],
      };
      tokio_stream::iter(messages)
    });
    Ok(Response::new(Box::pin(messages)))
  }
}
