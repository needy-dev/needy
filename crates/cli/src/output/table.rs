//! Borderless tables with `comfy-table`.

use comfy_table::{ContentArrangement, Table, presets};

/// Renders a header and rows, one line each, without trailing spaces.
pub fn render(header: &[&str], rows: impl Iterator<Item = Vec<String>>) -> String {
  let mut table = Table::new();
  table.load_preset(presets::NOTHING).set_content_arrangement(ContentArrangement::Disabled).set_header(header.iter().copied());
  for row in rows {
    table.add_row(row);
  }
  // Every cell carries one space of padding on each side; drop the margin
  // so columns start at the line's first character.
  table.column_iter_mut().for_each(|column| {
    column.set_padding((0, 2));
  });
  let mut text = String::new();
  for line in table.lines() {
    text.push_str(line.trim_end());
    text.push('\n');
  }
  text
}
