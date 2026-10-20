//! Spreadsheet conversions: XLSX / XLS / ODS -> CSV (`calamine`) and CSV -> XLSX
//! (`rust_xlsxwriter`).
//!
//! Operation ids: `sheet.to_csv` and `sheet.to_xlsx`, each with a delimiter suffix
//! (`.comma`, `.semicolon`, `.tab`; bare id = comma). For To CSV it is the delimiter written,
//! for To XLSX the one read. `options["delimiter"]` (a name or the character) wins over the id.
//!
//! To CSV: a workbook with one non-empty sheet becomes `name (converted).csv`; with several,
//! each non-empty sheet becomes `name - SheetName.csv`. Empty sheets are skipped, and a
//! workbook with no data at all yields a single empty `name (converted).csv`. Cells keep their
//! position (leading empty rows/columns are written as empty fields). Dates are written as ISO
//! 8601, booleans as TRUE/FALSE, errors as Excel shows them (`#DIV/0!`). Output is UTF-8
//! without a BOM, RFC 4180 quoting, CRLF line endings.
//!
//! To XLSX: input must be UTF-8 (a BOM is stripped); rows may have differing lengths. The first
//! row is bold, columns are auto-fitted, and fields that are plainly numbers become numeric
//! cells; anything a spreadsheet would mangle (leading zeros, a leading `+`, more than 15
//! significant digits, exponents) stays text.
//!
//! To JSON (`sheet.to_json`, CSV with the same delimiter suffixes): an array of row objects
//! keyed by the first row, in column order; blank header cells become `column N`, repeats get
//! ` (2)`. CSV numbers follow the To XLSX rule, workbook cells keep their types (dates as ISO
//! text, empty cells `null`). A workbook with several non-empty sheets gives one object keyed
//! by sheet name. Output: `name (converted).json`.
//!
//! CSV rows (`sheet.dedupe`, `sheet.split_rows.<n>` / option `rows`): rows are copied byte for
//! byte. Dedupe keeps the first of identical rows (`name (deduped).csv`); split writes parts of
//! at most n data rows, each starting with the header (`name (part N).csv`). The delimiter is
//! `options.delimiter`, else sniffed from the first line.
//!
//! Sort (`sheet.sort.asc` / `.desc`, option `order`): data rows sorted by column 1, or by
//! `options.column` (1-based number or header name); the header stays first. Fields that parse
//! as numbers sort numerically and before text, text ignores case, empty fields go last in
//! either order; ties keep their order. Rows keep their bytes, each ends with the header's line
//! ending (blank lines are dropped). Output: `name (sorted).csv`.
//!
//! Change delimiter (`sheet.change_delimiter.<comma|semicolon|tab>`, or `options.delimiter`
//! for the target): rewritten with the target delimiter and minimal quoting; the source is
//! `options.source_delimiter`, else sniffed. A BOM and the line ending (CRLF / LF) are kept.
//! Output: `name (semicolon).csv` etc.
//!
//! Split sheets (`sheet.split`, XLSX / XLS / ODS): each non-empty sheet becomes its own
//! `name - SheetName.xlsx` (same naming as To CSV). Values only: numbers, text, booleans,
//! dates (with a date format) and cached formula results are kept; formatting, formulas,
//! charts and images are not. A workbook with fewer than two sheets of data is `Empty`.
//!
//! To HTML (`sheet.to_html`, XLSX / XLS / ODS): one standalone `name (converted).html` with a
//! table per non-empty sheet (a heading per sheet when there are several), the first row as
//! the header, cell text as in To CSV, numbers right-aligned.

use std::path::{Path, PathBuf};

use calamine::{open_workbook_auto, Data, ExcelDateTime, Range, Reader, SheetType};
use rust_xlsxwriter::{Format, Workbook};

use crate::mime::{CSV, ODS, XLS, XLSX};
use crate::{
    catalog, create_unique, create_unique_sibling, write_all_or_nothing, write_siblings, Converter, EngineError, Operation,
    Options, Result,
};

pub struct SheetConverter;

const TO_CSV: &str = "sheet.to_csv";
const TO_XLSX: &str = "sheet.to_xlsx";
const TO_JSON: &str = "sheet.to_json";
const DEDUPE: &str = "sheet.dedupe";
const SPLIT_ROWS: &str = "sheet.split_rows";
const SORT: &str = "sheet.sort";
const CHANGE_DELIMITER: &str = "sheet.change_delimiter";
const SPLIT_SHEETS: &str = "sheet.split";
const TO_HTML: &str = "sheet.to_html";
/// `(id suffix, byte, label, lucide icon)`.
const DELIMITERS: [(&str, u8, &str, &str); 3] = [
    ("comma", b',', "Comma", "Columns3"),
    ("semicolon", b';', "Semicolon", "Columns3"),
    ("tab", b'\t', "Tab", "ArrowRightToLine"),
];

/// `base` + one child per delimiter.
fn with_delimiters(base: &str) -> Operation {
    let children = DELIMITERS
        .iter()
        .map(|(suffix, _, label, icon)| Operation::new(&format!("{base}.{suffix}"), label, icon))
        .collect();
    catalog::op(base).with_children(children)
}

/// The delimiter for an op id under `base`: from `options`, else the id suffix, else a comma.
fn delimiter(id: &str, base: &str, options: &Options) -> Result<u8> {
    let suffix = id.strip_prefix(base).and_then(|r| r.strip_prefix('.'));
    let Some(raw) = options.get("delimiter").map(String::as_str).or(suffix) else { return Ok(b',') };
    named_delimiter(raw)
}

/// A delimiter by name (`comma`, `semicolon`, `tab`) or as the character itself.
fn named_delimiter(raw: &str) -> Result<u8> {
    DELIMITERS
        .iter()
        .find(|(name, byte, ..)| *name == raw || raw.as_bytes() == [*byte])
        .map(|d| d.1)
        .ok_or_else(|| EngineError::InvalidOption(format!("unknown delimiter `{raw}`")))
}

/// Whether `id` is `base` or `base.<something>`.
fn is_op(id: &str, base: &str) -> bool {
    id.strip_prefix(base).is_some_and(|r| r.is_empty() || r.starts_with('.'))
}

fn is_workbook(mime: &str) -> bool {
    [XLSX, XLS, ODS].contains(&mime)
}

/// Text for a cell, the way a person reading the CSV expects it.
fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_string(),
        Data::DateTime(dt) => datetime_text(dt),
        other => other.to_string(), // numbers, strings, ISO dates, `#N/A`, empty
    }
}

fn datetime_text(dt: &ExcelDateTime) -> String {
    let serial = dt.as_f64();
    if dt.is_duration() {
        let secs = (serial * 86_400.0).round() as i64;
        let sign = if secs < 0 { "-" } else { "" };
        let secs = secs.abs();
        return format!("{sign}{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    }
    if serial < 0.0 {
        return serial.to_string(); // before Excel's epoch; keep the raw serial
    }
    let (y, mo, d, h, mi, s, _ms) = dt.to_ymd_hms_milli();
    if serial < 1.0 {
        format!("{h:02}:{mi:02}:{s:02}") // a time of day with no date
    } else if serial.fract() == 0.0 {
        format!("{y:04}-{mo:02}-{d:02}")
    } else {
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
    }
}

fn range_to_csv(range: &Range<Data>, delimiter: u8) -> Result<Vec<u8>> {
    let mut w = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());
    if let Some((last_row, last_col)) = range.end() {
        for r in 0..=last_row {
            let row = (0..=last_col).map(|c| range.get_value((r, c)).map(cell_text).unwrap_or_default());
            w.write_record(row)?;
        }
    }
    w.into_inner().map_err(|e| EngineError::Io(e.into_error()))
}

/// A sheet name made safe for a file name on every OS.
fn file_safe(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    cleaned.trim().trim_end_matches('.').to_string()
}

/// The non-empty worksheets of a workbook, in order.
fn data_sheets(input: &Path) -> Result<Vec<(String, Range<Data>)>> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let mut book = open_workbook_auto(input)?;
    let names: Vec<String> =
        book.sheets_metadata().iter().filter(|s| s.typ == SheetType::WorkSheet).map(|s| s.name.clone()).collect();
    let mut sheets = Vec::new();
    for name in names {
        let range = book.worksheet_range(&name)?;
        if !range.is_empty() {
            sheets.push((name, range));
        }
    }
    Ok(sheets)
}

