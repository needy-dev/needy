//! `cargo xtask mock-model`: a scripted OpenAI-compatible model server.
//!
//! It lets a coding agent (opencode, pi) run its whole loop without an
//! account: the model asks for one shell command, the agent runs it, and the
//! model answers with the command's output. End-to-end tests point an agent
//! at it through an actor network service (`examples/agents/mock-model`).
//!
//! The script, for each chat completion request:
//!
//! 1. A `tool` message after the last `user` message: the final answer,
//!    `mock done: ` and the first 200 characters of the latest tool result.
//! 2. No `tools` in the request (opencode's session title request): `mock
//!    title`.
//! 3. Otherwise one call to the shell tool, with `--command` as its command.
//!
//! Wire format: the Chat Completions API, non-streaming and streaming.
// Reference: https://platform.openai.com/docs/api-reference/chat (create,
// the chat completion object, the chat completion chunk object and
// `stream_options.include_usage`), retrieved 2026-11-13; the page has no
// versions.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;

const DEFAULT_COMMAND: &str = "uname -a; ls";

/// The one model the server lists and answers as.
const MODEL: &str = "mock";

/// How much of the tool result the final answer repeats, in characters.
const RESULT_PREFIX_CHARS: usize = 200;

/// The largest request body read. Agents send their system prompt, tool
/// schemas and the whole conversation in each request; this is far above
/// what a scripted loop of one tool call sends.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(clap::Args)]
pub struct Args {
  /// Address to listen on, for example `127.0.0.1:8080` or `0.0.0.0:8080`.
  #[arg(long)]
  listen: SocketAddr,
  /// The command the scripted shell tool call runs.
  #[arg(long, default_value = DEFAULT_COMMAND)]
  command: String,
}

pub fn main(args: Args) -> Result<()> {
  let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
  runtime.block_on(async {
    let listener = TcpListener::bind(args.listen).await.with_context(|| format!("bind {}", args.listen))?;
    eprintln!("mock-model listening on {}", listener.local_addr()?);
    serve(listener, Arc::new(Mock::new(args.command)), termination()).await;
    Ok(())
  })
}

/// Resolves on SIGINT or SIGTERM.
async fn termination() {
  #[cfg(unix)]
  {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
      Ok(mut terminate) => {
        tokio::select! {
          _ = tokio::signal::ctrl_c() => {}
          _ = terminate.recv() => {}
        }
      }
      Err(error) => {
        eprintln!("mock-model: no SIGTERM handler ({error}); stop it with SIGINT");
        // A failed SIGINT handler leaves only a kill to stop the server.
        let _ = tokio::signal::ctrl_c().await;
      }
    }
  }
  #[cfg(not(unix))]
  {
    // A failed ctrl-c handler leaves only a kill to stop the server.
    let _ = tokio::signal::ctrl_c().await;
  }
}

/// The server's state: the scripted command and the counters
/// `GET /mock/stats` reports.
pub(crate) struct Mock {
  command: String,
  /// Chat completion requests.
  requests: AtomicU64,
  /// Answers that were a shell tool call.
  tool_calls: AtomicU64,
  /// Requests that carried a tool result and got the final answer.
  tool_results: AtomicU64,
}

impl Mock {
  pub(crate) fn new(command: String) -> Self {
    Self {
      command,
      requests: AtomicU64::new(0),
      tool_calls: AtomicU64::new(0),
      tool_results: AtomicU64::new(0),
    }
  }

  fn stats(&self) -> Value {
    json!({
      "requests": self.requests.load(Ordering::Relaxed),
      "tool_calls": self.tool_calls.load(Ordering::Relaxed),
      "tool_results": self.tool_results.load(Ordering::Relaxed),
    })
  }
}

/// Serves HTTP/1.1 on `listener` until `shutdown` resolves.
pub(crate) async fn serve(listener: TcpListener, mock: Arc<Mock>, shutdown: impl Future<Output = ()>) {
  tokio::pin!(shutdown);
  loop {
    let (stream, _) = tokio::select! {
      _ = &mut shutdown => return,
      accepted = listener.accept() => match accepted {
        Ok(accepted) => accepted,
        Err(error) => {
          eprintln!("mock-model: accept: {error}");
          continue;
        }
      },
    };
    let mock = mock.clone();
    tokio::spawn(async move {
      let service = service_fn(move |request| {
        let mock = mock.clone();
        async move { Ok::<_, Infallible>(handle(&mock, request).await) }
      });
      if let Err(error) = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await {
        eprintln!("mock-model: connection: {error}");
      }
    });
  }
}

