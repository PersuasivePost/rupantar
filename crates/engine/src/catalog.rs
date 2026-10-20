//! The menu catalog: for each kind of file, every top-level operation the radial menu offers,
//! with its label, icon, priority and group. This is the only place those are defined.
//!
//! - Converters declare what they implement (ids, sub-options, availability) and take labels
//!   and icons from here via [`op`]; sub-option labels stay with the converter.
//! - `Registry::operations` lists a type's menu in priority order (lower first). Entries no
//!   converter implements yet are listed with `available: false`, so the menu hides them.
//! - The frontend shows the first five visible operations as wedges and the rest under "More".
//!
//! Selection context: `multi_priority` replaces `priority` when two or more files are
//! selected (Merge and Images → PDF move to the front).

use serde::{Deserialize, Serialize};

use crate::mime::{CSV, DOC, DOCX, HTML, MARKDOWN, ODP, ODS, ODT, PDF, PPT, PPTX, XLS, XLSX};
use crate::Operation;

/// What an operation does, for grouping in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    Convert,
    Optimize,
    Pages,
    Edit,
    Extract,
    Data,
    Security,
    File,
}

use Group::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub id: &'static str,
    pub label: &'static str,
    /// lucide-react icon name.
    pub icon: &'static str,
    /// Menu position, lower first.
    pub priority: u16,
    pub group: Group,
    /// Priority when two or more files are selected; `None` keeps `priority`.
    pub multi_priority: Option<u16>,
    /// Comes from a converter's `batch_operations` (only for some selections), so it is never
    /// listed as a planned per-file operation.
    pub batch: bool,
    /// The mime this writes; not offered for files that already are that type.
    pub produces: Option<&'static str>,
}

const fn e(id: &'static str, label: &'static str, icon: &'static str, priority: u16, group: Group) -> Entry {
    Entry { id, label, icon, priority, group, multi_priority: None, batch: false, produces: None }
}

impl Entry {
    const fn multi(mut self, priority: u16) -> Self {
        self.multi_priority = Some(priority);
        self
    }

    const fn batch(mut self) -> Self {
        self.batch = true;
        self
    }

    const fn produces(mut self, mime: &'static str) -> Self {
        self.produces = Some(mime);
        self
    }

    /// The operation with this entry's label, icon and group (no priority, no children).
    pub fn operation(&self) -> Operation {
        let mut op = Operation::new(self.id, self.label, self.icon);
        op.group = Some(self.group);
        op
    }

    /// Priority for a selection of `count` files.
    pub fn priority_for(&self, count: usize) -> u16 {
        match self.multi_priority {
            Some(p) if count > 1 => p,
            _ => self.priority,
        }
    }
}

const DUPLICATE: Entry = e("copy", "Duplicate", "Copy", 900, File);

pub const PDF_MENU: &[Entry] = &[
    e("pdf.merge", "Merge", "Merge", 5, Pages).multi(0).batch(),
    e("pdf.compress", "Compress", "Minimize2", 10, Optimize),
    e("pdf.to_docx", "To Word (text)", "FileType", 20, Convert),
    e("pdf.to_image", "To image", "FileImage", 30, Convert),
    e("pdf.split", "Split", "Scissors", 40, Pages),
    e("pdf.rotate", "Rotate", "RotateCw", 50, Pages),
    e("pdf.extract", "Extract", "FileOutput", 60, Pages),
    e("pdf.to_pptx", "To PowerPoint", "Presentation", 70, Convert),
    e("pdf.to_xlsx", "To Excel", "FileSpreadsheet", 80, Convert),
    e("pdf.to_text", "To text", "Text", 90, Extract),
    e("pdf.watermark", "Watermark", "Stamp", 100, Edit),
    e("pdf.page_numbers", "Page numbers", "Hash", 110, Edit),
    e("pdf.security", "Protect / unlock", "Lock", 120, Security),
    e("pdf.ocr", "OCR", "ScanText", 130, Extract),
    DUPLICATE,
];

pub const IMAGE_MENU: &[Entry] = &[
    e("image.compress", "Compress", "Minimize2", 10, Optimize),
    e("image.resize", "Resize", "Maximize2", 20, Optimize),
    e("image.convert", "Convert", "FileImage", 30, Convert),
    e("image.crop", "Crop", "Crop", 40, Edit),
    e("image.remove_bg", "Remove background", "WandSparkles", 50, Edit),
    e("image.rotate", "Rotate / flip", "RotateCw", 60, Edit),
    e("image.grayscale", "Grayscale", "Contrast", 65, Edit),
    e("image.upscale", "Upscale", "ZoomIn", 70, Optimize),
    e("pdf.from_images", "To PDF", "FileText", 80, Convert).multi(0).batch(),
    e("image.edit", "Edit", "SlidersHorizontal", 90, Edit),
    e("image.strip_metadata", "Strip metadata", "EyeOff", 110, Security),
    e("image.watermark", "Watermark", "Stamp", 120, Edit),
    DUPLICATE,
];

