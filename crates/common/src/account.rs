//! The account files of one user: `/etc/passwd`, `/etc/group` and
//! `/etc/shadow` rewritten so that a user, by name and by ID, is exactly
//! the one given. Pure text; callers read and write the files.
//!
//! Each file is one entry per line with `:`-separated fields:
//!
//! - passwd: `name:password:UID:GID:GECOS:directory:shell`
//! - group: `group_name:password:GID:user_list`
//! - shadow: `name:password:lastchange:min:max:warn:inactive:expire:reserved`
//!
// Source: https://man7.org/linux/man-pages/man5/passwd.5.html (man-pages 6.9.1,
// 2024-06-15, read 2026-11-12), https://man7.org/linux/man-pages/man5/group.5.html
// (man-pages 6.9.1) and https://man7.org/linux/man-pages/man5/shadow.5.html
// (shadow-utils 4.16.0, read 2026-11-12).
// Reference: an `x` password field in passwd and group means the password is in
// shadow; a password field starting with `!` in shadow locks the password.

/// How one account file is rewritten for a user.
pub type Rewrite = fn(&str, &Account<'_>) -> String;

/// The account files under `/etc`, each with how it is rewritten.
pub const FILES: [(&str, Rewrite); 3] = [("passwd", passwd), ("group", group), ("shadow", shadow)];

/// A user as the account files name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Account<'a> {
  pub name: &'a str,
  pub uid: u32,
  pub gid: u32,
}

impl Account<'_> {
  /// `/home/<name>`.
  pub fn home(&self) -> String {
    format!("/home/{}", self.name)
  }

  fn passwd_entry(&self) -> String {
    format!("{}:x:{}:{}::{}:/bin/bash", self.name, self.uid, self.gid, self.home())
  }

  fn group_entry(&self) -> String {
    format!("{}:x:{}:", self.name, self.gid)
  }

  fn shadow_entry(&self) -> String {
    // Locked password, no aging: the user never logs in with a password.
    format!("{}:!:::::::", self.name)
  }
}

/// `text` with the user's passwd entry: every entry with the user's name or
/// UID is replaced by one entry for the user. Returned unchanged when it
/// already has exactly that entry and no other with the name or UID.
pub fn passwd(text: &str, user: &Account<'_>) -> String {
  let uid = user.uid.to_string();
  rewrite(text, &user.passwd_entry(), |fields| fields[0] == user.name || fields.get(2) == Some(&uid.as_str()))
}

/// `text` with the user's own group: every group with the user's name or
/// GID is replaced by one group for the user.
pub fn group(text: &str, user: &Account<'_>) -> String {
  let gid = user.gid.to_string();
  rewrite(text, &user.group_entry(), |fields| fields[0] == user.name || fields.get(2) == Some(&gid.as_str()))
}

/// `text` with a locked shadow entry for the user in place of any with its
/// name. An existing entry for the name is kept as it is, since shadow has
/// no ID to disagree with.
pub fn shadow(text: &str, user: &Account<'_>) -> String {
  if text.lines().any(|line| line.split(':').next() == Some(user.name)) {
    return text.to_string();
  }
  rewrite(text, &user.shadow_entry(), |fields| fields[0] == user.name)
}

/// Drops the lines `collides` matches and appends `entry`; `text` as it is
/// when `entry` is its only colliding line.
fn rewrite(text: &str, entry: &str, collides: impl Fn(&[&str]) -> bool) -> String {
  let colliding: Vec<&str> = text
    .lines()
    .filter(|line| {
      let fields: Vec<&str> = line.split(':').collect();
      !line.trim().is_empty() && !line.starts_with('#') && collides(&fields)
    })
    .collect();
  if colliding == [entry] {
    return text.to_string();
  }
  let mut out: String = text.lines().filter(|line| !colliding.contains(line)).flat_map(|line| [line, "\n"]).collect();
  out.push_str(entry);
  out.push('\n');
  out
}

#[cfg(test)]
#[path = "account_test.rs"]
mod tests;
