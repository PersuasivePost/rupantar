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

use std::io::Write;
use std::path::{Path, PathBuf};

use calamine::{open_workbook_auto, Data, ExcelDateTime, Range, Reader, SheetType};
use rust_xlsxwriter::{Format, Workbook};

use crate::mime::{CSV, ODS, XLS, XLSX};
use crate::{create_unique, create_unique_sibling, Converter, EngineError, Operation, Options, Result};

pub struct SheetConverter;

const TO_CSV: &str = "sheet.to_csv";
const TO_XLSX: &str = "sheet.to_xlsx";
/// `(id suffix, byte, label, lucide icon)`.
const DELIMITERS: [(&str, u8, &str, &str); 3] = [
    ("comma", b',', "Comma", "Columns3"),
    ("semicolon", b';', "Semicolon", "Columns3"),
    ("tab", b'\t', "Tab", "ArrowRightToLine"),
];

/// `base` + one child per delimiter.
fn with_delimiters(base: &str, label: &str, icon: &str) -> Operation {
    let children = DELIMITERS
        .iter()
        .map(|(suffix, _, label, icon)| Operation::new(&format!("{base}.{suffix}"), label, icon))
        .collect();
    Operation::new(base, label, icon).with_children(children)
}

/// The delimiter for an op id under `base`: from `options`, else the id suffix, else a comma.
fn delimiter(id: &str, base: &str, options: &Options) -> Result<u8> {
    let suffix = id.strip_prefix(base).and_then(|r| r.strip_prefix('.'));
    let Some(raw) = options.get("delimiter").map(String::as_str).or(suffix) else { return Ok(b',') };
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

/// Creates every output or none: files written before a failure are removed.
fn write_all_or_nothing(outputs: Vec<(std::fs::File, PathBuf, Vec<u8>)>) -> Result<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = outputs.iter().map(|(_, p, _)| p.clone()).collect();
    for (mut file, _, bytes) in outputs {
        if let Err(e) = file.write_all(&bytes) {
            for p in &paths {
                let _ = std::fs::remove_file(p);
            }
            return Err(e.into());
        }
    }
    Ok(paths)
}

fn workbook_to_csv(input: &Path, delimiter: u8) -> Result<Vec<PathBuf>> {
    if !input.is_file() {
        return Err(EngineError::InputNotFound(input.to_path_buf()));
    }
    let mut book = open_workbook_auto(input)?;
    let names: Vec<String> = book
        .sheets_metadata()
        .iter()
        .filter(|s| s.typ == SheetType::WorkSheet)
        .map(|s| s.name.clone())
        .collect();
    let mut sheets = Vec::new();
    for name in names {
        let range = book.worksheet_range(&name)?;
        if !range.is_empty() {
            sheets.push((name, range_to_csv(&range, delimiter)?));
        }
    }

    let mut outputs = Vec::new();
    if sheets.len() <= 1 {
        let bytes = sheets.pop().map(|(_, b)| b).unwrap_or_default();
        let (file, path) = create_unique_sibling(input, "converted", Some("csv"))?;
        outputs.push((file, path, bytes));
    } else {
        let dir = input.parent().unwrap_or_else(|| Path::new(""));
        let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
        for (i, (name, bytes)) in sheets.into_iter().enumerate() {
            let sheet = match file_safe(&name) {
                s if s.is_empty() => format!("Sheet{}", i + 1),
                s => s,
            };
            let created = create_unique(dir, |n| match n {
                1 => format!("{stem} - {sheet}.csv"),
                n => format!("{stem} - {sheet} ({n}).csv"),
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
    }
    write_all_or_nothing(outputs)
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

impl Converter for SheetConverter {
    fn can_handle(&self, mime: &str) -> bool {
        is_workbook(mime) || mime == CSV
    }

    fn operations(&self, mime: &str) -> Vec<Operation> {
        if is_workbook(mime) {
            vec![with_delimiters(TO_CSV, "CSV", "FileText")]
        } else if mime == CSV {
            vec![with_delimiters(TO_XLSX, "XLSX", "FileSpreadsheet")]
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
        } else {
            Err(EngineError::UnknownOperation(op.id.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{open_workbook, Xlsx};
    use std::io::Read;

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

        let out = run(TO_CSV, &[input.clone()]).unwrap();
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
        let out = run(TO_XLSX, &[input.clone()]).unwrap();
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
        assert!(matches!(run(TO_XLSX, &[missing.clone()]), Err(EngineError::InputNotFound(_))));
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
        assert_eq!(ids(XLSX), [TO_CSV]);
        assert_eq!(ids(XLS), [TO_CSV]);
        assert_eq!(ids(ODS), [TO_CSV]);
        assert_eq!(ids(CSV), [TO_XLSX]);
        let kids = |m: &str| SheetConverter.operations(m)[0].children.iter().map(|o| o.id.clone()).collect::<Vec<_>>();
        assert_eq!(kids(XLSX), ["sheet.to_csv.comma", "sheet.to_csv.semicolon", "sheet.to_csv.tab"]);
        assert_eq!(kids(CSV), ["sheet.to_xlsx.comma", "sheet.to_xlsx.semicolon", "sheet.to_xlsx.tab"]);
        assert!(!SheetConverter.can_handle("image/png"));
        let err = run("image.convert.png", &[]).unwrap_err();
        assert!(matches!(err, EngineError::UnknownOperation(_)));
    }
}
