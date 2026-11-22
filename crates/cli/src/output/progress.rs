//! Temporary terminal feedback while a command waits for an operation,
//! including spinners, multi-layer container image pulls, and archive
//! downloads.

use std::collections::HashMap;
use std::io::IsTerminal as _;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use needy_core_executor::engine::models::CreateImageInfo;

fn draw_target(enabled: bool) -> ProgressDrawTarget {
  if enabled && super::stderr_animated() {
    ProgressDrawTarget::stderr()
  } else {
    ProgressDrawTarget::hidden()
  }
}

/// Whether live animated progress replaces the permanent phase log.
pub(crate) fn visible(enabled: bool) -> bool {
  !draw_target(enabled).is_hidden()
}

/// Keeps phase messages in the plain log when no live display replaces them.
pub(crate) fn phase(enabled: bool, message: std::fmt::Arguments<'_>) {
  if !visible(enabled) {
    super::message(message);
  }
}

/// Prints the terminal result after its live display has been cleared.
/// Redirecting stdout alone keeps the existing stderr-only contract.
pub(crate) fn complete(enabled: bool, message: std::fmt::Arguments<'_>, legacy: std::fmt::Arguments<'_>) -> std::io::Result<()> {
  if visible(enabled) && std::io::stdout().is_terminal() {
    super::success(&message.to_string())
  } else {
    super::message(legacy);
    Ok(())
  }
}

fn style(template: &str) -> ProgressStyle {
  ProgressStyle::with_template(template).expect("progress templates are valid")
}

/// A scoped spinner on stderr. Finish it before printing an outcome or
/// handing the terminal to an interactive session; errors clear it on drop.
///
/// Redirected stderr, dumb terminals, `--quiet`, and nonempty `NO_COLOR` remain
/// silent.
#[must_use = "keep the progress guard alive until the operation finishes"]
pub struct Progress {
  bar: ProgressBar,
}

impl Progress {
  /// Starts progress when enabled and stderr supports terminal rendering.
  /// Callers can disable it for machine-readable diagnostic modes.
  pub fn new(enabled: bool, message: impl Into<String>) -> Self {
    Self::with_draw_target(draw_target(enabled), message)
  }

  fn with_draw_target(target: ProgressDrawTarget, message: impl Into<String>) -> Self {
    // TODO: Delay the first draw when indicatif supports it without another
    // worker; this slice keeps all animation lifetime owned by its ticker.
    let bar = ProgressBar::with_draw_target(None, target).with_style(ProgressStyle::default_spinner()).with_message(message.into());
    if !bar.is_hidden() {
      bar.tick();
      // NOTICE: indicatif owns and joins its ticker; no task outlives this guard.
      // Source: https://github.com/console-rs/indicatif/blob/0.18.6/src/progress_bar.rs#L692-L700
      // Reference: https://docs.rs/indicatif/0.18.6/indicatif/struct.ProgressBar.html#method.enable_steady_tick
      bar.enable_steady_tick(Duration::from_millis(80));
    }
    Self { bar }
  }

  /// Replaces the description while the operation continues.
  pub fn message(&self, message: impl Into<String>) {
    self.bar.set_message(message.into());
  }

  /// Whether terminal feedback replaces the caller's plain progress line.
  pub fn is_visible(&self) -> bool {
    !self.bar.is_hidden()
  }

  /// Stops animation and clears the spinner before subsequent output.
  pub fn finish(self) {
    drop(self);
  }
}

impl Drop for Progress {
  fn drop(&mut self) {
    // Join before clearing so no later tick can overwrite the command's output.
    self.bar.disable_steady_tick();
    self.bar.finish_and_clear();
  }
}

/// Stable rows for the engine's layer IDs. One ticker animates the pull
/// header; layer rows redraw only when the engine reports real progress.
pub(crate) struct Pull {
  terminal: Option<Layers>,
}

impl Pull {
  pub(crate) fn new(enabled: bool, image: &str) -> Self {
    let target = draw_target(enabled);
    let terminal = (!target.is_hidden()).then(|| Layers::new(target, image));
    Self { terminal }
  }

  pub(crate) fn update(&mut self, update: CreateImageInfo) {
    if let Some(terminal) = &mut self.terminal {
      terminal.update(update);
    } else if let Some(line) = plain(update) {
      super::message(format_args!("image pull: {line}"));
    }
  }
}

struct Layers {
  display: MultiProgress,
  header: ProgressBar,
  rows: HashMap<String, Layer>,
}

struct Layer {
  bar: ProgressBar,
  phase: String,
}

