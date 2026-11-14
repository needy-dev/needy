use super::*;

#[test]
fn a_fault_lists_on_one_line_in_flow_style() {
  let value: Value = serde_yaml::from_str("net:\n  request: echo.needy\nstatus: 503\n").unwrap();
  assert_eq!(flow(&value), "{net: {request: echo.needy}, status: 503}");
}
