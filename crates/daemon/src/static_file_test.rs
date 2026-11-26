use super::check_plain_secret;

#[test]
fn a_plain_secret_needs_32_bytes_of_hex_or_base64() {
  let hex = "0123456789abcdef".repeat(4);
  let base64url = "Qm9vdHN0cmFwcGVkLXJhbmRvbS1zZWNyZXQtMDEyMzQ"; // 43 characters
  let base64 = "Qm9vdHN0cmFwcGVkK3JhbmRvbS9zZWNyZXQtMDEyMzQ=";

  assert!(check_plain_secret(&hex).is_ok());
  assert!(check_plain_secret(base64url).is_ok());
  assert!(check_plain_secret(base64).is_ok());
  assert!(check_plain_secret(&hex[..63]).is_err(), "63 hex digits");
  assert!(check_plain_secret(&base64url[..42]).is_err(), "42 base64 characters");
  assert!(check_plain_secret("correct horse battery staple correct horse battery").is_err(), "not base64");
  assert!(check_plain_secret(&format!("{base64url}===")).is_err(), "three padding characters");
}
