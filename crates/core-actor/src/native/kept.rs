//! The kept-command service: init's process service for the shims. A shim
//! inside the fence cannot execute its real program, since the fence denies
//! every exec but the shims, so it asks init (outside the fence) to start
//! it: init finds the program by its bare name on the actor's search path
//! and starts it with `sandbox-exec` under the same fence, with that one
//! program allowed. Its own children meet the fence and the shims again.
//!
//! The client names only the program's name, never a path: whatever it
//! sends, what runs is a program on the search path, under the fence.

use std::path::{Path, PathBuf};

use futures_util::StreamExt as _;
use needy_api_proto::v1::process_service_server::ProcessService;
use needy_api_proto::v1::{ExecRequest, KillProcessRequest, KillProcessResponse, ListProcessesRequest, ListProcessesResponse, exec_request};
use needy_core_executor::service::{ExecutorService, Inbound};
use tonic::{Request, Response, Status, Streaming};

use crate::host::macos::{SANDBOX_EXEC, fence};

pub(super) struct Kept {
  pub processes: ExecutorService,
  /// The fence's profile.
  pub fence: PathBuf,
  /// The actor's search path: where its shims' programs are.
  pub search: String,
  /// The active developer directory (`xcode-select -p`), whose trampolines
  /// in `/usr/bin` run its programs.
  pub developer: Option<PathBuf>,
}

/// The command that starts `program` with `args` (its argv after argv[0])
/// under the fence in `profile`.
pub(super) fn command(profile: &Path, program: &Path, developer: Option<&Path>, args: impl IntoIterator<Item = String>) -> Vec<String> {
  let tools = fence::tools(program, developer);
  let mut argv = vec![SANDBOX_EXEC.to_string()];
  argv.extend(fence::arguments(profile, &fence::allowed(program), tools.as_deref()));
  argv.extend(args);
  argv
}

/// The real program the bare name `name` names on `search`, by its real
/// path, which is what Seatbelt checks. `None` for a path or a name that is
/// not there.
pub(super) fn program(name: &str, search: &str) -> Option<PathBuf> {
  use std::os::unix::fs::PermissionsExt;
  if name.is_empty() || name.contains('/') {
    return None;
  }
  search
    .split(':')
    .filter(|dir| !dir.is_empty())
    .map(|dir| Path::new(dir).join(name))
    .find(|path| std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
    .and_then(|path| std::fs::canonicalize(path).ok())
}

#[tonic::async_trait]
impl ProcessService for Kept {
  type ExecStream = needy_core_executor::service::ExecStream;

  async fn exec(&self, request: Request<Streaming<ExecRequest>>) -> Result<Response<Self::ExecStream>, Status> {
    let mut inbound = request.into_inner();
    let first = inbound.next().await.transpose()?.ok_or_else(|| Status::invalid_argument("empty request stream"))?;
    let Some(exec_request::Payload::Start(mut start)) = first.payload else {
      return Err(Status::invalid_argument("a kept command starts a process"));
    };
    let mut argv = std::mem::take(&mut start.argv).into_iter();
    let name = argv.next().unwrap_or_default();
    let Some(real) = program(&name, &self.search) else {
      return Err(Status::not_found(format!("{name}: command not found")));
    };
    start.argv = command(&self.fence, &real, self.developer.as_deref(), argv);
    // A kept command ends with the shim that asked for it, and is never
    // looked up elsewhere.
    start.detach = false;
    start.search_path_if_missing = false;
    let first = ExecRequest {
      payload: Some(exec_request::Payload::Start(start)),
    };
    let rest: Inbound = Box::pin(futures_util::stream::once(async move { Ok(first) }).chain(inbound));
    Ok(Response::new(self.processes.open(rest).await?))
  }

  async fn list_processes(&self, _request: Request<ListProcessesRequest>) -> Result<Response<ListProcessesResponse>, Status> {
    Err(Status::unimplemented("the kept-command service only starts commands"))
  }

  async fn kill_process(&self, _request: Request<KillProcessRequest>) -> Result<Response<KillProcessResponse>, Status> {
    Err(Status::unimplemented("the kept-command service only starts commands"))
  }
}
