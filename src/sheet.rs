//! Spreadsheets for the file tab: xlsx, xlsm, xlsb, xls and ods through calamine,
//! csv and tsv parsed here. Cells keep their place in the sheet, so row 676 in
//! Excel is row 676 here.

use std::path::Path;

use calamine::{Data, Reader};
use gpui::SharedString;

/// More than this is cut, with a note: the grid stays responsive.
const MAX_ROWS: usize = 200_000;
const MAX_COLS: usize = 512;
const MAX_CSV_BYTES: u64 = 64 << 20;

pub struct Workbook {
    pub sheets: Vec<Sheet>,
}

pub struct Sheet {
    pub name: SharedString,
    pub rows: Vec<Vec<Cell>>,
    /// Pixel width per column, from its longest value; one per column drawn.
    pub widths: Vec<f32>,
    /// Rows or columns past the limits were left out.
    pub truncated: bool,
}

#[derive(Clone, Default)]
pub struct Cell {
    pub text: SharedString,
    pub kind: CellKind,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum CellKind {
    #[default]
    Text,
    /// Numbers and dates, aligned right.
    Number,
    /// A formula saved without its result (files written by scripts): shown as `=…`.
    Formula,
    Error,
}

pub fn read(path: &Path) -> Result<Workbook, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "csv" | "tsv" => {
            let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
            if size > MAX_CSV_BYTES {
                return Err(format!(
                    "File is too large to preview ({:.1} MB)",
                    size as f64 / 1e6
                ));
            }
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            let text = String::from_utf8_lossy(&bytes);
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            let sep = if ext == "tsv" {
                '\t'
            } else {
                guess_separator(text)
            };
            let rows = parse_delimited(text, sep)
                .into_iter()
                .map(|row| row.into_iter().map(text_cell).collect())
                .collect();
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Sheet");
            Ok(Workbook {
                sheets: vec![sheet(name, rows)],
            })
        }
        _ => read_workbook(path),
    }
}

fn read_workbook(path: &Path) -> Result<Workbook, String> {
    let mut book = calamine::open_workbook_auto(path).map_err(|e| e.to_string())?;
    let mut sheets = Vec::new();
    for name in book.sheet_names() {
        let values = book.worksheet_range(&name).map_err(|e| e.to_string())?;
        // Formulas fill the cells a script saved without results.
        let formulas = book.worksheet_formula(&name).ok();
        let (height, width) = match values.end().zip(formulas.as_ref().and_then(|f| f.end())) {
            Some((v, f)) => (v.0.max(f.0) as usize + 1, v.1.max(f.1) as usize + 1),
            None => values
                .end()
                .or_else(|| formulas.as_ref().and_then(|f| f.end()))
                .map_or((0, 0), |(r, c)| (r as usize + 1, c as usize + 1)),
        };
        let mut rows: Vec<Vec<Cell>> = Vec::with_capacity(height.min(MAX_ROWS + 1));
        for r in 0..height.min(MAX_ROWS + 1) {
            let mut row: Vec<Cell> = (0..width.min(MAX_COLS + 1))
                .map(|c| {
                    values
                        .get_value((r as u32, c as u32))
                        .map(data_cell)
                        .unwrap_or_default()
                })
                .collect();
            if let Some(f) = &formulas {
                for (c, cell) in row.iter_mut().enumerate() {
                    if cell.text.is_empty()
                        && let Some(formula) =
                            f.get_value((r as u32, c as u32)).filter(|s| !s.is_empty())
                    {
                        *cell = Cell {
                            text: format!("={formula}").into(),
                            kind: CellKind::Formula,
                        };
                    }
                }
            }
            while row.last().is_some_and(|c| c.text.is_empty()) {
                row.pop();
            }
            rows.push(row);
        }
        sheets.push(sheet(&name, rows));
    }
    if sheets.is_empty() {
        return Err("The workbook has no sheets".into());
    }
    Ok(Workbook { sheets })
}

