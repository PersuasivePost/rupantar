// Third-party software Rupantar ships or downloads, for Settings → Licenses. License texts are
// bundled (src/licenses/); notice files that ship next to a component are read from disk.
import apache2 from "./licenses/Apache-2.0.txt?raw";
import gpl2 from "./licenses/GPL-2.0.txt?raw";
import mpl2 from "./licenses/MPL-2.0.txt?raw";
import pandocCopyright from "./licenses/pandoc-COPYRIGHT.txt?raw";
import pdfiumBinaries from "./licenses/pdfium-LICENSE.txt?raw";

export interface ThirdParty {
  name: string;
  version: string;
  /** SPDX expression. */
  license: string;
  url: string;
  /** What Rupantar uses it for, and how it is shipped. */
  use: string;
  note?: string;
  texts: { title: string; text: string }[];
  /** Notice files read from disk (`thirdPartyNotices`). */
  notices?: "pdfium" | "office";
}

export const THIRD_PARTY: ThirdParty[] = [
  {
    name: "pandoc",
    version: "3.12",
    license: "GPL-2.0-or-later",
    url: "https://pandoc.org",
    use: "Document conversions (Markdown, Word, HTML, EPUB). Bundled as a separate, unmodified program.",
    note: "Source code: https://github.com/jgm/pandoc/tree/3.12",
    texts: [
      { title: "Copyright", text: pandocCopyright },
      { title: "GNU General Public License, version 2", text: gpl2 },
    ],
  },
  {
    name: "Typst",
    version: "0.15.1",
    license: "Apache-2.0",
    url: "https://typst.app",
    use: "Typesetting PDFs made from documents. Bundled as a separate, unmodified program.",
    note: "Source code: https://github.com/typst/typst/tree/v0.15.1",
    texts: [{ title: "Apache License 2.0", text: apache2 }],
  },
  {
    name: "PDFium",
    version: "chromium/7881",
    license: "BSD-3-Clause AND Apache-2.0 AND others",
    url: "https://pdfium.googlesource.com/pdfium/",
    use: "Rendering PDF pages and reading their text. Bundled as a library, built by the pdfium-binaries project (MIT).",
    texts: [{ title: "pdfium-binaries (MIT)", text: pdfiumBinaries }],
    notices: "pdfium",
  },
  {
    name: "LibreOffice",
    version: "26.2.6",
    license: "MPL-2.0",
    url: "https://www.libreoffice.org",
    use: "Word, Excel and PowerPoint conversions. Only present if you install the Office pack.",
    note:
      "The Office pack is an official, unmodified LibreOffice build downloaded from documentfoundation.org, " +
      "only after you agree. It installs its own license file and third-party notices. " +
      "Source code: https://www.libreoffice.org/about-us/source-code/",
    texts: [{ title: "Mozilla Public License 2.0", text: mpl2 }],
    notices: "office",
  },
];
