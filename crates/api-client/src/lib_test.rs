use super::*;

fn dummy_client() -> Client {
  Client::from_channel(Endpoint::from_static("http://127.0.0.1:1").connect_lazy())
}

#[tokio::test]
async fn with_headers_and_with_token_attach_both_regardless_of_order() {
  let headers = [
    ("CF-Access-Client-Id", "id-123.access"),
    ("CF-Access-Client-Secret", "secret-456"),
  ];

  for client in [
    dummy_client().with_headers(headers).unwrap().with_token("tok_abc").unwrap(),
    dummy_client().with_token("tok_abc").unwrap().with_headers(headers).unwrap(),
  ] {
    let mut interceptor = client.auth.clone();
    let request = interceptor.call(Request::new(())).unwrap();
    let md = request.metadata();
    assert_eq!(md.get("cf-access-client-id").unwrap(), "id-123.access");
    assert_eq!(md.get("cf-access-client-secret").unwrap(), "secret-456");
    assert_eq!(md.get("authorization").unwrap(), "Bearer tok_abc");
  }
}

#[tokio::test]
async fn with_headers_rejects_reserved_duplicate_and_invalid_entries() {
  let err = dummy_client().with_headers([("Authorization", "Bearer nope")]).unwrap_err();
  assert_eq!(err.kind(), Kind::Invalid);
  assert!(!err.to_string().contains("nope"));

  let err = dummy_client().with_headers([("X-Key", "a"), ("x-key", "b")]).unwrap_err();
  assert_eq!(err.kind(), Kind::Invalid);

  let err = dummy_client().with_headers([("x-data-bin", "abc")]).unwrap_err();
  assert_eq!(err.kind(), Kind::Invalid);

  let err = dummy_client().with_headers([("X-Empty", "")]).unwrap_err();
  assert_eq!(err.kind(), Kind::Invalid);

  let err = dummy_client().with_headers([("X-Bad", "secret\r\nInjected: 1")]).unwrap_err();
  assert_eq!(err.kind(), Kind::Invalid);
  assert!(!err.to_string().contains("secret"));
}

#[tokio::test]
async fn bearer_auth_debug_redacts_token_and_header_values() {
  let client =
    dummy_client().with_headers([("CF-Access-Client-Secret", "super-secret-value")]).unwrap().with_token("super-secret-token").unwrap();
  let debug = format!("{:?}", client.auth);
  assert!(!debug.contains("super-secret-value"), "header value leaked in Debug: {debug}");
  assert!(!debug.contains("super-secret-token"), "token leaked in Debug: {debug}");
  assert!(debug.contains("cf-access-client-secret"), "header name should stay visible: {debug}");
}