fn sheet(name: &str, mut rows: Vec<Vec<Cell>>) -> Sheet {
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    let mut truncated = rows.len() > MAX_ROWS;
    rows.truncate(MAX_ROWS);
    for row in &mut rows {
        truncated |= row.len() > MAX_COLS;
        row.truncate(MAX_COLS);
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let mut chars = vec![0usize; cols];
    for row in &rows {
        for (c, cell) in row.iter().enumerate() {
            chars[c] = chars[c].max(cell.text.chars().count());
        }
    }
    let widths = chars.iter().map(|&n| column_width(n)).collect();
    Sheet {
        name: name.to_string().into(),
        rows,
        widths,
        truncated,
    }
}

/// Pixels for a column whose longest value has `chars` characters.
fn column_width(chars: usize) -> f32 {
    (chars as f32 * 7.4 + 16.).clamp(56., 480.)
}

fn data_cell(value: &Data) -> Cell {
    let (text, kind) = match value {
        Data::Empty => return Cell::default(),
        Data::String(s) => (s.replace(['\r', '\n'], " "), CellKind::Text),
        Data::Int(n) => (n.to_string(), CellKind::Number),
        Data::Float(f) => (format_number(*f), CellKind::Number),
        Data::Bool(b) => (
            if *b { "TRUE" } else { "FALSE" }.to_string(),
            CellKind::Text,
        ),
        Data::DateTime(d) => {
            let text = if d.is_duration() {
                d.as_duration().map(|t| {
                    let s = t.num_seconds();
                    format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
                })
            } else {
                d.as_datetime().map(|t| {
                    let time = t.format("%H:%M").to_string();
                    if time == "00:00" {
                        t.format("%Y-%m-%d").to_string()
                    } else {
                        t.format("%Y-%m-%d %H:%M").to_string()
                    }
                })
            };
            (
                text.unwrap_or_else(|| format_number(d.as_f64())),
                CellKind::Number,
            )
        }
        Data::DateTimeIso(s) | Data::DurationIso(s) => (s.clone(), CellKind::Number),
        Data::Error(e) => (e.to_string(), CellKind::Error),
    };
    Cell {
        text: text.into(),
        kind,
    }
}

fn text_cell(text: String) -> Cell {
    let kind = if !text.is_empty() && text.trim().parse::<f64>().is_ok() {
        CellKind::Number
    } else {
        CellKind::Text
    };
    Cell {
        text: text.into(),
        kind,
    }
}

/// Whole numbers without decimals, others to six places without trailing zeros.
fn format_number(f: f64) -> String {
    if f.fract() == 0. && f.abs() < 1e15 {
        format!("{f:.0}")
    } else {
        let s = format!("{f:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Comma unless the first line has more semicolons or tabs (European exports).
fn guess_separator(text: &str) -> char {
    let first = text.lines().next().unwrap_or_default();
    [',', ';', '\t']
        .into_iter()
        .max_by_key(|&c| (first.matches(c).count(), c == ','))
        .unwrap_or(',')
}

/// RFC 4180 rows: quoted fields may hold the separator, quotes (`""`) and newlines.
fn parse_delimited(text: &str, sep: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                '\r' | '\n' => field.push(' '),
                _ => field.push(c),
            }
        } else if c == '"' && field.is_empty() {
            quoted = true;
        } else if c == sep {
            row.push(std::mem::take(&mut field));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
        } else {
            field.push(c);
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// Excel's column letters: 0 is A, 25 is Z, 26 is AA.
pub fn column_name(mut i: usize) -> String {
    let mut name = Vec::new();
    loop {
        name.push(b'A' + (i % 26) as u8);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    name.reverse();
    String::from_utf8(name).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(701), "ZZ");
        assert_eq!(column_name(702), "AAA");
    }

    #[test]
    fn delimited_rows_keep_quotes_and_separators() {
        let rows = parse_delimited("a,\"b, c\",\"di\"\"jo\"\r\n1,2,3\n", ',');
        assert_eq!(rows, vec![vec!["a", "b, c", "di\"jo"], vec!["1", "2", "3"]]);
        assert_eq!(parse_delimited("x;y\n", ';'), vec![vec!["x", "y"]]);
        assert_eq!(
            parse_delimited("sin,salto", ','),
            vec![vec!["sin", "salto"]]
        );
    }

    #[test]
    fn separator_follows_the_header() {
        assert_eq!(guess_separator("a;b;c\n1;2;3"), ';');
        assert_eq!(guess_separator("a,b\n"), ',');
        assert_eq!(guess_separator("solo"), ',');
    }

    #[test]
    fn numbers_drop_needless_decimals() {
        assert_eq!(format_number(22.5), "22.5");
        assert_eq!(format_number(1350.), "1350");
        assert_eq!(format_number(0.1 + 0.2), "0.3");
    }

    #[test]
    fn csv_cells_are_typed_and_sized() {
        let s = sheet(
            "t",
            vec![
                vec![text_cell("Horas".into()), text_cell("22.5".into())],
                vec![],
            ],
        );
        assert_eq!(s.rows.len(), 1);
        assert_eq!(s.widths.len(), 2);
        assert_eq!(s.rows[0][1].kind, CellKind::Number);
        assert_eq!(s.widths[0], 56.);
    }
}