/// Writes `name - SheetName.ext` next to `input` for each sheet (`name - SheetName (2).ext`
/// when taken; sheet names made file-safe), all or none.
fn write_per_sheet(input: &Path, ext: &str, sheets: Vec<(String, Vec<u8>)>) -> Result<Vec<PathBuf>> {
    let dir = input.parent().unwrap_or_else(|| Path::new(""));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let mut outputs = Vec::new();
    for (i, (name, bytes)) in sheets.into_iter().enumerate() {
        let sheet = match file_safe(&name) {
            s if s.is_empty() => format!("Sheet{}", i + 1),
            s => s,
        };
        let created = create_unique(dir, |n| match n {
            1 => format!("{stem} - {sheet}.{ext}"),
            n => format!("{stem} - {sheet} ({n}).{ext}"),
        });
        match created {
            Ok((file, path)) => outputs.push((file, path, bytes)),
            Err(e) => {
                for (_, p, _) in &outputs {
                    let _ = std::fs::remove_file(p);
                }
                return Err(e);
            }
        }
    }
    write_all_or_nothing(outputs)
}

fn workbook_to_csv(input: &Path, delimiter: u8) -> Result<Vec<PathBuf>> {
    let mut sheets = Vec::new();
    for (name, range) in data_sheets(input)? {
        sheets.push((name, range_to_csv(&range, delimiter)?));
    }
    if sheets.len() <= 1 {
        let bytes = sheets.pop().map(|(_, b)| b).unwrap_or_default();
        let (file, path) = create_unique_sibling(input, "converted", Some("csv"))?;
        return write_all_or_nothing(vec![(file, path, bytes)]);
    }
    write_per_sheet(input, "csv", sheets)
}

/// A CSV field as an Excel number, if it is unambiguously one: `-?digits(.digits)?` with no
/// leading zero (so ZIP codes and IDs like `007` stay text) and at most 15 significant digits
/// (Excel's precision, so long IDs aren't rounded).
fn as_number(field: &str) -> Option<f64> {
    let unsigned = field.strip_prefix('-').unwrap_or(field);
    let (int, frac) = match unsigned.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (unsigned, None),
    };
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(int) || frac.is_some_and(|f| !all_digits(f)) {
        return None;
    }
    if int.len() > 1 && int.starts_with('0') {
        return None;
    }
    if int.len() + frac.map_or(0, str::len) > 15 {
        return None;
    }
    field.parse().ok()
}

/// Excel sheet names: at most 31 chars, none of `[]:*?/\`, no leading/trailing apostrophe.
fn sheet_name(stem: &str) -> String {
    let cleaned: String = stem.chars().map(|c| if r"[]:*?/\".contains(c) { '_' } else { c }).collect();
    let name: String = cleaned.trim_matches('\'').chars().take(31).collect();
    if name.trim().is_empty() {
        "Sheet1".to_string()
    } else {
        name
    }
}

fn csv_to_xlsx(input: &Path, delimiter: u8) -> Result<PathBuf> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    // csv-core strips a leading UTF-8 BOM itself; invalid UTF-8 is a `Csv` error.
    let mut reader = csv::ReaderBuilder::new().delimiter(delimiter).has_headers(false).flexible(true).from_path(input)?;
    let mut book = Workbook::new();
    let sheet = book.add_worksheet();
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("Sheet1");
    // A name Excel still rejects (e.g. the reserved "History") just keeps the default.
    let _ = sheet.set_name(sheet_name(stem));
    let bold = Format::new().set_bold();

    for (r, record) in reader.records().enumerate() {
        let record = record?;
        let row = u32::try_from(r).map_err(|_| EngineError::InvalidOption("too many rows".into()))?;
        for (c, field) in record.iter().enumerate() {
            if field.is_empty() {
                continue;
            }
            let col = u16::try_from(c).map_err(|_| EngineError::InvalidOption("too many columns".into()))?;
            if row == 0 {
                sheet.write_string_with_format(row, col, field, &bold)?;
            } else if let Some(n) = as_number(field) {
                sheet.write_number(row, col, n)?;
            } else {
                sheet.write_string(row, col, field)?;
            }
        }
    }
    sheet.autofit();
    let bytes = book.save_to_buffer()?;

    let (file, path) = create_unique_sibling(input, "converted", Some("xlsx"))?;
    write_all_or_nothing(vec![(file, path.clone(), bytes)])?;
    Ok(path)
}

// ---- JSON -----------------------------------------------------------------------------------
// Written by hand rather than through serde_json's map so keys keep the header order.

fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Header cells as JSON keys: blanks become `column N`, repeats `name (2)`, `name (3)`, ...
fn unique_keys(header: &[String], width: usize) -> Vec<String> {
    let mut keys: Vec<String> = Vec::with_capacity(width);
    for i in 0..width {
        let base = match header.get(i).map(|h| h.trim()) {
            Some(h) if !h.is_empty() => h.to_string(),
            _ => format!("column {}", i + 1),
        };
        let mut key = base.clone();
        let mut n = 1;
        while keys.contains(&key) {
            n += 1;
            key = format!("{base} ({n})");
        }
        keys.push(key);
    }
    keys
}

/// An array of objects, one per row; each value is JSON text already, `None` is null.
fn json_table(keys: &[String], rows: &[Vec<Option<String>>], indent: &str) -> String {
    if rows.is_empty() {
        return "[]".into();
    }
    let objects: Vec<String> = rows
        .iter()
        .map(|row| {
            let fields: Vec<String> = keys
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    let v = row.get(i).cloned().flatten().unwrap_or_else(|| "null".into());
                    format!("{indent}    {}: {v}", json_string(k))
                })
                .collect();
            format!("{indent}  {{\n{}\n{indent}  }}", fields.join(",\n"))
        })
        .collect();
    format!("[\n{}\n{indent}]", objects.join(",\n"))
}

fn write_json(input: &Path, text: String) -> Result<PathBuf> {
    Ok(write_siblings(input, "json", vec![("converted".into(), (text + "\n").into_bytes())])?.remove(0))
}

/// CSV -> JSON: the first row names the keys; plain numbers (as for XLSX) become JSON numbers,
/// other fields strings (an empty field is `""`); cells missing from short rows are `null`.
fn csv_to_json(input: &Path, delimiter: u8) -> Result<PathBuf> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let mut reader = csv::ReaderBuilder::new().delimiter(delimiter).has_headers(false).flexible(true).from_path(input)?;
    let mut records = reader.records();
    let header: Vec<String> = match records.next() {
        Some(r) => r?.iter().map(String::from).collect(),
        None => Vec::new(),
    };
    let mut width = header.len();
    let mut rows = Vec::new();
    for record in records {
        let record = record?;
        width = width.max(record.len());
        // `as_number` only accepts `-?digits(.digits)?`, which is also a valid JSON number.
        rows.push(record.iter().map(|f| Some(if as_number(f).is_some() { f.to_string() } else { json_string(f) })).collect());
    }
    write_json(input, json_table(&unique_keys(&header, width), &rows, ""))
}

/// A workbook cell as JSON text; empty cells are `null`.
fn cell_json(cell: &Data) -> Option<String> {
    match cell {
        Data::Empty => None,
        Data::Int(i) => Some(i.to_string()),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => Some((*f as i64).to_string()),
        Data::Float(f) => serde_json::Number::from_f64(*f).map(|n| n.to_string()),
        Data::Bool(b) => Some(b.to_string()),
        Data::String(s) => Some(json_string(s)),
        other => Some(json_string(&cell_text(other))), // dates (ISO 8601), errors (`#N/A`)
    }
}

fn range_to_json(range: &Range<Data>, indent: &str) -> String {
    let mut rows = range.rows();
    let header: Vec<String> = rows.next().map(|r| r.iter().map(cell_text).collect()).unwrap_or_default();
    let body: Vec<Vec<Option<String>>> = rows
        .filter(|r| r.iter().any(|c| *c != Data::Empty))
        .map(|r| r.iter().map(cell_json).collect())
        .collect();
    json_table(&unique_keys(&header, range.width()), &body, indent)
}

/// XLSX/XLS/ODS -> JSON: one non-empty sheet gives an array of row objects (keys from its first
/// row; empty rows skipped); several give an object of those arrays keyed by sheet name.
fn workbook_to_json(input: &Path) -> Result<PathBuf> {
    let sheets = data_sheets(input)?;
    let text = match sheets.as_slice() {
        [] => "[]".to_string(),
        [(_, range)] => range_to_json(range, ""),
        many => {
            let fields: Vec<String> =
                many.iter().map(|(name, range)| format!("  {}: {}", json_string(name), range_to_json(range, "  "))).collect();
            format!("{{\n{}\n}}", fields.join(",\n"))
        }
    };
    write_json(input, text)
}

// ---- row operations on CSV ------------------------------------------------------------------
// Rows are copied byte for byte (quoting, line endings and a BOM stay as they were); parsing
// is only used to find where rows start and to compare them.

