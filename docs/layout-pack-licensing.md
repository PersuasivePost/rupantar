# A "Layout pack" for PDF → Word: licensing note

Status: **not built; for a decision.** Written 2026-10-21. This is an engineering summary, not legal advice.

## What it would be for

PDF → Word "Keep layout" (LibreOffice's PDF import) keeps the page looking right, but every line becomes its own positioned text box. That's fine to read or print and awkward to edit. pdf2docx rebuilds flowing paragraphs, tables and images, so its DOCX is much easier to edit. A Layout pack would offer that as a third mode.

## Licences (checked 2026-10-21)

| Component | Licence | Notes |
|---|---|---|
| [pdf2docx](https://github.com/ArtifexSoftware/pdf2docx) | **MIT** (recently relicensed; older docs and releases say AGPL-3.0) | Artifex no longer maintains it actively. Pin a release whose LICENSE file says MIT. |
| [PyMuPDF](https://pypi.org/project/PyMuPDF/) (+ MuPDF) | **AGPL-3.0**, or a paid commercial licence from Artifex | Required by pdf2docx (`PyMuPDF>=1.26.7`), so the pack would be AGPL anyway. |
| python-docx, fonttools, numpy, opencv-python-headless, fire, a bundled CPython | MIT / BSD / Apache-2.0 / PSF | No problem. |

So a Layout pack would really be AGPL-3.0, because of PyMuPDF.

## What that means for an MIT app

- **Rupantar can stay MIT** if the pack runs as a **separate program**. The engine would start a bundled `pdf2docx` executable, pass it file paths, and read back the file it writes, the same way it runs pandoc (GPL) and LibreOffice today. Running a separate program at arm's length doesn't make Rupantar a derivative work. Loading MuPDF or PyMuPDF into Rupantar's own process would, and the combined program would then have to be AGPL.
- **The pack itself must follow the AGPL.** We'd ship the licence texts, offer the exact corresponding source for everything in the pack (our build script plus PyMuPDF/MuPDF source at the pinned versions, and we should mirror it rather than rely on upstream links), and keep the pack unmodified or publish our changes. AGPL §13 (the network clause) doesn't apply, because nothing is served over a network.
- **We already do something similar.** The Office pack's PDF import (`xpdfimport`) includes Poppler (GPL), and pandoc is GPL. A Layout pack would follow the same pattern: optional, downloaded only after consent, a separate process, with notices on the Licenses screen.
- **Cost of the pack:** an embedded Python, PyMuPDF, numpy and OpenCV come to roughly 150–250 MB per platform, built and pinned by us for each OS and architecture (there's no official all-in-one build like LibreOffice's). That's real build and maintenance work, and upstream pdf2docx is no longer actively maintained.
- **Other options:** buy a commercial PyMuPDF licence from Artifex (removes the AGPL obligations), or keep only LibreOffice's "Keep layout" and the current "Editable text".

## Decision needed

1. Do we want a third PDF → Word mode (editable *and* laid out) enough to build and maintain an AGPL pack per platform?
2. If yes: AGPL pack running out of process (free, with the source-distribution duties above), or a commercial PyMuPDF licence?
