//! Helpers for init's tests, which run as a user that is not root.

/// A group of this process other than its own, which a user that is not
/// root may still give its files to; `None` when it has no other group. A
/// test that gives files to it tells the files init gave away apart from
/// those it did not.
pub(super) fn other_group() -> Option<u32> {
  // SAFETY: a zero count asks only for the number of groups.
  let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
  let mut groups = vec![0; usize::try_from(count).ok()?];
  // SAFETY: `groups` has room for `count` entries.
  let count = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
  groups.truncate(usize::try_from(count).ok()?);
  // SAFETY: `getegid` cannot fail.
  let own = unsafe { libc::getegid() };
  groups.into_iter().find(|&g| g != own)
}
