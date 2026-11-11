use super::*;

#[test]
fn a_route_reads_back_what_init_wrote() {
  let route = Route {
    names: vec!["python3".into(), "curl".into()],
  };
  assert_eq!(Route::parse(&route.render()), route);
}

#[test]
fn a_route_matches_a_listed_name_or_everything() {
  let route = Route::parse("0\npython3,curl\n");
  assert!(route.routes(&["python3".into()]));
  assert!(!route.routes(&["ls".into()]));
  assert!(Route::parse("0\n*\n").routes(&["ls".into()]));
  assert!(!Route::parse("0\n\n").routes(&["ls".into()]));
}