/// The delimiter for dedupe/split: `options.delimiter`, else the most common of `,` `;` tab in
/// the first line (outside quotes), else a comma.
fn sniffed_delimiter(bytes: &[u8], options: &Options) -> Result<u8> {
    if let Some(raw) = options.get("delimiter") {
        return named_delimiter(raw);
    }
    let mut counts = [0usize; 3];
    let mut quoted = false;
    for &b in bytes.iter().take(64 * 1024) {
        match b {
            b'"' => quoted = !quoted,
            b'\n' | b'\r' if !quoted => break,
            _ if !quoted => {
                if let Some(i) = DELIMITERS.iter().position(|d| d.1 == b) {
                    counts[i] += 1;
                }
            }
            _ => {}
        }
    }
    let best = (0..3).max_by_key(|&i| (counts[i], std::cmp::Reverse(i))).unwrap_or(0);
    Ok(if counts[best] == 0 { b',' } else { DELIMITERS[best].1 })
}

/// A record's raw bytes and its parsed fields.
type RawRecord<'a> = (&'a [u8], Vec<Vec<u8>>);

/// Each record's bytes (from its start to the next record's start, line ending included) and
/// its fields. The first record starts at 0 so a BOM stays with it.
fn byte_records(bytes: &[u8], delimiter: u8) -> Result<Vec<RawRecord<'_>>> {
    let mut reader = csv::ReaderBuilder::new().delimiter(delimiter).has_headers(false).flexible(true).from_reader(bytes);
    let mut starts = Vec::new();
    let mut fields = Vec::new();
    let mut record = csv::ByteRecord::new();
    while reader.read_byte_record(&mut record)? {
        let mut start = if starts.is_empty() { 0 } else { record.position().map_or(0, |p| p.byte() as usize) };
        // csv reports a record after CRLF as starting at the LF; a record never starts with a
        // line break, so the previous record keeps its whole line ending (and any blank lines).
        while matches!(bytes.get(start), Some(b'\r' | b'\n')) {
            start += 1;
        }
        starts.push(start);
        fields.push(record.iter().map(<[u8]>::to_vec).collect());
    }
    let ends = starts.iter().skip(1).copied().chain(std::iter::once(bytes.len()));
    Ok(starts.iter().zip(ends).map(|(&s, e)| &bytes[s..e]).zip(fields).collect())
}

fn read_csv(input: &Path) -> Result<Vec<u8>> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    Ok(std::fs::read(input)?)
}

/// Removes rows that repeat an earlier row (compared field by field, so `"1",2` equals `1,2`);
/// the first occurrence stays, in place.
fn dedupe(input: &Path, options: &Options) -> Result<PathBuf> {
    let bytes = read_csv(input)?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(bytes.len());
    for (raw, fields) in byte_records(&bytes, sniffed_delimiter(&bytes, options)?)? {
        if seen.insert(fields) {
            out.extend_from_slice(raw);
        }
    }
    Ok(write_siblings(input, "csv", vec![("deduped".into(), out)])?.remove(0))
}

/// Splits into `name (part N).csv` files of at most `rows` data rows each, every part starting
/// with the header row. All parts or none are written.
fn split_rows(input: &Path, rows: usize, options: &Options) -> Result<Vec<PathBuf>> {
    let bytes = read_csv(input)?;
    let records = byte_records(&bytes, sniffed_delimiter(&bytes, options)?)?;
    let Some(((header, _), body)) = records.split_first() else {
        return Err(EngineError::Empty("the CSV is empty".into()));
    };
    if body.is_empty() {
        return Err(EngineError::Empty("the CSV has only a header row".into()));
    }
    let mut header = header.to_vec();
    if !header.ends_with(b"\n") && !header.ends_with(b"\r") {
        header.extend_from_slice(b"\r\n");
    }
    let parts = body
        .chunks(rows)
        .enumerate()
        .map(|(i, chunk)| {
            let mut part = header.clone();
            chunk.iter().for_each(|(raw, _)| part.extend_from_slice(raw));
            (format!("part {}", i + 1), part)
        })
        .collect();
    write_siblings(input, "csv", parts)
}

/// Rows per part: `options.rows`, else the id suffix (`sheet.split_rows.1000`).
fn rows_per_part(id: &str, options: &Options) -> Result<usize> {
    let raw = options.get("rows").map(String::as_str).or_else(|| id.strip_prefix(SPLIT_ROWS)?.strip_prefix('.'));
    let raw = raw.ok_or_else(|| EngineError::InvalidOption("missing `rows`".into()))?;
    match raw.trim().parse::<usize>() {
        Ok(n @ 1..=100_000_000) => Ok(n),
        _ => Err(EngineError::InvalidOption(format!("`rows` is not valid: `{raw}`"))),
    }
}


// ---- sort / change delimiter ----------------------------------------------------------------

/// A field as a number for sorting: anything Rust parses as a finite float (so `007`, `+4`
/// and `1e3` count), without surrounding spaces.
fn sort_number(field: &str) -> Option<f64> {
    let f = field.trim();
    if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) {
        return None;
    }
    f.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Numbers before text, text without case (ties: exact text); empty fields always last.
fn compare_fields(a: &[u8], b: &[u8], descending: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let (a, b) = (String::from_utf8_lossy(a), String::from_utf8_lossy(b));
    let (a, b) = (a.trim(), b.trim());
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Equal,
        (true, false) => return Greater,
        (false, true) => return Less,
        _ => {}
    }
    let order = match (sort_number(a), sort_number(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Equal),
        (Some(_), None) => Less,
        (None, Some(_)) => Greater,
        (None, None) => a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)),
    };
    if descending {
        order.reverse()
    } else {
        order
    }
}

/// The column to sort by (0-based): `options.column` as a 1-based number or a header name
/// (exact, then ignoring case); the first column by default.
fn sort_column(header: &[Vec<u8>], options: &Options) -> Result<usize> {
    let Some(raw) = options.get("column").map(|c| c.trim()) else { return Ok(0) };
    let names: Vec<String> = header.iter().map(|h| String::from_utf8_lossy(h).trim().to_string()).collect();
    if let Some(i) = names.iter().position(|n| n == raw).or_else(|| names.iter().position(|n| n.eq_ignore_ascii_case(raw))) {
        return Ok(i);
    }
    match raw.parse::<usize>() {
        Ok(n @ 1..) if n <= header.len() => Ok(n - 1),
        _ => Err(EngineError::InvalidOption(format!("no column `{raw}`"))),
    }
}

/// The line ending a record ends with (CRLF if it has none).
fn line_ending(raw: &[u8]) -> &'static [u8] {
    if raw.ends_with(b"\r\n") {
        b"\r\n"
    } else if raw.ends_with(b"\n") {
        b"\n"
    } else if raw.ends_with(b"\r") {
        b"\r"
    } else {
        b"\r\n"
    }
}

fn trim_line_breaks(raw: &[u8]) -> &[u8] {
    let end = raw.iter().rposition(|b| *b != b'\r' && *b != b'\n').map_or(0, |i| i + 1);
    &raw[..end]
}

fn sort_rows(input: &Path, id: &str, options: &Options) -> Result<PathBuf> {
    let order = options.get("order").map(String::as_str).or_else(|| id.strip_prefix(SORT)?.strip_prefix('.')).unwrap_or("asc");
    let descending = match order {
        "asc" => false,
        "desc" => true,
        other => return Err(EngineError::InvalidOption(format!("unknown sort order `{other}`"))),
    };
    let bytes = read_csv(input)?;
    let mut records = byte_records(&bytes, sniffed_delimiter(&bytes, options)?)?;
    if records.len() < 2 {
        return Err(EngineError::Empty("the CSV has no rows to sort".into()));
    }
    let (header, header_fields) = records.remove(0);
    let column = sort_column(&header_fields, options)?;
    let eol = line_ending(header);
    // Stable sort: rows with equal keys keep their order.
    records.sort_by(|(_, a), (_, b)| {
        let empty: &[u8] = b"";
        let field = |f: &Vec<Vec<u8>>| f.get(column).map_or(empty, |v| v.as_slice()).to_vec();
        compare_fields(&field(a), &field(b), descending)
    });
    let mut out = Vec::with_capacity(bytes.len() + 2);
    out.extend_from_slice(trim_line_breaks(header));
    out.extend_from_slice(eol);
    for (raw, _) in records {
        let row = trim_line_breaks(raw);
        if !row.is_empty() {
            out.extend_from_slice(row);
            out.extend_from_slice(eol);
        }
    }
    Ok(write_siblings(input, "csv", vec![("sorted".into(), out)])?.remove(0))
}

