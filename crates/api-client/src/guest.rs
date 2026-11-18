//! The actor's side of the guest API where one call is not enough: the
//! upload of a session's upper layer, streamed as it is read.

use needy_api_proto::v1::PushLayerRequest;
use tokio::io::AsyncRead;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::io::ReaderStream;

use crate::{Client, Error, Result};

// NOTICE: 1 MiB chunks stay under gRPC's default 4 MiB message limit.
const CHUNK: usize = 1 << 20;

/// Streams the tar archive `archive` reads to the daemon as the session's
/// upper layer (`GuestService.PushLayer`), never holding it whole. Only an
/// archive read to its end is sent with its last part; a read that fails
/// sends none, so the daemon keeps nothing of the upload.
pub async fn push_layer(client: &Client, archive: impl AsyncRead + Send + Unpin + 'static) -> Result<()> {
  // A few chunks in flight keep the reader ahead of the connection.
  let (sender, receiver) = mpsc::channel(4);
  let mut guest = client.guest();
  let call = guest.push_layer(ReceiverStream::new(receiver));
  tokio::pin!(call);
  let read = async move {
    let mut chunks = ReaderStream::with_capacity(archive, CHUNK);
    while let Some(chunk) = chunks.next().await {
      let request = PushLayerRequest {
        data: chunk?.to_vec(),
        last: false,
      };
      if sender.send(request).await.is_err() {
        // The call ended first; its outcome says why.
        return Ok(());
      }
    }
    // The archive ended: an empty last part marks it whole. A send that
    // fails means the call ended, as above.
    let last = PushLayerRequest {
      data: Vec::new(),
      last: true,
    };
    if sender.send(last).await.is_err() {
      tracing::debug!("the layer upload ended before its last part was sent");
    }
    Ok::<_, std::io::Error>(())
  };
  tokio::select! {
    read = read => read.map_err(Error::Read)?,
    // The daemon answered, or refused, before the archive was read out.
    response = &mut call => return response.map(drop).map_err(Error::from),
  }
  call.await.map(drop).map_err(Error::from)
}