pub const WORD_MENU: &[Entry] = &[
    e("doc.to_pdf", "To PDF", "FileText", 10, Convert).produces(PDF),
    e("doc.to_md", "To Markdown", "FileCode", 20, Convert).produces(MARKDOWN),
    e("doc.to_html", "To HTML", "Globe", 30, Convert).produces(HTML),
    e("doc.compress", "Compress", "Minimize2", 40, Optimize),
    e("doc.extract_images", "Extract images", "Images", 50, Extract),
    e("doc.to_odt", "To ODT", "FilePen", 60, Convert).produces(ODT),
    e("doc.to_epub", "To EPUB", "BookOpen", 70, Convert),
    e("doc.to_text", "To text", "Text", 80, Extract),
    DUPLICATE,
];

pub const EXCEL_MENU: &[Entry] = &[
    e("sheet.to_csv", "To CSV", "Rows3", 10, Convert).produces(CSV),
    e("sheet.to_pdf", "To PDF", "FileText", 20, Convert).produces(PDF),
    e("sheet.to_json", "To JSON", "Braces", 30, Convert),
    e("sheet.split", "Split sheets", "Split", 40, Pages),
    e("sheet.compress", "Compress", "Minimize2", 50, Optimize),
    e("sheet.to_ods", "To ODS", "FileSpreadsheet", 60, Convert).produces(ODS),
    e("sheet.to_html", "To HTML", "Globe", 70, Convert).produces(HTML),
    DUPLICATE,
];

pub const POWERPOINT_MENU: &[Entry] = &[
    e("slides.to_pdf", "To PDF", "FileText", 10, Convert).produces(PDF),
    e("slides.to_images", "To images", "Images", 20, Convert),
    e("slides.extract_images", "Extract images", "ImageDown", 30, Extract),
    e("slides.compress", "Compress", "Minimize2", 40, Optimize),
    e("slides.to_text", "To text", "Text", 50, Extract),
    e("slides.to_odp", "To ODP", "Presentation", 60, Convert).produces(ODP),
    DUPLICATE,
];

pub const CSV_MENU: &[Entry] = &[
    e("sheet.to_xlsx", "To Excel", "FileSpreadsheet", 10, Convert).produces(XLSX),
    e("sheet.to_json", "To JSON", "Braces", 20, Convert),
    e("sheet.change_delimiter", "Change delimiter", "Columns3", 30, Data),
    e("sheet.to_pdf", "To PDF", "FileText", 40, Convert).produces(PDF),
    e("sheet.dedupe", "Dedupe", "CopyMinus", 50, Data),
    e("sheet.sort", "Sort", "ArrowDownAZ", 60, Data),
    e("sheet.split_rows", "Split by rows", "Rows3", 70, Pages),
    DUPLICATE,
];

/// Markdown and HTML share a menu; "To HTML" / "To Markdown" drops out for its own type.
pub const MARKUP_MENU: &[Entry] = &[
    e("doc.to_pdf", "To PDF", "FileText", 10, Convert).produces(PDF),
    e("doc.to_docx", "To Word", "FileType", 20, Convert).produces(DOCX),
    e("doc.to_html", "To HTML", "Globe", 30, Convert).produces(HTML),
    e("doc.to_md", "To Markdown", "FileCode", 30, Convert).produces(MARKDOWN),
    e("doc.to_image", "To image", "FileImage", 40, Convert),
    e("doc.to_slides", "To slides", "Presentation", 50, Convert),
    e("doc.to_epub", "To EPUB", "BookOpen", 60, Convert),
    e("doc.to_text", "To text", "Text", 70, Extract),
    DUPLICATE,
];

/// Every menu with the mimes it applies to.
pub const MENUS: &[(&[&str], &[Entry])] = &[
    (&[PDF], PDF_MENU),
    (&["image/png", "image/jpeg", "image/webp"], IMAGE_MENU),
    (&[DOCX, DOC, ODT], WORD_MENU),
    (&[XLSX, XLS, ODS], EXCEL_MENU),
    (&[PPTX, PPT, ODP], POWERPOINT_MENU),
    (&[CSV], CSV_MENU),
    (&[MARKDOWN, HTML], MARKUP_MENU),
];

/// The menu for a mime type; empty for types without one (they get whatever converters offer).
pub fn menu(mime: &str) -> &'static [Entry] {
    MENUS.iter().find(|(mimes, _)| mimes.contains(&mime)).map_or(&[], |(_, entries)| entries)
}

/// The per-file entries offered for `mime`, in priority order: batch-only entries and
/// conversions to the file's own type are left out.
pub fn entries_for(mime: &str) -> impl Iterator<Item = &'static Entry> + '_ {
    menu(mime).iter().filter(move |e| !e.batch && e.produces != Some(mime))
}