impl Layers {
  fn new(target: ProgressDrawTarget, image: &str) -> Self {
    let display = MultiProgress::with_draw_target(target);
    let header = display.add(ProgressBar::hidden().with_style(ProgressStyle::default_spinner()).with_message(format!("Pulling {image}")));
    header.tick();
    header.enable_steady_tick(Duration::from_millis(80));
    Self {
      display,
      header,
      rows: HashMap::new(),
    }
  }

  fn update(&mut self, update: CreateImageInfo) {
    let Some(id) = update.id else {
      if let Some(status) = update.status {
        self.header.set_message(status);
      }
      return;
    };
    let row = self.rows.entry(id.clone()).or_insert_with(|| Layer {
      bar: self.display.add(ProgressBar::hidden().with_prefix(id).with_style(style("{prefix}: {msg}"))),
      phase: String::new(),
    });
    if let Some(status) = update.status
      && status != row.phase
    {
      row.phase = status;
      // A new phase has its own byte counts; never carry a download's total
      // into extraction or show a completed/cached layer as still downloading.
      row.bar.set_style(style("{prefix}: {msg}"));
      row.bar.unset_length();
      row.bar.set_position(0);
    }
    row.bar.set_message(row.phase.clone());
    if let Some(detail) = update.progress_detail
      && let Some(current) = detail.current.and_then(|value| u64::try_from(value).ok())
    {
      if let Some(total) = detail.total.and_then(|value| u64::try_from(value).ok()).filter(|total| *total > 0) {
        row.bar.set_length(total);
        row.bar.set_style(style("{prefix}: {msg} [{bar:20}] {bytes}/{total_bytes}"));
      } else {
        row.bar.unset_length();
        row.bar.set_style(style("{prefix}: {msg} {bytes}"));
      }
      row.bar.set_position(current);
    }
    row.bar.tick();
  }
}

impl Drop for Layers {
  fn drop(&mut self) {
    self.header.disable_steady_tick();
    self.header.finish_and_clear();
    for row in self.rows.values() {
      row.bar.finish_and_clear();
    }
    if let Err(error) = self.display.clear() {
      // Terminal presentation must not replace an image-pull error.
      tracing::debug!(%error, "cannot clear image progress");
    }
  }
}

/// A VM archive transfer, with a byte total only when HTTP supplies one.
pub(crate) struct Download {
  bar: ProgressBar,
}

impl Download {
  pub(crate) fn new(enabled: bool, message: String) -> Self {
    Self::with_target(draw_target(enabled), message)
  }

  fn with_target(target: ProgressDrawTarget, message: String) -> Self {
    let bar = ProgressBar::with_draw_target(None, target).with_style(style("{spinner} {msg} {bytes}")).with_message(message);
    if !bar.is_hidden() {
      bar.tick();
      bar.enable_steady_tick(Duration::from_millis(80));
    }
    Self { bar }
  }

  pub(crate) fn visible(&self) -> bool {
    !self.bar.is_hidden()
  }

  pub(crate) fn total(&self, total: Option<u64>) {
    if let Some(total) = total.filter(|total| *total > 0) {
      self.bar.set_length(total);
      self.bar.set_style(style("{spinner} {msg} [{bar:20}] {bytes}/{total_bytes}"));
    }
  }

  pub(crate) fn received(&self, received: u64) {
    self.bar.set_position(received);
  }
}

impl Drop for Download {
  fn drop(&mut self) {
    self.bar.disable_steady_tick();
    self.bar.finish_and_clear();
  }
}

/// Keep the engine stream's existing redirected representation unchanged.
fn plain(update: CreateImageInfo) -> Option<String> {
  let mut parts = Vec::new();
  if let Some(id) = update.id {
    parts.push(format!("{id}:"));
  }
  if let Some(status) = update.status {
    parts.push(status);
  }
  if let Some(detail) = update.progress_detail
    && let Some(current) = detail.current
  {
    parts.push(match detail.total {
      Some(total) if total > 0 => format!("{current}/{total} bytes"),
      _ => format!("{current} bytes"),
    });
  }
  (!parts.is_empty()).then(|| parts.join(" "))
}

#[cfg(test)]
mod tests {
  use std::time::Instant;

  use indicatif::InMemoryTerm;
  use needy_core_executor::engine::models::ProgressDetail;

  use super::*;

  fn progress(term: &InMemoryTerm) -> Progress {
    Progress::with_draw_target(ProgressDrawTarget::term_like(Box::new(term.clone())), "Connecting")
  }

