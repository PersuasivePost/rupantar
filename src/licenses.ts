// Third-party software Rupantar ships or downloads, for Settings → Licenses. License texts are
// bundled (src/licenses/); notice files that ship next to a component are read from disk.
import apache2 from "./licenses/Apache-2.0.txt?raw";
import birefnet from "./licenses/BiRefNet-LICENSE.txt?raw";
import gpl2 from "./licenses/GPL-2.0.txt?raw";
import mpl2 from "./licenses/MPL-2.0.txt?raw";
import ncnn from "./licenses/ncnn-LICENSE.txt?raw";
import onnxruntime from "./licenses/onnxruntime-LICENSE.txt?raw";
import pandocCopyright from "./licenses/pandoc-COPYRIGHT.txt?raw";
import pdfiumBinaries from "./licenses/pdfium-LICENSE.txt?raw";
import realEsrgan from "./licenses/Real-ESRGAN-LICENSE.txt?raw";
import realEsrganNcnn from "./licenses/Real-ESRGAN-ncnn-vulkan-LICENSE.txt?raw";

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
  notices?: "pdfium" | "office" | "ai";
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
    use: "Word, Excel and PowerPoint conversions, and PDF to Word keeping the layout. Only present if you install the Office pack.",
    note:
      "The Office pack is an official, unmodified LibreOffice build downloaded from documentfoundation.org, " +
      "only after you agree. It installs its own license file and third-party notices (its PDF import " +
      "includes Poppler, GPL). " +
      "Source code: https://www.libreoffice.org/about-us/source-code/",
    texts: [{ title: "Mozilla Public License 2.0", text: mpl2 }],
    notices: "office",
  },
  {
    name: "ONNX Runtime",
    version: "1.28.3",
    license: "MIT",
    url: "https://onnxruntime.ai",
    use: "Runs the background-removal model. Only present if you install the AI pack.",
    note:
      "The AI pack contains an official, unmodified Microsoft build: 1.28.3 on Linux and Apple silicon, 1.23.2 on " +
      "Intel Macs (the last with an Intel build), and the DirectML build 1.24.4 with DirectML 1.15.4 on Windows " +
      "(Microsoft’s DirectML license is among its notice files). It installs its own third-party notices. " +
      "Source code: https://github.com/microsoft/onnxruntime",
    texts: [{ title: "MIT License", text: onnxruntime }],
    notices: "ai",
  },
  {
    name: "BiRefNet lite",
    version: "general, epoch 232",
    license: "MIT",
    url: "https://github.com/ZhengPeng7/BiRefNet",
    use: "The background-removal model (BiRefNet-general-bb_swin_v1_tiny, ONNX). Only present if you install the AI pack.",
    note: "Downloaded unmodified from the author’s GitHub release v1.",
    texts: [{ title: "MIT License", text: birefnet }],
  },
  {
    name: "Real-ESRGAN",
    version: "0.2.5.0",
    license: "BSD-3-Clause AND MIT",
    url: "https://github.com/xinntao/Real-ESRGAN",
    use: "Upscaling (the realesrgan-x4plus model, run by realesrgan-ncnn-vulkan). Only present if you install the AI pack.",
    note:
      "The AI pack contains the unmodified release build: the Real-ESRGAN model (BSD-3-Clause), the " +
      "realesrgan-ncnn-vulkan program (MIT, based on realsr-ncnn-vulkan) and the ncnn library it is built on " +
      "(Tencent, BSD-3-Clause). Source code: https://github.com/xinntao/Real-ESRGAN-ncnn-vulkan",
    texts: [
      { title: "Real-ESRGAN (BSD 3-Clause)", text: realEsrgan },
      { title: "Real-ESRGAN-ncnn-vulkan (MIT)", text: realEsrganNcnn },
      { title: "ncnn (BSD 3-Clause and others)", text: ncnn },
    ],
  },
];
