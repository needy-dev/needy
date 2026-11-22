//! `ImageService` over [`Images`].

use std::sync::Arc;

use futures_util::StreamExt as _;
use needy_api_proto::v1::image_service_server::ImageService;
use needy_api_proto::v1::{
  BuildImageRequest, BuildImageResponse, ExportImageRequest, ExportImageResponse, FinishImageEditRequest, FinishImageEditResponse,
  LogImageRequest, LogImageResponse, PruneImagesRequest, PruneImagesResponse, RollbackImageRequest, RollbackImageResponse,
  StartImageEditRequest, StartImageEditResponse,
};
use tonic::{Request, Response, Status};

use super::{CHUNK, Streamed, operator};
use crate::control::{Caller, Images};

pub struct Adapter<I> {
  pub images: Arc<I>,
  pub caller: Caller,
}

impl<I> Adapter<I> {
  fn operator<T>(&self, request: &Request<T>, refusal: &str) -> Result<Caller, Status> {
    operator(&self.caller, request, refusal, "runners cannot manage images")
  }
}

#[tonic::async_trait]
impl<I: Images> ImageService for Adapter<I> {
  async fn build_image(&self, request: Request<BuildImageRequest>) -> Result<Response<BuildImageResponse>, Status> {
    self.operator(&request, "actors cannot build images")?;
    let request = request.into_inner().try_into()?;
    let built = self.images.build(request).await?;
    Ok(Response::new(built.into()))
  }

  async fn start_image_edit(&self, request: Request<StartImageEditRequest>) -> Result<Response<StartImageEditResponse>, Status> {
    self.operator(&request, "actors cannot edit images")?;
    let request = request.into_inner().try_into()?;
    let started = self.images.start_edit(request).await?;
    Ok(Response::new(started.into()))
  }

  async fn finish_image_edit(&self, request: Request<FinishImageEditRequest>) -> Result<Response<FinishImageEditResponse>, Status> {
    self.operator(&request, "actors cannot edit images")?;
    let request = request.into_inner();
    let finished = self.images.finish_edit(&request.session_id, request.abort).await?;
    Ok(Response::new(finished.into()))
  }

  async fn rollback_image(&self, request: Request<RollbackImageRequest>) -> Result<Response<RollbackImageResponse>, Status> {
    self.operator(&request, "actors cannot roll back images")?;
    let request = request.into_inner().try_into()?;
    let rolled = self.images.rollback(request).await?;
    Ok(Response::new(rolled.into()))
  }

  async fn log_image(&self, request: Request<LogImageRequest>) -> Result<Response<LogImageResponse>, Status> {
    self.operator(&request, "actors cannot read image history")?;
    let request = request.into_inner().try_into()?;
    let logged = self.images.log(request).await?;
    Ok(Response::new(logged.into()))
  }

  type ExportImageStream = Streamed<ExportImageResponse>;

  async fn export_image(&self, request: Request<ExportImageRequest>) -> Result<Response<Self::ExportImageStream>, Status> {
    self.operator(&request, "actors cannot export images")?;
    let request = request.into_inner().try_into()?;
    let chunks = self.images.export(request).await?;
    let messages = chunks.flat_map(|read| {
      let messages: Vec<Result<ExportImageResponse, Status>> = match read {
        Ok(bytes) => bytes
          .chunks(CHUNK)
          .map(|data| {
            Ok(ExportImageResponse {
              data: data.to_vec(),
            })
          })
          .collect(),
        Err(e) => vec![Err(e.into())],
      };
      tokio_stream::iter(messages)
    });
    Ok(Response::new(Box::pin(messages)))
  }

  async fn prune_images(&self, request: Request<PruneImagesRequest>) -> Result<Response<PruneImagesResponse>, Status> {
    self.operator(&request, "actors cannot prune images")?;
    let pruned = self.images.prune(request.into_inner().into()).await?;
    Ok(Response::new(pruned.into()))
  }
}
