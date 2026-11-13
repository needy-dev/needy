use std::net::SocketAddr;
use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};

use super::{Mock, serve};

/// Starts the server on a free local port with `command` as the scripted
/// command. It stops when the test's runtime ends.
async fn start(command: &str) -> SocketAddr {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  tokio::spawn(serve(listener, Arc::new(Mock::new(command.to_string())), std::future::pending()));
  address
}

/// Sends one request and returns the status and the body as text.
async fn send(address: SocketAddr, method: Method, path: &str, body: Option<&Value>) -> (u16, String) {
  let stream = TcpStream::connect(address).await.unwrap();
  let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.unwrap();
  tokio::spawn(connection);
  let body = body.map(Value::to_string).unwrap_or_default();
  let request = Request::builder()
    .method(method)
    .uri(path)
    .header("host", address.to_string())
    .header("authorization", "Bearer any-key")
    .header("content-type", "application/json")
    .body(Full::new(Bytes::from(body)))
    .unwrap();
  let response = sender.send_request(request).await.unwrap();
  let status = response.status().as_u16();
  let bytes = response.into_body().collect().await.unwrap().to_bytes();
  (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn post(address: SocketAddr, body: &Value) -> Value {
  let (status, text) = send(address, Method::POST, "/v1/chat/completions", Some(body)).await;
  assert_eq!(status, 200, "{text}");
  serde_json::from_str(&text).unwrap()
}

/// The parsed `data:` payloads of a streamed answer, and whether it ended
/// with `data: [DONE]`.
fn events(text: &str) -> (Vec<Value>, bool) {
  let data: Vec<&str> = text.split("\n\n").filter(|event| !event.is_empty()).map(|event| event.strip_prefix("data: ").unwrap()).collect();
  let done = data.last() == Some(&"[DONE]");
  let chunks = data.iter().filter(|data| **data != "[DONE]").map(|data| serde_json::from_str(data).unwrap()).collect();
  (chunks, done)
}

/// pi's bash tool: `command` required, `timeout` optional.
fn bash_tool() -> Value {
  json!({
    "type": "function",
    "function": {
      "name": "bash",
      "description": "Execute a bash command",
      "parameters": {
        "type": "object",
        "properties": {
          "command": { "type": "string" },
          "timeout": { "type": "number" },
        },
        "required": ["command"],
      },
    },
  })
}

fn read_tool() -> Value {
  json!({
    "type": "function",
    "function": {
      "name": "read",
      "parameters": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] },
    },
  })
}

fn user_only(stream: bool) -> Value {
  json!({
    "model": "mock",
    "stream": stream,
    "messages": [
      { "role": "system", "content": "You are a coding agent." },
      { "role": "user", "content": "List the files." },
    ],
    "tools": [read_tool(), bash_tool()],
  })
}