/// The catalog operation for `id` (label, icon, group). Converters build their top-level
/// operations with this. An id missing from the catalog gets itself as the label.
pub fn op(id: &str) -> Operation {
    MENUS
        .iter()
        .flat_map(|(_, entries)| entries.iter())
        .find(|e| e.id == id)
        .map_or_else(|| Operation::new(id, id, "Circle"), Entry::operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mime;
    use std::collections::HashMap;

    fn labels(mime: &str) -> Vec<&'static str> {
        entries_for(mime).map(|e| e.label).collect()
    }

    /// The target menus: five wedges, then "More" (Duplicate stays last everywhere).
    #[test]
    fn menus_match_the_targets() {
        let more = |mime: &str| labels(mime)[5..].to_vec();
        let top = |mime: &str| labels(mime)[..5].to_vec();

        assert_eq!(top(PDF), ["Compress", "To Word (text)", "To image", "Split", "Rotate"]);
        assert_eq!(
            more(PDF),
            ["Extract", "To PowerPoint", "To Excel", "To text", "Watermark", "Page numbers", "Protect / unlock", "OCR", "Duplicate"]
        );
        for png in ["image/png", "image/jpeg", "image/webp"] {
            assert_eq!(top(png), ["Compress", "Resize", "Convert", "Crop", "Remove background"]);
            // "To PDF" is a batch op: it joins the list in `Registry::operations_for`.
            assert_eq!(
                more(png),
                ["Rotate / flip", "Grayscale", "Upscale", "Edit", "Strip metadata", "Watermark", "Duplicate"]
            );
        }
        for word in [DOCX, DOC] {
            assert_eq!(top(word), ["To PDF", "To Markdown", "To HTML", "Compress", "Extract images"]);
            assert_eq!(more(word), ["To ODT", "To EPUB", "To text", "Duplicate"]);
        }
        assert_eq!(more(ODT), ["To EPUB", "To text", "Duplicate"], "no ODT -> ODT");
        for sheet in [XLSX, XLS] {
            assert_eq!(top(sheet), ["To CSV", "To PDF", "To JSON", "Split sheets", "Compress"]);
            assert_eq!(more(sheet), ["To ODS", "To HTML", "Duplicate"]);
        }
        assert_eq!(more(ODS), ["To HTML", "Duplicate"]);
        for deck in [PPTX, PPT] {
            assert_eq!(top(deck), ["To PDF", "To images", "Extract images", "Compress", "To text"]);
            assert_eq!(more(deck), ["To ODP", "Duplicate"]);
        }
        assert_eq!(more(ODP), ["Duplicate"]);
        assert_eq!(top(CSV), ["To Excel", "To JSON", "Change delimiter", "To PDF", "Dedupe"]);
        assert_eq!(more(CSV), ["Sort", "Split by rows", "Duplicate"]);
        assert_eq!(top(MARKDOWN), ["To PDF", "To Word", "To HTML", "To image", "To slides"]);
        assert_eq!(top(HTML), ["To PDF", "To Word", "To Markdown", "To image", "To slides"]);
        for markup in [MARKDOWN, HTML] {
            assert_eq!(more(markup), ["To EPUB", "To text", "Duplicate"]);
        }
        assert!(menu(mime::UNKNOWN).is_empty());
    }

    #[test]
    fn menus_are_in_priority_order_without_duplicates() {
        for (mimes, entries) in MENUS {
            assert!(entries.windows(2).all(|w| w[0].priority <= w[1].priority), "{mimes:?} out of order");
            let mut ids: Vec<_> = entries.iter().map(|e| e.id).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), entries.len(), "{mimes:?} repeats an id");
        }
    }

    /// `op(id)` takes the first entry with that id, so every menu must agree on its metadata.
    #[test]
    fn an_id_looks_the_same_in_every_menu() {
        let mut seen: HashMap<&str, (&str, &str, Group)> = HashMap::new();
        for (_, entries) in MENUS {
            for e in *entries {
                let meta = (e.label, e.icon, e.group);
                assert_eq!(*seen.entry(e.id).or_insert(meta), meta, "{} differs between menus", e.id);
            }
        }
    }

    #[test]
    fn selection_size_picks_the_priority() {
        let merge = PDF_MENU.iter().find(|e| e.id == "pdf.merge").unwrap();
        assert_eq!((merge.priority_for(1), merge.priority_for(2)), (5, 0));
        let split = PDF_MENU.iter().find(|e| e.id == "pdf.split").unwrap();
        assert_eq!((split.priority_for(1), split.priority_for(3)), (40, 40));
    }

    #[test]
    fn op_uses_the_catalog() {
        let o = op("pdf.split");
        assert_eq!((o.label.as_str(), o.icon.as_str(), o.group), ("Split", "Scissors", Some(Pages)));
        assert_eq!(o.priority, None, "priority is per menu; the registry sets it");
        assert_eq!(op("nope").label, "nope");
    }
}
