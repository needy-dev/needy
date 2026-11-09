use super::*;

fn argv(s: &str) -> Vec<String> {
  s.split_whitespace().map(str::to_string).collect()
}

#[test]
fn word_only_parser() {
  assert_eq!(
    word_only("a b && c 'd e' | f; g\nh").unwrap(),
    vec![
      argv("a b"),
      vec!["c".into(), "d e".into()],
      argv("f"),
      argv("g"),
      argv("h")
    ]
  );
  assert!(word_only("a > b").is_none());
  assert!(word_only("a $(b)").is_none());
  assert!(word_only("a &").is_none());
  assert!(word_only("a && && b").is_none());
  assert!(word_only("(a)").is_none());
  assert!(word_only("\"$x\"").is_none());
  assert!(word_only("a;").is_some());
  assert!(word_only("").is_none());
}