async fn handle(mock: &Mock, request: Request<Incoming>) -> Response<Full<Bytes>> {
  let method = request.method().clone();
  let path = request.uri().path().to_string();
  // Some clients join a base URL without `/v1` to the path; both forms work.
  let route = path.strip_prefix("/v1").unwrap_or(&path);
  match (&method, route) {
    (&Method::GET, "/models") => {
      eprintln!("mock-model: {method} {path} -> model list");
      let created = now();
      json_response(
        StatusCode::OK,
        &json!({
          "object": "list",
          "data": [{ "id": MODEL, "object": "model", "created": created, "owned_by": "needy" }],
        }),
      )
    }
    (&Method::GET, "/mock/stats") => json_response(StatusCode::OK, &mock.stats()),
    (&Method::POST, "/chat/completions") => {
      let body = match Limited::new(request.into_body(), MAX_BODY_BYTES).collect().await {
        Ok(body) => body.to_bytes(),
        Err(error) => {
          eprintln!("mock-model: {method} {path} -> 400 unreadable body: {error}");
          return error_response(StatusCode::BAD_REQUEST, &format!("unreadable body: {error}"));
        }
      };
      let request: Value = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
          eprintln!("mock-model: {method} {path} -> 400 invalid JSON: {error}");
          return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON: {error}"));
        }
      };
      complete(mock, &method, &path, &request)
    }
    _ => {
      eprintln!("mock-model: {method} {path} -> 404");
      error_response(StatusCode::NOT_FOUND, &format!("no route for {method} {path}"))
    }
  }
}

/// What the script answers one chat completion request with.
enum Reply {
  ToolCall {
    name: String,
    arguments: String,
  },
  Text {
    content: String,
    finish_reason: &'static str,
  },
}

fn complete(mock: &Mock, method: &Method, path: &str, request: &Value) -> Response<Full<Bytes>> {
  let number = mock.requests.fetch_add(1, Ordering::Relaxed) + 1;
  let tools = request.get("tools").and_then(Value::as_array).filter(|tools| !tools.is_empty());
  let stream = request.get("stream").and_then(Value::as_bool).unwrap_or(false);
  let (reply, summary) = if let Some(result) = latest_tool_result(request) {
    mock.tool_results.fetch_add(1, Ordering::Relaxed);
    let prefix: String = result.chars().take(RESULT_PREFIX_CHARS).collect();
    let content = format!("mock done: {prefix}");
    (
      Reply::Text {
        content,
        finish_reason: "stop",
      },
      "final answer".to_string(),
    )
  } else if let Some(tools) = tools {
    match shell_tool(tools) {
      Some(tool) => {
        mock.tool_calls.fetch_add(1, Ordering::Relaxed);
        let name = tool_name(tool).to_string();
        let arguments = tool_arguments(tool, &mock.command).to_string();
        let summary = format!("tool call {name} {arguments}");
        (Reply::ToolCall { name, arguments }, summary)
      }
      None => {
        let names: Vec<&str> = tools.iter().map(tool_name).collect();
        let content = format!("mock: no shell tool among {names:?}");
        (
          Reply::Text {
            content,
            finish_reason: "stop",
          },
          "no shell tool".to_string(),
        )
      }
    }
  } else {
    (
      Reply::Text {
        content: "mock title".to_string(),
        finish_reason: "stop",
      },
      "title".to_string(),
    )
  };
  eprintln!("mock-model: {method} {path} tools={} stream={stream} -> {summary}", if tools.is_some() { "yes" } else { "no" });

  let id = format!("chatcmpl-mock-{number}");
  let call_id = format!("call_mock_{number}");
  let model = request.get("model").and_then(Value::as_str).unwrap_or(MODEL);
  if stream {
    let usage = request.pointer("/stream_options/include_usage").and_then(Value::as_bool).unwrap_or(false);
    let body = stream_body(&id, &call_id, model, &reply, usage);
    let mut response = Response::new(Full::new(Bytes::from(body)));
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response.headers_mut().insert("cache-control", HeaderValue::from_static("no-cache"));
    response
  } else {
    json_response(StatusCode::OK, &completion(&id, &call_id, model, &reply))
  }
}

/// The text of the latest `tool` message after the last `user` message, or
/// `None` when the model has not been answered with a tool result since the
/// user spoke.
// NOTICE: the rule looks past the last message on purpose: a client may put
// a non-tool message (an assistant note, a system reminder) after the tool
// results. A `user` message starts a new turn, which gets a new tool call.
fn latest_tool_result(request: &Value) -> Option<String> {
  let messages = request.get("messages").and_then(Value::as_array)?;
  let role = |message: &Value| message.get("role").and_then(Value::as_str).unwrap_or_default().to_string();
  let turn = messages.iter().rposition(|message| role(message) == "user").map_or(0, |index| index + 1);
  let result = messages[turn..].iter().rev().find(|message| role(message) == "tool")?;
  Some(message_text(result.get("content").unwrap_or(&Value::Null)))
}