  fn wait_for(term: &InMemoryTerm, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
      let contents = term.contents();
      if contents.contains(expected) {
        return;
      }
      assert!(Instant::now() < deadline, "expected {expected:?} in {contents:?}");
      std::thread::sleep(Duration::from_millis(10));
    }
  }

  // Synthetic engine events, not captured registry data.
  // Reference: Docker Engine API v1.51, ImageCreate:
  // https://docs.docker.com/reference/api/engine/version/v1.51/#tag/Image/operation/ImageCreate
  fn update(id: &str, status: &str, bytes: Option<(i64, i64)>) -> CreateImageInfo {
    CreateImageInfo {
      id: Some(id.into()),
      status: Some(status.into()),
      progress_detail: bytes.map(|(current, total)| ProgressDetail {
        current: Some(current),
        total: Some(total),
      }),
      ..Default::default()
    }
  }

  #[test]
  fn renders_updates_and_clears_before_the_next_output() {
    let term = InMemoryTerm::new(4, 80);
    let progress = progress(&term);
    assert!(term.contents().contains("Connecting"));

    progress.message("Creating run");
    assert!(term.contents().contains("Creating run"));
    assert!(!term.contents().contains("Connecting"));

    progress.finish();
    assert_eq!(term.contents(), "");
  }

  #[test]
  fn error_return_clears_the_spinner() {
    fn operation(term: &InMemoryTerm) -> Result<(), &'static str> {
      let _progress = progress(term);
      assert!(term.contents().contains("Connecting"));
      Err("operation failed")
    }

    let term = InMemoryTerm::new(4, 80);
    assert_eq!(operation(&term), Err("operation failed"));
    assert_eq!(term.contents(), "");
  }

  #[test]
  fn a_waiting_operation_animates_and_stays_cleared_after_drop() {
    let term = InMemoryTerm::new(4, 80);
    let progress = progress(&term);
    let first_frame = term.contents();
    let deadline = Instant::now() + Duration::from_secs(2);
    while term.contents() == first_frame && Instant::now() < deadline {
      std::thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(term.contents(), first_frame, "spinner did not advance while waiting");

    drop(progress);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(term.contents(), "");
  }

  #[test]
  fn layer_rows_update_in_place_keep_cached_rows_and_clear_on_drop() {
    let term = InMemoryTerm::new(10, 120);
    let mut layers = Layers::new(ProgressDrawTarget::term_like(Box::new(term.clone())), "example.com/image");
    layers.update(update("cached", "Already exists", None));
    layers.update(update("active", "Downloading", Some((1024, 4096))));
    assert!(term.contents().contains("cached: Already exists"), "{}", term.contents());
    assert!(term.contents().contains("active: Downloading ["), "{}", term.contents());
    assert!(term.contents().contains("1.00 KiB/4.00 KiB"), "{}", term.contents());

    layers.update(update("active", "Extracting", Some((512, 2048))));
    let contents = term.contents();
    assert_eq!(contents.matches("active:").count(), 1, "{contents}");
    assert!(!contents.contains("Downloading"), "{contents}");
    assert!(contents.contains("512 B/2.00 KiB"), "{contents}");

    layers.update(update("active", "Pull complete", None));
    assert!(term.contents().contains("active: Pull complete"));
    assert!(!term.contents().contains("2.00 KiB"));
    assert!(term.contents().contains("cached: Already exists"));
    drop(layers);
    assert_eq!(term.contents(), "");
  }

  #[test]
  fn downloads_show_only_reported_totals_and_clear_on_drop() {
    let term = InMemoryTerm::new(4, 120);
    let download = Download::with_target(ProgressDrawTarget::term_like(Box::new(term.clone())), "Downloading archive".into());
    download.received(1024);
    wait_for(&term, "1.00 KiB");
    assert!(!term.contents().contains('/'));
    download.total(Some(4096));
    download.received(2048);
    wait_for(&term, "2.00 KiB/4.00 KiB");
    drop(download);
    assert_eq!(term.contents(), "");
  }

  #[test]
  fn plain_engine_progress_keeps_the_existing_text_contract() {
    assert_eq!(plain(update("layer", "Downloading", Some((1024, 4096)))).as_deref(), Some("layer: Downloading 1024/4096 bytes"));
    assert_eq!(plain(update("cached", "Already exists", None)).as_deref(), Some("cached: Already exists"));
    assert_eq!(plain(update("layer", "Downloading", Some((1024, 0)))).as_deref(), Some("layer: Downloading 1024 bytes"));
    assert_eq!(plain(CreateImageInfo::default()), None);
  }
}