fn change_delimiter(input: &Path, id: &str, options: &Options) -> Result<PathBuf> {
    let target = delimiter(id, CHANGE_DELIMITER, options)?;
    let bytes = read_csv(input)?;
    let source = match options.get("source_delimiter") {
        Some(raw) => named_delimiter(raw)?,
        None => sniffed_delimiter(&bytes, &Options::new())?,
    };
    let name = |d: u8| DELIMITERS.iter().find(|x| x.1 == d).map_or("comma", |x| x.0);
    if source == target {
        return Err(EngineError::InvalidOption(format!("the file already uses {} as its delimiter", name(target))));
    }
    let eol = match bytes.iter().position(|b| *b == b'\n' || *b == b'\r') {
        Some(i) if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') => csv::Terminator::CRLF,
        Some(i) => csv::Terminator::Any(bytes[i]),
        None => csv::Terminator::CRLF,
    };
    let mut reader = csv::ReaderBuilder::new().delimiter(source).has_headers(false).flexible(true).from_reader(&bytes[..]);
    let mut out = Vec::with_capacity(bytes.len());
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        out.extend_from_slice(b"\xEF\xBB\xBF");
    }
    let mut writer = csv::WriterBuilder::new().delimiter(target).terminator(eol).flexible(true).from_writer(out);
    let mut record = csv::ByteRecord::new();
    while reader.read_byte_record(&mut record)? {
        writer.write_byte_record(&record)?;
    }
    let out = writer.into_inner().map_err(|e| EngineError::Io(e.into_error()))?;
    Ok(write_siblings(input, "csv", vec![(name(target).into(), out)])?.remove(0))
}

// ---- split sheets / HTML --------------------------------------------------------------------

/// A sheet's values as a one-sheet XLSX.
fn range_to_xlsx(name: &str, range: &Range<Data>) -> Result<Vec<u8>> {
    let mut book = Workbook::new();
    let sheet = book.add_worksheet();
    let _ = sheet.set_name(name); // a name Excel rejects (possible from ODS) keeps the default
    let date = Format::new().set_num_format("yyyy-mm-dd");
    let stamp = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    let time = Format::new().set_num_format("hh:mm:ss");
    let duration = Format::new().set_num_format("[h]:mm:ss");
    let (row0, col0) = range.start().unwrap_or((0, 0));
    for (r, row) in range.rows().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            let (Ok(r), Ok(c)) = (u32::try_from(r as u64 + u64::from(row0)), u16::try_from(c as u64 + u64::from(col0))) else {
                return Err(EngineError::InvalidOption("sheet too large".into()));
            };
            match cell {
                Data::Empty => {}
                Data::Int(i) => {
                    sheet.write_number(r, c, *i as f64)?;
                }
                Data::Float(f) => {
                    sheet.write_number(r, c, *f)?;
                }
                Data::Bool(b) => {
                    sheet.write_boolean(r, c, *b)?;
                }
                Data::String(s) => {
                    sheet.write_string(r, c, s)?;
                }
                Data::DateTime(dt) => {
                    let serial = dt.as_f64();
                    let format = if dt.is_duration() {
                        &duration
                    } else if serial < 1.0 {
                        &time
                    } else if serial.fract() == 0.0 {
                        &date
                    } else {
                        &stamp
                    };
                    sheet.write_number_with_format(r, c, serial, format)?;
                }
                other => {
                    sheet.write_string(r, c, cell_text(other))?; // ISO dates/durations, errors
                }
            }
        }
    }
    sheet.autofit();
    Ok(book.save_to_buffer()?)
}