#[tokio::test]
async fn mock_model_answers_a_user_message_with_one_bash_tool_call() {
  let address = start("uname -a; ls").await;
  let answer = post(address, &user_only(false)).await;

  let choice = &answer["choices"][0];
  assert_eq!(choice["finish_reason"], "tool_calls");
  let calls = choice["message"]["tool_calls"].as_array().unwrap();
  assert_eq!(calls.len(), 1);
  assert_eq!(calls[0]["type"], "function");
  assert_eq!(calls[0]["function"]["name"], "bash");
  let arguments: Value = serde_json::from_str(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap();
  assert_eq!(arguments, json!({ "command": "uname -a; ls" }));
}

#[tokio::test]
async fn mock_model_streams_the_tool_call_in_chunks_that_join_to_the_same_call() {
  let address = start("uname -a; ls").await;
  let mut request = user_only(true);
  request["stream_options"] = json!({ "include_usage": true });
  let (status, text) = send(address, Method::POST, "/v1/chat/completions", Some(&request)).await;
  assert_eq!(status, 200);

  let (chunks, done) = events(&text);
  assert!(done, "the stream must end with data: [DONE]: {text}");
  assert!(chunks.iter().all(|chunk| chunk["object"] == "chat.completion.chunk"));
  let (mut id, mut name, mut arguments) = (String::new(), String::new(), String::new());
  for chunk in &chunks {
    for call in chunk["choices"][0]["delta"]["tool_calls"].as_array().into_iter().flatten() {
      assert_eq!(call["index"], 0);
      id.push_str(call["id"].as_str().unwrap_or_default());
      name.push_str(call["function"]["name"].as_str().unwrap_or_default());
      arguments.push_str(call["function"]["arguments"].as_str().unwrap_or_default());
    }
  }
  assert!(id.starts_with("call_"), "{id}");
  assert_eq!(name, "bash");
  let arguments: Value = serde_json::from_str(&arguments).unwrap();
  assert_eq!(arguments, json!({ "command": "uname -a; ls" }));
  let finishes: Vec<&Value> =
    chunks.iter().filter_map(|chunk| chunk["choices"][0].get("finish_reason")).filter(|reason| !reason.is_null()).collect();
  assert_eq!(finishes, [&json!("tool_calls")]);
  // include_usage: a last chunk with no choices carries the usage.
  let last = chunks.last().unwrap();
  assert_eq!(last["choices"], json!([]));
  assert!(last["usage"]["total_tokens"].is_number());
}

#[tokio::test]
async fn mock_model_answers_a_tool_result_with_the_final_text() {
  let address = start("uname -a; ls").await;
  let output = format!("Linux actor 6.1.0 x86_64\n{}", "x".repeat(300));
  let request = json!({
    "model": "mock",
    "messages": [
      { "role": "user", "content": "List the files." },
      { "role": "assistant", "content": null, "tool_calls": [
        { "id": "call_1", "type": "function", "function": { "name": "bash", "arguments": "{\"command\":\"uname -a; ls\"}" } },
      ] },
      { "role": "tool", "tool_call_id": "call_1", "content": output },
    ],
    "tools": [bash_tool()],
  });
  let answer = post(address, &request).await;

  let choice = &answer["choices"][0];
  assert_eq!(choice["finish_reason"], "stop");
  assert!(choice["message"].get("tool_calls").is_none());
  let expected: String = output.chars().take(200).collect();
  assert_eq!(choice["message"]["content"], format!("mock done: {expected}"));
}

#[tokio::test]
async fn mock_model_fills_a_required_description_of_an_opencode_style_tool() {
  let address = start("ls").await;
  let request = json!({
    "model": "mock",
    "messages": [{ "role": "user", "content": "List the files." }],
    "tools": [{
      "type": "function",
      "function": {
        "name": "bash",
        "parameters": {
          "type": "object",
          "properties": {
            "command": { "type": "string" },
            "description": { "type": "string" },
            "timeout": { "type": "number" },
            "workdir": { "type": "string" },
          },
          "required": ["command", "description"],
        },
      },
    }],
  });
  let answer = post(address, &request).await;

  let arguments = answer["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"].as_str().unwrap();
  let arguments: Value = serde_json::from_str(arguments).unwrap();
  // The optional `workdir` string and `timeout` stay out.
  assert_eq!(arguments, json!({ "command": "ls", "description": "mock tool call" }));
}

#[tokio::test]
async fn mock_model_counts_requests_tool_calls_and_tool_results_in_stats() {
  let address = start("ls").await;
  post(address, &user_only(false)).await;
  let result = json!({
    "model": "mock",
    "messages": [
      { "role": "user", "content": "List the files." },
      { "role": "tool", "tool_call_id": "call_1", "content": "a\nb" },
    ],
    "tools": [bash_tool()],
  });
  post(address, &result).await;
  // A request without tools, such as opencode's session title, is counted
  // but is neither a call nor a result.
  let title = post(address, &json!({ "model": "mock", "messages": [{ "role": "user", "content": "Title?" }] })).await;
  assert_eq!(title["choices"][0]["message"]["content"], "mock title");

  let (status, text) = send(address, Method::GET, "/mock/stats", None).await;
  assert_eq!(status, 200);
  let stats: Value = serde_json::from_str(&text).unwrap();
  assert_eq!(stats, json!({ "requests": 3, "tool_calls": 1, "tool_results": 1 }));
}

#[tokio::test]
async fn mock_model_lists_one_model_with_and_without_the_v1_prefix() {
  let address = start("ls").await;
  for path in ["/v1/models", "/models"] {
    let (status, text) = send(address, Method::GET, path, None).await;
    assert_eq!(status, 200, "{path}");
    let list: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(list["data"][0]["id"], "mock", "{path}");
    assert_eq!(list["data"].as_array().unwrap().len(), 1, "{path}");
  }
}