/// A message's content as text: a string, or the `text` of each part of an
/// array of content parts.
fn message_text(content: &Value) -> String {
  match content {
    Value::String(text) => text.clone(),
    Value::Array(parts) => parts.iter().filter_map(|part| part.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
    _ => String::new(),
  }
}

fn tool_name(tool: &Value) -> &str {
  tool.pointer("/function/name").and_then(Value::as_str).unwrap_or_default()
}

/// The shell tool among the request's tools: the one named `bash` (any
/// case), else the first whose name has `bash`, `shell` or `exec` in it.
fn shell_tool(tools: &[Value]) -> Option<&Value> {
  tools.iter().find(|tool| tool_name(tool).eq_ignore_ascii_case("bash")).or_else(|| {
    tools.iter().find(|tool| {
      let name = tool_name(tool).to_ascii_lowercase();
      ["bash", "shell", "exec"].iter().any(|part| name.contains(part))
    })
  })
}

/// The shell tool call's arguments, from the tool's JSON schema: the
/// command goes in `command` (or `cmd` when only that exists), and every
/// other required string property gets a short text, such as the
/// `description` some agents require. Optional properties are left out.
fn tool_arguments(tool: &Value, command: &str) -> Value {
  let parameters = tool.pointer("/function/parameters");
  let properties = parameters.and_then(|parameters| parameters.get("properties")).and_then(Value::as_object);
  let has = |name: &str| properties.is_some_and(|properties| properties.contains_key(name));
  let command_key = if !has("command") && has("cmd") {
    "cmd"
  } else {
    "command"
  };
  let mut arguments = Map::new();
  arguments.insert(command_key.to_string(), Value::String(command.to_string()));
  let required = parameters.and_then(|parameters| parameters.get("required")).and_then(Value::as_array);
  for name in required.into_iter().flatten().filter_map(Value::as_str) {
    if arguments.contains_key(name) {
      continue;
    }
    let property = properties.and_then(|properties| properties.get(name));
    if property.and_then(|property| property.get("type")).and_then(Value::as_str) == Some("string") {
      arguments.insert(name.to_string(), Value::String("mock tool call".to_string()));
    }
  }
  Value::Object(arguments)
}

/// A non-streaming `chat.completion` object.
fn completion(id: &str, call_id: &str, model: &str, reply: &Reply) -> Value {
  let (message, finish_reason) = match reply {
    Reply::ToolCall { name, arguments } => (
      json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{ "id": call_id, "type": "function", "function": { "name": name, "arguments": arguments } }],
      }),
      "tool_calls",
    ),
    Reply::Text {
      content,
      finish_reason,
    } => (json!({ "role": "assistant", "content": content }), *finish_reason),
  };
  json!({
    "id": id,
    "object": "chat.completion",
    "created": now(),
    "model": model,
    "choices": [{ "index": 0, "message": message, "logprobs": null, "finish_reason": finish_reason }],
    "usage": usage(),
  })
}

/// The server-sent events of a streamed answer: `chat.completion.chunk`
/// objects, each as `data: {...}`, then `data: [DONE]`. The whole answer is
/// known up front, so the body is sent at once.
fn stream_body(id: &str, call_id: &str, model: &str, reply: &Reply, include_usage: bool) -> String {
  let created = now();
  let chunk = |delta: Value, finish_reason: Value| {
    let mut chunk = json!({
      "id": id,
      "object": "chat.completion.chunk",
      "created": created,
      "model": model,
      "choices": [{ "index": 0, "delta": delta, "logprobs": null, "finish_reason": finish_reason }],
    });
    // With include_usage, every chunk but the last carries `usage: null`.
    if include_usage {
      chunk["usage"] = Value::Null;
    }
    chunk
  };
  let mut chunks = vec![chunk(
    json!({ "role": "assistant", "content": "" }),
    Value::Null,
  )];
  match reply {
    Reply::ToolCall { name, arguments } => {
      // The first delta names the call; the arguments follow in two
      // fragments, as a real model streams them, so a client that does not
      // join fragments fails here and not only against a real model.
      chunks.push(chunk(
        json!({ "tool_calls": [{ "index": 0, "id": call_id, "type": "function", "function": { "name": name, "arguments": "" } }] }),
        Value::Null,
      ));
      let middle = arguments.char_indices().nth(arguments.chars().count() / 2).map_or(arguments.len(), |(index, _)| index);
      for fragment in [&arguments[..middle], &arguments[middle..]] {
        chunks.push(chunk(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": fragment } }] }), Value::Null));
      }
      chunks.push(chunk(json!({}), json!("tool_calls")));
    }
    Reply::Text {
      content,
      finish_reason,
    } => {
      chunks.push(chunk(json!({ "content": content }), Value::Null));
      chunks.push(chunk(json!({}), json!(finish_reason)));
    }
  }
  if include_usage {
    chunks.push(json!({
      "id": id,
      "object": "chat.completion.chunk",
      "created": created,
      "model": model,
      "choices": [],
      "usage": usage(),
    }));
  }
  let mut body = String::new();
  for chunk in chunks {
    body.push_str(&format!("data: {chunk}\n\n"));
  }
  body.push_str("data: [DONE]\n\n");
  body
}

/// Token counts: the mock counts no tokens.
fn usage() -> Value {
  json!({ "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 })
}

fn now() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

fn json_response(status: StatusCode, body: &Value) -> Response<Full<Bytes>> {
  let mut response = Response::new(Full::new(Bytes::from(body.to_string())));
  *response.status_mut() = status;
  response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
  response
}

/// An error in the API's error object shape.
fn error_response(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
  json_response(status, &json!({ "error": { "message": message, "type": "invalid_request_error" } }))
}

#[cfg(test)]
#[path = "mock_model_test.rs"]
mod tests;