fn split_sheets(input: &Path) -> Result<Vec<PathBuf>> {
    let sheets = data_sheets(input)?;
    if sheets.len() < 2 {
        return Err(EngineError::Empty("the workbook has only one sheet with data".into()));
    }
    let mut files = Vec::new();
    for (name, range) in sheets {
        let bytes = range_to_xlsx(&name, &range)?;
        files.push((name, bytes));
    }
    write_per_sheet(input, "xlsx", files)
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("<br>"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn range_to_html(range: &Range<Data>) -> String {
    let mut html = String::from("<table>\n");
    for (i, row) in range.rows().enumerate() {
        let tag = if i == 0 { "th" } else { "td" };
        if i == 0 {
            html.push_str("<thead>\n");
        } else if i == 1 {
            html.push_str("<tbody>\n");
        }
        html.push_str("<tr>");
        for cell in row {
            let numeric = matches!(cell, Data::Int(_) | Data::Float(_)) && i > 0;
            let class = if numeric { " class=\"n\"" } else { "" };
            html.push_str(&format!("<{tag}{class}>{}</{tag}>", html_escape(&cell_text(cell))));
        }
        html.push_str("</tr>\n");
        if i == 0 {
            html.push_str("</thead>\n");
        }
    }
    if range.height() > 1 {
        html.push_str("</tbody>\n");
    }
    html.push_str("</table>\n");
    html
}

fn workbook_to_html(input: &Path) -> Result<PathBuf> {
    let sheets = data_sheets(input)?;
    if sheets.is_empty() {
        return Err(EngineError::Empty("the workbook has no data".into()));
    }
    let title = input.file_stem().and_then(|s| s.to_str()).unwrap_or("Workbook");
    let mut body = String::new();
    for (name, range) in &sheets {
        if sheets.len() > 1 {
            body.push_str(&format!("<h2>{}</h2>\n", html_escape(name)));
        }
        body.push_str(&range_to_html(range));
    }
    let html = format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>{}</title>\n<style>\n\
         body {{ font-family: system-ui, sans-serif; margin: 2rem; }}\n\
         table {{ border-collapse: collapse; margin-bottom: 2rem; }}\n\
         th, td {{ border: 1px solid #ccc; padding: 0.25rem 0.5rem; text-align: left; vertical-align: top; }}\n\
         th {{ background: #f3f3f3; }}\n\
         td.n {{ text-align: right; font-variant-numeric: tabular-nums; }}\n\
         </style>\n</head>\n<body>\n{body}</body>\n</html>\n",
        html_escape(title)
    );
    Ok(write_siblings(input, "html", vec![("converted".into(), html.into_bytes())])?.remove(0))
}

impl Converter for SheetConverter {
    fn can_handle(&self, mime: &str) -> bool {
        is_workbook(mime) || mime == CSV
    }

    fn operations(&self, mime: &str) -> Vec<Operation> {
        if is_workbook(mime) {
            vec![with_delimiters(TO_CSV), catalog::op(TO_JSON), catalog::op(SPLIT_SHEETS), catalog::op(TO_HTML)]
        } else if mime == CSV {
            let split = [(100, "100 rows"), (1000, "1,000 rows"), (10_000, "10,000 rows")]
                .into_iter()
                .map(|(n, label)| Operation::new(&format!("{SPLIT_ROWS}.{n}"), label, "Rows3"))
                .collect();
            vec![
                with_delimiters(TO_XLSX),
                with_delimiters(TO_JSON),
                with_delimiters(CHANGE_DELIMITER),
                catalog::op(DEDUPE),
                catalog::op(SORT).with_children(vec![
                    Operation::new(&format!("{SORT}.asc"), "A → Z", "ArrowDownAZ"),
                    Operation::new(&format!("{SORT}.desc"), "Z → A", "ArrowDownZA"),
                ]),
                catalog::op(SPLIT_ROWS).with_children(split),
            ]
        } else {
            Vec::new()
        }
    }

    fn run(&self, op: &Operation, inputs: &[PathBuf], options: &Options) -> Result<Vec<PathBuf>> {
        let id = op.id.as_str();
        if is_op(id, TO_CSV) {
            let delim = delimiter(id, TO_CSV, options)?;
            let mut out = Vec::new();
            for input in inputs {
                out.extend(workbook_to_csv(input, delim)?);
            }
            Ok(out)
        } else if is_op(id, TO_XLSX) {
            let delim = delimiter(id, TO_XLSX, options)?;
            inputs.iter().map(|input| csv_to_xlsx(input, delim)).collect()
        } else if is_op(id, TO_JSON) {
            let delim = delimiter(id, TO_JSON, options)?;
            inputs
                .iter()
                .map(|input| if crate::mime::detect(input) == CSV { csv_to_json(input, delim) } else { workbook_to_json(input) })
                .collect()
        } else if id == DEDUPE {
            inputs.iter().map(|input| dedupe(input, options)).collect()
        } else if is_op(id, SPLIT_ROWS) {
            let rows = rows_per_part(id, options)?;
            let mut out = Vec::new();
            for input in inputs {
                out.extend(split_rows(input, rows, options)?);
            }
            Ok(out)
        } else if is_op(id, SORT) {
            inputs.iter().map(|input| sort_rows(input, id, options)).collect()
        } else if is_op(id, CHANGE_DELIMITER) {
            inputs.iter().map(|input| change_delimiter(input, id, options)).collect()
        } else if id == SPLIT_SHEETS {
            let mut out = Vec::new();
            for input in inputs {
                out.extend(split_sheets(input)?);
            }
            Ok(out)
        } else if id == TO_HTML {
            inputs.iter().map(|input| workbook_to_html(input)).collect()
        } else {
            Err(EngineError::UnknownOperation(op.id.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{open_workbook, Xlsx};
    use std::io::{Read, Write};

    fn run(id: &str, inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
        run_with(id, inputs, &[])
    }

    fn run_with(id: &str, inputs: &[PathBuf], options: &[(&str, &str)]) -> Result<Vec<PathBuf>> {
        let opts: Options = options.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        SheetConverter.run(&Operation::new(id, id, "x"), inputs, &opts)
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    // ---- fixture builders -------------------------------------------------------------

    type Fill<'a> = &'a dyn Fn(&mut rust_xlsxwriter::Worksheet);
    type XlsSheet<'a> = (&'a str, Vec<(u16, u16, XlsCell<'a>)>);

    /// Writes an .xlsx with the given sheets; each sheet is written by its closure.
    fn xlsx_fixture(path: &Path, sheets: &[(&str, Fill)]) {
        let mut book = Workbook::new();
        for (name, fill) in sheets {
            let ws = book.add_worksheet();
            ws.set_name(*name).unwrap();
            fill(ws);
        }
        book.save(path).unwrap();
    }

    enum XlsCell<'a> {
        Text(&'a str),
        Number(f64),
    }

    /// Writes a minimal BIFF8 .xls: a CFB container with a `Workbook` stream holding BOF,
    /// BoundSheet8 and EOF records, then each sheet's BOF / Label / Number / EOF records.
    fn xls_fixture(path: &Path, sheets: &[XlsSheet]) {
        fn record(out: &mut Vec<u8>, typ: u16, data: &[u8]) {
            out.extend(typ.to_le_bytes());
            out.extend((data.len() as u16).to_le_bytes());
            out.extend(data);
        }
        fn utf16(s: &str) -> Vec<u8> {
            s.encode_utf16().flat_map(u16::to_le_bytes).collect()
        }
        fn bof(out: &mut Vec<u8>, dt: u16) {
            let mut d = [0u8; 16];
            d[..2].copy_from_slice(&0x0600u16.to_le_bytes());
            d[2..4].copy_from_slice(&dt.to_le_bytes());
            record(out, 0x0809, &d);
        }

        let mut bodies = Vec::new();
        for (_, cells) in sheets {
            let mut s = Vec::new();
            bof(&mut s, 0x0010);
            for (row, col, cell) in cells {
                let mut d = [row.to_le_bytes(), col.to_le_bytes(), [0, 0]].concat();
                match cell {
                    XlsCell::Text(t) => {
                        d.extend((t.encode_utf16().count() as u16).to_le_bytes());
                        d.push(1); // fHighByte: UTF-16LE characters
                        d.extend(utf16(t));
                        record(&mut s, 0x0204, &d);
                    }
                    XlsCell::Number(n) => {
                        d.extend(n.to_le_bytes());
                        record(&mut s, 0x0203, &d);
                    }
                }
            }
            record(&mut s, 0x000A, &[]);
            bodies.push(s);
        }

        let bound_len = |name: &str| 4 + 8 + 2 * name.encode_utf16().count();
        let globals_len = 20 + sheets.iter().map(|(n, _)| bound_len(n)).sum::<usize>() + 4;
        let mut stream = Vec::new();
        bof(&mut stream, 0x0005);
        let mut pos = globals_len;
        for ((name, _), body) in sheets.iter().zip(&bodies) {
            let mut d = (pos as u32).to_le_bytes().to_vec();
            d.extend([0, 0, name.encode_utf16().count() as u8, 1]);
            d.extend(utf16(name));
            record(&mut stream, 0x0085, &d);
            pos += body.len();
        }
        record(&mut stream, 0x000A, &[]);
        assert_eq!(stream.len(), globals_len);
        stream.extend(bodies.concat());

        let mut cf = cfb::create(path).unwrap();
        cf.create_stream("/Workbook").unwrap().write_all(&stream).unwrap();
        cf.flush().unwrap();
    }

    /// Writes a minimal .ods: an uncompressed `mimetype` first, the manifest, then `content.xml`.
    fn ods_fixture(path: &Path, tables_xml: &str) {
        use zip::write::SimpleFileOptions;
        let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        z.start_file("mimetype", stored).unwrap();
        z.write_all(ODS.as_bytes()).unwrap();
        z.start_file("META-INF/manifest.xml", stored).unwrap();
        z.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">
  <manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/>
  <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#,
        )
        .unwrap();
        z.start_file("content.xml", stored).unwrap();
        write!(
            z,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
  xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
  xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.2">
<office:body><office:spreadsheet>{tables_xml}</office:spreadsheet></office:body>
</office:document-content>"#
        )
        .unwrap();
        z.finish().unwrap();
    }

    fn zip_entry(path: &Path, name: &str) -> String {
        let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut s = String::new();
        z.by_name(name).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    fn read_xlsx(path: &Path) -> Vec<(String, Range<Data>)> {
        let mut book: Xlsx<_> = open_workbook(path).unwrap();
        book.sheet_names().into_iter().map(|n| (n.clone(), book.worksheet_range(&n).unwrap())).collect()
    }

    // ---- workbook -> CSV --------------------------------------------------------------

    #[test]
    fn multi_sheet_xlsx_writes_one_csv_per_sheet() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("Budget.xlsx");
        let date = Format::new().set_num_format("yyyy-mm-dd");
        let stamp = Format::new().set_num_format("yyyy-mm-dd hh:mm");
        let hours = Format::new().set_num_format("[h]:mm:ss");
        xlsx_fixture(
            &input,
            &[
                ("Q1", &|ws| {
                    ws.write(0, 0, "Item").unwrap();
                    ws.write(0, 1, "Cost").unwrap();
                    ws.write(1, 0, "Rent, office").unwrap();
                    ws.write(1, 1, 1200).unwrap();
                    ws.write(2, 0, "He said \"hi\"\nthen left").unwrap();
                    ws.write(2, 1, 0.5).unwrap();
                    ws.write(3, 0, "Café ☕").unwrap();
                    ws.write(3, 1, true).unwrap();
                    ws.write_number_with_format(4, 0, 45306.0, &date).unwrap();
                    ws.write_number_with_format(4, 1, 45306.5, &stamp).unwrap();
                    ws.write_number_with_format(5, 0, 1.5, &hours).unwrap();
                }),
                ("Blank", &|_| {}),
                ("Q2", &|ws| {
                    ws.write(0, 0, "only").unwrap();
                }),
            ],
        );

        let out = run(TO_CSV, std::slice::from_ref(&input)).unwrap();
        assert_eq!(out, vec![d.path().join("Budget - Q1.csv"), d.path().join("Budget - Q2.csv")]);
        assert_eq!(
            read(&out[0]),
            "Item,Cost\r\n\
             \"Rent, office\",1200\r\n\
             \"He said \"\"hi\"\"\nthen left\",0.5\r\n\
             Café ☕,TRUE\r\n\
             2024-01-15,2024-01-15 12:00:00\r\n\
             36:00:00,\r\n"
        );
        assert_eq!(read(&out[1]), "only\r\n");
        assert!(!d.path().join("Budget - Blank.csv").exists(), "empty sheets are skipped");
        assert!(input.exists());
    }

    #[test]
    fn single_sheet_uses_converted_name_and_keeps_cell_positions() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("one.xlsx");
        xlsx_fixture(
            &input,
            &[
                ("Data", &|ws| {
                    ws.write(1, 1, "b2").unwrap();
                    ws.write(2, 3, 7).unwrap();
                }),
                ("Empty", &|_| {}),
            ],
        );
        let out = run(TO_CSV, &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("one (converted).csv")]);
        assert_eq!(read(&out[0]), ",,,\r\n,b2,,\r\n,,,7\r\n");
    }

    #[test]
    fn workbook_with_only_empty_sheets_gives_one_empty_csv() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("nothing.xlsx");
        xlsx_fixture(&input, &[("A", &|_| {}), ("B", &|_| {})]);
        let out = run(TO_CSV, &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("nothing (converted).csv")]);
        assert_eq!(read(&out[0]), "");
    }

    #[test]
    fn sheet_csvs_never_overwrite_and_names_are_file_safe() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("r.xlsx");
        let one = |ws: &mut rust_xlsxwriter::Worksheet| {
            ws.write(0, 0, 1).unwrap();
        };
        // Excel forbids most path characters in sheet names, but not `<>|"` or a trailing dot.
        xlsx_fixture(&input, &[("A<B>", &one), ("Sales.", &one)]);
        std::fs::write(d.path().join("r - A_B_.csv"), "keep").unwrap();

        let out = run(TO_CSV, &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("r - A_B_ (2).csv"), d.path().join("r - Sales.csv")]);
        assert_eq!(read(&d.path().join("r - A_B_.csv")), "keep");
    }

    #[test]
    fn reads_xls() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("legacy.xls");
        xls_fixture(
            &input,
            &[
                ("People", vec![
                    (0, 0, XlsCell::Text("Name")),
                    (0, 1, XlsCell::Text("Age")),
                    (1, 0, XlsCell::Text("Zoë, Jr.")),
                    (1, 1, XlsCell::Number(41.0)),
                ]),
                ("Totals", vec![(0, 0, XlsCell::Number(2.25))]),
            ],
        );
        let out = run(TO_CSV, &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("legacy - People.csv"), d.path().join("legacy - Totals.csv")]);
        assert_eq!(read(&out[0]), "Name,Age\r\n\"Zoë, Jr.\",41\r\n");
        assert_eq!(read(&out[1]), "2.25\r\n");
    }

    #[test]
    fn reads_ods() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("cities.ods");
        let cell = |v: &str| format!(r#"<table:table-cell office:value-type="string"><text:p>{v}</text:p></table:table-cell>"#);
        let num = |v: f64| format!(r#"<table:table-cell office:value-type="float" office:value="{v}"><text:p>{v}</text:p></table:table-cell>"#);
        let row = |cells: &[String]| format!("<table:table-row>{}</table:table-row>", cells.concat());
        ods_fixture(
            &input,
            &format!(
                r#"<table:table table:name="Cities">{}{}</table:table>
                   <table:table table:name="Empty"><table:table-row><table:table-cell/></table:table-row></table:table>"#,
                row(&[cell("City"), cell("Population")]),
                row(&[cell("Zürich"), num(421878.0)]),
            ),
        );
        let out = run(TO_CSV, &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("cities (converted).csv")]);
        assert_eq!(read(&out[0]), "City,Population\r\nZürich,421878\r\n");
    }

    #[test]
    fn date_and_number_text() {
        let dt = |v: f64| datetime_text(&ExcelDateTime::new(v, calamine::ExcelDateTimeType::DateTime, false));
        assert_eq!(dt(0.75), "18:00:00");
        assert_eq!(dt(1.0), "1900-01-01");
        assert_eq!(dt(45306.25), "2024-01-15 06:00:00");
        let dur = ExcelDateTime::new(-0.5, calamine::ExcelDateTimeType::TimeDelta, false);
        assert_eq!(datetime_text(&dur), "-12:00:00");
        assert_eq!(cell_text(&Data::Float(3.0)), "3");
        assert_eq!(cell_text(&Data::Error(calamine::CellErrorType::Div0)), "#DIV/0!");
        assert_eq!(cell_text(&Data::Empty), "");
    }

    // ---- CSV -> XLSX ------------------------------------------------------------------

    #[test]
    fn csv_to_xlsx_handles_bom_quotes_and_ragged_rows() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("people.csv");
        let csv = "\u{feff}Name,Zip,Score,Note\r\n\
                   \"Doe, Jane\",02134,98.5,\"said \"\"ok\"\"\nnext line\"\n\
                   Bob,90210,-3\n\
                   ,,,\n\
                   Ünïcødé,+44,1e5,12345678901234567\n";
        std::fs::write(&input, csv).unwrap();

        let out = run(TO_XLSX, &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("people (converted).xlsx")]);
        let sheets = read_xlsx(&out[0]);
        assert_eq!(sheets.len(), 1);
        let (name, range) = &sheets[0];
        assert_eq!(name, "people");
        let at = |r, c| range.get_value((r, c)).cloned().unwrap_or(Data::Empty);
        let s = |v: &str| Data::String(v.into());

        assert_eq!(at(0, 0), s("Name"), "BOM must not leak into the first cell");
        assert_eq!(at(0, 3), s("Note"));
        assert_eq!(at(1, 0), s("Doe, Jane"));
        assert_eq!(at(1, 1), s("02134"), "leading zero keeps it text");
        assert_eq!(at(1, 2), Data::Float(98.5));
        assert_eq!(at(1, 3), s("said \"ok\"\nnext line"));
        assert_eq!(at(2, 1), Data::Float(90210.0));
        assert_eq!(at(2, 2), Data::Float(-3.0));
        assert_eq!(at(2, 3), Data::Empty, "short row");
        assert_eq!(at(3, 0), Data::Empty, "blank row");
        assert_eq!(at(4, 0), s("Ünïcødé"));
        assert_eq!(at(4, 1), s("+44"));
        assert_eq!(at(4, 2), s("1e5"));
        assert_eq!(at(4, 3), s("12345678901234567"), "too precise for a number");
    }

    #[test]
    fn csv_to_xlsx_bolds_the_header_and_fits_columns() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("w.csv");
        std::fs::write(&input, "id,description\n1,a much much longer piece of text\n").unwrap();
        let out = run(TO_XLSX, &[input]).unwrap();

        let styles = zip_entry(&out[0], "xl/styles.xml");
        assert!(styles.contains("<b/>"), "a bold font is defined");
        let sheet = zip_entry(&out[0], "xl/worksheets/sheet1.xml");
        let style_of = |cell: &str| {
            let start = sheet.find(&format!("<c r=\"{cell}\"")).unwrap();
            let tag = &sheet[start..start + sheet[start..].find('>').unwrap()];
            tag.contains(" s=\"")
        };
        assert!(style_of("A1") && style_of("B1"), "header cells are styled");
        assert!(!style_of("A2") && !style_of("B2"), "body cells are not");

        let widths: Vec<f64> = sheet
            .match_indices("width=\"")
            .map(|(i, m)| {
                let rest = &sheet[i + m.len()..];
                rest[..rest.find('"').unwrap()].parse().unwrap()
            })
            .collect();
        assert_eq!(widths.len(), 2, "both columns have a fitted width: {sheet}");
        assert!(widths[1] > widths[0] * 2.0, "{widths:?}");
    }

    #[test]
    fn empty_csv_gives_an_empty_workbook() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("empty.csv");
        std::fs::write(&input, "").unwrap();
        let out = run(TO_XLSX, std::slice::from_ref(&input)).unwrap();
        let sheets = read_xlsx(&out[0]);
        assert_eq!(sheets.len(), 1);
        assert!(sheets[0].1.is_empty());

        std::fs::write(&input, "\u{feff}").unwrap(); // BOM only
        let out = run(TO_XLSX, &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("empty (converted 2).xlsx")]);
        assert!(read_xlsx(&out[0])[0].1.is_empty());
    }

    #[test]
    fn round_trip_csv_xlsx_csv() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("rt.csv");
        let original = "a,b\r\n\"x, y\",1.25\r\n\"multi\nline\",007\r\n";
        std::fs::write(&input, original).unwrap();
        let xlsx = run(TO_XLSX, &[input]).unwrap();
        let back = run(TO_CSV, &xlsx).unwrap();
        assert_eq!(back, vec![d.path().join("rt (converted) (converted).csv")]);
        assert_eq!(read(&back[0]), original);
    }

    #[test]
    fn delimiters_from_id_or_options() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("eu.csv");
        std::fs::write(&input, "name;price\n\"a;b\";1.5\n").unwrap();
        let xlsx = run("sheet.to_xlsx.semicolon", std::slice::from_ref(&input)).unwrap();
        let (_, range) = read_xlsx(&xlsx[0]).remove(0);
        assert_eq!(range.get_value((1, 0)), Some(&Data::String("a;b".into())));
        assert_eq!(range.get_value((1, 1)), Some(&Data::Float(1.5)));

        let tsv = run("sheet.to_csv.tab", std::slice::from_ref(&xlsx[0])).unwrap();
        assert_eq!(read(&tsv[0]), "name\tprice\r\na;b\t1.5\r\n");
        // `options` wins over the id; either a name or the character itself.
        let semi = run_with("sheet.to_csv.tab", std::slice::from_ref(&xlsx[0]), &[("delimiter", ";")]).unwrap();
        assert_eq!(read(&semi[0]), "name;price\r\n\"a;b\";1.5\r\n");
        let comma = run_with("sheet.to_csv", &xlsx, &[("delimiter", "comma")]).unwrap();
        assert_eq!(read(&comma[0]), "name,price\r\na;b,1.5\r\n");

        for (id, opts) in [("sheet.to_csv.pipe", vec![]), ("sheet.to_csv", vec![("delimiter", "|")])] {
            let err = run_with(id, &xlsx, &opts).unwrap_err();
            assert!(matches!(err, EngineError::InvalidOption(_)), "{id}: {err}");
        }
        assert!(matches!(run("sheet.to_csvx", &xlsx), Err(EngineError::UnknownOperation(_))));
    }

    #[test]
    fn sheet_names_follow_excel_rules() {
        assert_eq!(sheet_name("Q1/Q2 [draft]"), "Q1_Q2 _draft_");
        assert_eq!(sheet_name("'quoted'"), "quoted");
        assert_eq!(sheet_name(&"x".repeat(40)).len(), 31);
        assert_eq!(sheet_name("  "), "Sheet1");
    }

    #[test]
    fn number_detection_is_conservative() {
        for (s, n) in [("0", 0.0), ("42", 42.0), ("-3.5", -3.5), ("0.25", 0.25), ("123456789012345", 123456789012345.0)] {
            assert_eq!(as_number(s), Some(n), "{s}");
        }
        for s in ["", "-", "007", "+1", "1e3", "1,000", "1.", ".5", "NaN", "inf", " 1", "1234567890123456"] {
            assert_eq!(as_number(s), None, "{s:?}");
        }
    }

    // ---- errors -----------------------------------------------------------------------

    #[test]
    fn bad_inputs_are_errors_not_panics() {
        let d = tempfile::tempdir().unwrap();
        let fake = d.path().join("fake.xlsx");
        std::fs::write(&fake, b"definitely not a zip").unwrap();
        assert!(matches!(run(TO_CSV, &[fake]), Err(EngineError::Spreadsheet(_))));

        let latin1 = d.path().join("latin1.csv");
        std::fs::write(&latin1, b"caf\xe9\n").unwrap();
        assert!(matches!(run(TO_XLSX, &[latin1]), Err(EngineError::Csv(_))));
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 2, "no partial outputs");

        let missing = d.path().join("missing.csv");
        assert!(matches!(run(TO_XLSX, std::slice::from_ref(&missing)), Err(EngineError::InputNotFound(_))));
        assert!(matches!(run(TO_CSV, &[missing]), Err(EngineError::InputNotFound(_))));
    }

    #[test]
    fn handles_multiple_inputs() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.csv");
        let b = d.path().join("b.csv");
        std::fs::write(&a, "x\n1\n").unwrap();
        std::fs::write(&b, "y\n2\n").unwrap();
        let out = run(TO_XLSX, &[a, b]).unwrap();
        assert_eq!(out, vec![d.path().join("a (converted).xlsx"), d.path().join("b (converted).xlsx")]);
    }

    #[test]
    fn operations_per_mime_and_foreign_ops() {
        let ids = |m: &str| SheetConverter.operations(m).into_iter().map(|o| o.id).collect::<Vec<_>>();
        for m in [XLSX, XLS, ODS] {
            assert_eq!(ids(m), [TO_CSV, TO_JSON, SPLIT_SHEETS, TO_HTML]);
        }
        assert_eq!(ids(CSV), [TO_XLSX, TO_JSON, CHANGE_DELIMITER, DEDUPE, SORT, SPLIT_ROWS]);
        let kids = |m: &str, i: usize| SheetConverter.operations(m)[i].children.iter().map(|o| o.id.clone()).collect::<Vec<_>>();
        assert_eq!(kids(XLSX, 0), ["sheet.to_csv.comma", "sheet.to_csv.semicolon", "sheet.to_csv.tab"]);
        assert_eq!(kids(CSV, 0), ["sheet.to_xlsx.comma", "sheet.to_xlsx.semicolon", "sheet.to_xlsx.tab"]);
        assert_eq!(kids(CSV, 1), ["sheet.to_json.comma", "sheet.to_json.semicolon", "sheet.to_json.tab"]);
        assert_eq!(kids(CSV, 2), ["sheet.change_delimiter.comma", "sheet.change_delimiter.semicolon", "sheet.change_delimiter.tab"]);
        assert_eq!(kids(CSV, 4), ["sheet.sort.asc", "sheet.sort.desc"]);
        assert_eq!(kids(CSV, 5), ["sheet.split_rows.100", "sheet.split_rows.1000", "sheet.split_rows.10000"]);
        assert!(kids(XLSX, 1).is_empty());
        assert!(!SheetConverter.can_handle("image/png"));
        let err = run("image.convert.png", &[]).unwrap_err();
        assert!(matches!(err, EngineError::UnknownOperation(_)));
    }

    // ---- JSON -------------------------------------------------------------------------

    fn json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&read(path)).unwrap()
    }

    #[test]
    fn csv_to_json_types_keys_and_ragged_rows() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("people.csv");
        std::fs::write(&input, "\u{feff}name,age,zip,,name\r\nAnn,31,007,x,dup\r\nBob\r\n\"Cy, Jr\",-2.5,,,\r\n").unwrap();
        let out = run("sheet.to_json", std::slice::from_ref(&input)).unwrap().remove(0);
        assert_eq!(out, d.path().join("people (converted).json"));
        let v = json(&out);
        assert_eq!(v[0], serde_json::json!({"name": "Ann", "age": 31, "zip": "007", "column 4": "x", "name (2)": "dup"}));
        assert_eq!(v[1], serde_json::json!({"name": "Bob", "age": null, "zip": null, "column 4": null, "name (2)": null}));
        assert_eq!(v[2]["name"], "Cy, Jr");
        assert_eq!(v[2]["age"], -2.5);
        assert_eq!(v[2]["zip"], "", "an empty field is an empty string");
        let text = read(&out);
        assert!(text.find("\"age\"").unwrap() < text.find("\"zip\"").unwrap(), "keys keep column order");

        let semi = d.path().join("semi.csv");
        std::fs::write(&semi, "a;b\n1;2\n").unwrap();
        let v = json(&run("sheet.to_json.semicolon", &[semi]).unwrap()[0]);
        assert_eq!(v, serde_json::json!([{"a": 1, "b": 2}]));

        let empty = d.path().join("empty.csv");
        std::fs::write(&empty, "").unwrap();
        assert_eq!(json(&run("sheet.to_json", &[empty]).unwrap()[0]), serde_json::json!([]));
    }

    #[test]
    fn workbook_to_json_keeps_cell_types_and_sheets() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("book.xlsx");
        let people: Fill = &|ws| {
            ws.write(0, 0, "Name").unwrap();
            ws.write(0, 1, "Score").unwrap();
            ws.write(0, 2, "Active").unwrap();
            ws.write(1, 0, "Ann").unwrap();
            ws.write(1, 1, 2.0).unwrap();
            ws.write(1, 2, true).unwrap();
            ws.write(3, 0, "Bob").unwrap(); // row 3 empty in between: skipped
            ws.write(3, 1, 1.5).unwrap();
        };
        let other: Fill = &|ws| {
            ws.write(0, 0, "x").unwrap();
            ws.write(1, 0, "y").unwrap();
        };
        let blank: Fill = &|_| {};
        xlsx_fixture(&path, &[("People", people), ("Blank", blank), ("Other", other)]);
        let v = json(&run("sheet.to_json", &[path]).unwrap()[0]);
        assert_eq!(
            v,
            serde_json::json!({
                "People": [{"Name": "Ann", "Score": 2, "Active": true}, {"Name": "Bob", "Score": 1.5, "Active": null}],
                "Other": [{"x": "y"}],
            })
        );

        let single = d.path().join("single.xlsx");
        xlsx_fixture(&single, &[("Only", other)]);
        assert_eq!(json(&run("sheet.to_json", &[single]).unwrap()[0]), serde_json::json!([{"x": "y"}]));
    }

    // ---- dedupe / split ---------------------------------------------------------------

    #[test]
    fn dedupe_keeps_first_rows_byte_for_byte() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("list.csv");
        std::fs::write(&input, "\u{feff}a,b\r\n1,2\r\n3,4\r\n\"1\",2\r\n1,2\r\n\"x,y\",5\r\n3,4").unwrap();
        let out = run("sheet.dedupe", &[input]).unwrap().remove(0);
        assert_eq!(out, d.path().join("list (deduped).csv"));
        assert_eq!(read(&out), "\u{feff}a,b\r\n1,2\r\n3,4\r\n\"x,y\",5\r\n");

        let semi = d.path().join("semi.csv");
        std::fs::write(&semi, "a;b\n1;2\n1;2\n1;3\n").unwrap();
        assert_eq!(read(&run("sheet.dedupe", &[semi]).unwrap()[0]), "a;b\n1;2\n1;3\n", "delimiter sniffed");
    }

    #[test]
    fn split_rows_repeats_the_header_in_every_part() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("big.csv");
        std::fs::write(&input, "id,v\n1,a\n2,b\n3,\"multi\nline\"\n4,d\n5,e").unwrap();
        let out = run("sheet.split_rows.2", std::slice::from_ref(&input)).unwrap();
        let names: Vec<_> = (1..=3).map(|n| d.path().join(format!("big (part {n}).csv"))).collect();
        assert_eq!(out, names);
        assert_eq!(read(&out[0]), "id,v\n1,a\n2,b\n");
        assert_eq!(read(&out[1]), "id,v\n3,\"multi\nline\"\n4,d\n");
        assert_eq!(read(&out[2]), "id,v\n5,e");
        let out = run_with("sheet.split_rows", &[input], &[("rows", "10")]).unwrap();
        assert_eq!(out.len(), 1);

        let header_only = d.path().join("h.csv");
        std::fs::write(&header_only, "id,v").unwrap();
        assert!(matches!(run("sheet.split_rows.2", &[header_only]), Err(EngineError::Empty(_))));
        for bad in ["sheet.split_rows.0", "sheet.split_rows.x", "sheet.split_rows"] {
            assert!(matches!(run(bad, &[d.path().join("big.csv")]), Err(EngineError::InvalidOption(_))), "{bad}");
        }
    }

    #[test]
    fn delimiter_sniffing() {
        let none = Options::new();
        assert_eq!(sniffed_delimiter(b"a;b;c\n1,2", &none).unwrap(), b';');
        assert_eq!(sniffed_delimiter(b"a\tb\n", &none).unwrap(), b'\t');
        assert_eq!(sniffed_delimiter(b"\"x;y;z\",b\n", &none).unwrap(), b',', "quoted text doesn't count");
        assert_eq!(sniffed_delimiter(b"single", &none).unwrap(), b',');
        let semi = Options::from([("delimiter".to_string(), "semicolon".to_string())]);
        assert_eq!(sniffed_delimiter(b"a,b", &semi).unwrap(), b';', "the option wins");
    }

    // ---- sort / change delimiter ------------------------------------------------------

    #[test]
    fn sort_orders_numbers_text_and_empties() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("s.csv");
        std::fs::write(&input, "\u{feff}k,v\r\nbanana,1\r\n10,2\r\n,3\r\nApple,4\r\n9,5\r\n\"multi\nline\",6\r\napple,7").unwrap();
        let asc = run("sheet.sort.asc", std::slice::from_ref(&input)).unwrap().remove(0);
        assert_eq!(asc, d.path().join("s (sorted).csv"));
        assert_eq!(read(&asc), "\u{feff}k,v\r\n9,5\r\n10,2\r\nApple,4\r\napple,7\r\nbanana,1\r\n\"multi\nline\",6\r\n,3\r\n");
        let desc = run("sheet.sort.desc", std::slice::from_ref(&input)).unwrap().remove(0);
        assert_eq!(read(&desc), "\u{feff}k,v\r\n\"multi\nline\",6\r\nbanana,1\r\napple,7\r\nApple,4\r\n10,2\r\n9,5\r\n,3\r\n");
    }

    #[test]
    fn sort_column_by_name_or_number() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("p.csv");
        std::fs::write(&input, "name;age\nAnn;31\nBob;007\nCy;-2.5\n").unwrap();
        for col in ["age", "AGE", "2"] {
            let out = run_with("sheet.sort.asc", std::slice::from_ref(&input), &[("column", col)]).unwrap().remove(0);
            assert_eq!(read(&out), "name;age\nCy;-2.5\nBob;007\nAnn;31\n", "{col}");
        }
        let out = run_with("sheet.sort", std::slice::from_ref(&input), &[("order", "desc")]).unwrap().remove(0);
        assert_eq!(read(&out), "name;age\nCy;-2.5\nBob;007\nAnn;31\n");
        for (k, v) in [("column", "3"), ("column", "height"), ("order", "up")] {
            assert!(matches!(run_with("sheet.sort", std::slice::from_ref(&input), &[(k, v)]), Err(EngineError::InvalidOption(_))), "{v}");
        }
        let header_only = d.path().join("h.csv");
        std::fs::write(&header_only, "a,b\n").unwrap();
        assert!(matches!(run("sheet.sort.asc", &[header_only]), Err(EngineError::Empty(_))));
    }

    #[test]
    fn change_delimiter_requotes_and_keeps_bom_and_line_endings() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("c.csv");
        std::fs::write(&input, "\u{feff}a,b\r\n\"x,y\",z;w\r\n\"q \"\"t\"\"\",\r\n").unwrap();
        let out = run("sheet.change_delimiter.semicolon", std::slice::from_ref(&input)).unwrap().remove(0);
        assert_eq!(out, d.path().join("c (semicolon).csv"));
        assert_eq!(read(&out), "\u{feff}a;b\r\nx,y;\"z;w\"\r\n\"q \"\"t\"\"\";\r\n");

        let lf = d.path().join("lf.csv");
        std::fs::write(&lf, "a;b\n1;2\n").unwrap();
        let out = run("sheet.change_delimiter.tab", std::slice::from_ref(&lf)).unwrap().remove(0);
        assert_eq!(read(&out), "a\tb\n1\t2\n");
        let out = run_with("sheet.change_delimiter", std::slice::from_ref(&lf), &[("delimiter", ","), ("source_delimiter", "comma")]);
        assert!(matches!(out, Err(EngineError::InvalidOption(_))), "same as the (forced) source");
        assert!(matches!(run("sheet.change_delimiter.semicolon", &[lf]), Err(EngineError::InvalidOption(_))), "already semicolons");
    }

    // ---- split sheets / HTML ----------------------------------------------------------

    #[test]
    fn split_sheets_writes_one_xlsx_per_sheet() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("Book.xlsx");
        let date = Format::new().set_num_format("yyyy-mm-dd");
        xlsx_fixture(
            &input,
            &[
                ("Q1", &|ws| {
                    ws.write(0, 0, "Item").unwrap();
                    ws.write(1, 0, "Rent").unwrap();
                    ws.write(1, 1, 1200.5).unwrap();
                    ws.write(2, 2, true).unwrap();
                    ws.write_number_with_format(3, 0, 45306.0, &date).unwrap();
                }),
                ("Blank", &|_| {}),
                ("Q2", &|ws| {
                    ws.write(2, 3, "late").unwrap();
                }),
            ],
        );
        let out = run(SPLIT_SHEETS, &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("Book - Q1.xlsx"), d.path().join("Book - Q2.xlsx")]);
        let (name, q1) = read_xlsx(&out[0]).remove(0);
        assert_eq!(name, "Q1");
        assert_eq!(q1.get_value((1, 1)), Some(&Data::Float(1200.5)));
        assert_eq!(q1.get_value((2, 2)), Some(&Data::Bool(true)));
        assert!(matches!(q1.get_value((3, 0)), Some(Data::DateTime(dt)) if dt.as_f64() == 45306.0), "dates stay dates");
        let (_, q2) = read_xlsx(&out[1]).remove(0);
        assert_eq!(q2.start(), Some((2, 3)), "cells keep their position");
        assert_eq!(q2.get_value((2, 3)), Some(&Data::String("late".into())));

        let single = d.path().join("single.xlsx");
        xlsx_fixture(&single, &[("Only", &|ws| {
            ws.write(0, 0, 1).unwrap();
        })]);
        assert!(matches!(run(SPLIT_SHEETS, &[single]), Err(EngineError::Empty(_))));
    }

    #[test]
    fn split_sheets_reads_xls() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("old.xls");
        xls_fixture(&input, &[("A", vec![(0, 0, XlsCell::Text("x"))]), ("B", vec![(0, 0, XlsCell::Number(2.0))])]);
        let out = run(SPLIT_SHEETS, &[input]).unwrap();
        assert_eq!(out, vec![d.path().join("old - A.xlsx"), d.path().join("old - B.xlsx")]);
        assert_eq!(read_xlsx(&out[1])[0].1.get_value((0, 0)), Some(&Data::Float(2.0)));
    }

    #[test]
    fn workbook_to_html_has_a_table_per_sheet() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("r&d.xlsx");
        xlsx_fixture(
            &input,
            &[
                ("Costs <2024>", &|ws| {
                    ws.write(0, 0, "Item").unwrap();
                    ws.write(0, 1, "Cost").unwrap();
                    ws.write(1, 0, "A & \"B\"\nnext").unwrap();
                    ws.write(1, 1, 3.5).unwrap();
                }),
                ("Notes", &|ws| {
                    ws.write(0, 0, "just a header").unwrap();
                }),
            ],
        );
        let out = run(TO_HTML, &[input]).unwrap().remove(0);
        assert_eq!(out, d.path().join("r&d (converted).html"));
        let html = read(&out);
        assert!(html.starts_with("<!DOCTYPE html>") && html.contains("<title>r&amp;d</title>"));
        assert!(html.contains("<h2>Costs &lt;2024&gt;</h2>") && html.contains("<h2>Notes</h2>"));
        assert!(html.contains("<thead>\n<tr><th>Item</th><th>Cost</th></tr>\n</thead>"));
        assert!(html.contains("<tr><td>A &amp; &quot;B&quot;<br>next</td><td class=\"n\">3.5</td></tr>"));
        assert_eq!(html.matches("<table>").count(), 2);
        assert_eq!(html.matches("<tbody>").count(), html.matches("</tbody>").count());

        let ods = d.path().join("one.ods");
        ods_fixture(&ods, r#"<table:table table:name="S"><table:table-row><table:table-cell office:value-type="string"><text:p>h</text:p></table:table-cell></table:table-row></table:table>"#);
        let html = read(&run(TO_HTML, &[ods]).unwrap()[0]);
        assert!(!html.contains("<h2>"), "one sheet: no heading");

        let empty = d.path().join("empty.xlsx");
        xlsx_fixture(&empty, &[("A", &|_| {})]);
        assert!(matches!(run(TO_HTML, &[empty]), Err(EngineError::Empty(_))));
    }
}
