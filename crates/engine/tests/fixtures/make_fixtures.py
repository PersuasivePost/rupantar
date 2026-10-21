#!/usr/bin/env python3
"""Builds the layout-fidelity corpus in this folder (the outputs are committed; rerun only to
change them).

    python3 -I make_fixtures.py <path to soffice or soffice.bin>

- letterhead.docx: written here as raw WordprocessingML, the way Word lays out a letter: a
  header with a logo and a full-width band behind the text, a floating photo (square wrap), a
  tinted box behind the subject line, a bordered table with a shaded header row, and a
  signature with a stamp floating over it.
- letterhead.pdf: letterhead.docx exported by LibreOffice (the PDF -> Word source).
- deck.pptx: three slides with shapes, an image and bullets, from flat ODF via LibreOffice.
- styled.xlsx: fills, borders, bold header, number formats, a merged title (values, no
  formulas), from flat ODF via
  LibreOffice.

Needs Pillow for the images. LibreOffice runs with a throwaway profile.
"""

import base64
import io
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent
EMU = 914400  # per inch


def png(img):
    buf = io.BytesIO()
    img.save(buf, "PNG", optimize=True)
    return buf.getvalue()


def logo():
    img = Image.new("RGB", (360, 140), "#1c1d21")
    d = ImageDraw.Draw(img)
    d.pieslice((20, 10, 140, 130), 0, 300, fill="#ece5d6")
    d.pieslice((32, 10, 152, 130), 300, 360, fill="#d9663a")
    d.rectangle((170, 40, 340, 62), fill="#ece5d6")
    d.rectangle((170, 78, 300, 96), fill="#d9663a")
    return png(img)


def photo():
    img = Image.new("RGB", (480, 320))
    d = ImageDraw.Draw(img)
    for y in range(320):
        d.line((0, y, 480, y), fill=(40 + y // 3, 90 + y // 4, 160 - y // 4))
    d.ellipse((300, 30, 400, 130), fill="#f4d35e")
    d.polygon([(0, 320), (160, 140), (300, 320)], fill="#2d5016")
    d.polygon([(180, 320), (340, 170), (480, 320)], fill="#3e6b20")
    return png(img)


def signature():
    img = Image.new("RGBA", (440, 150), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    pts = [(10, 110), (50, 40), (80, 120), (120, 30), (150, 115), (190, 60), (230, 100), (270, 45), (320, 105), (420, 70)]
    d.line(pts, fill=(20, 40, 120, 255), width=6, joint="curve")
    d.line([(30, 135), (400, 125)], fill=(20, 40, 120, 255), width=3)
    return png(img)


def stamp():
    img = Image.new("RGBA", (260, 260), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    red = (200, 30, 40, 200)
    d.ellipse((8, 8, 252, 252), outline=red, width=12)
    d.ellipse((40, 40, 220, 220), outline=red, width=5)
    d.rectangle((30, 108, 230, 152), fill=red)
    return png(img)


# ---- letterhead.docx ----------------------------------------------------------------------

NS = (
    'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" '
    'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" '
    'xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" '
    'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" '
    'xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" '
    'xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" '
    'xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"'
)

_ids = iter(range(1, 1000))


def anchor(graphic, cx, cy, x, y, rel_h, rel_v, wrap="<wp:wrapNone/>", behind=False, z=1):
    n = next(_ids)
    return f"""<w:r><w:drawing><wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" relativeHeight="{251658240 + z}" behindDoc="{1 if behind else 0}" locked="0" layoutInCell="1" allowOverlap="1">
<wp:simplePos x="0" y="0"/>
<wp:positionH relativeFrom="{rel_h}"><wp:posOffset>{x}</wp:posOffset></wp:positionH>
<wp:positionV relativeFrom="{rel_v}"><wp:posOffset>{y}</wp:posOffset></wp:positionV>
<wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>{wrap}
<wp:docPr id="{n}" name="Object {n}"/><wp:cNvGraphicFramePr/>
<a:graphic>{graphic(n, cx, cy)}</a:graphic></wp:anchor></w:drawing></w:r>"""


def inline(graphic, cx, cy):
    n = next(_ids)
    return f"""<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/>
<wp:docPr id="{n}" name="Object {n}"/><wp:cNvGraphicFramePr/><a:graphic>{graphic(n, cx, cy)}</a:graphic></wp:inline></w:drawing></w:r>"""


def picture(rid):
    return lambda n, cx, cy: f"""<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>
<pic:nvPicPr><pic:cNvPr id="{n}" name="image{n}.png"/><pic:cNvPicPr/></pic:nvPicPr>
<pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>
<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData>"""


def shape(fill, line=None, geom="rect"):
    ln = f'<a:ln w="19050"><a:solidFill><a:srgbClr val="{line}"/></a:solidFill></a:ln>' if line else "<a:ln><a:noFill/></a:ln>"
    return lambda n, cx, cy: f"""<a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:cNvSpPr/>
<wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="{geom}"><a:avLst/></a:prstGeom>
<a:solidFill><a:srgbClr val="{fill}"/></a:solidFill>{ln}</wps:spPr><wps:bodyPr/></wps:wsp></a:graphicData>"""


def para(text="", *, runs="", bold=False, size=None, color=None, align=None, after=120, before=0):
    rpr = ""
    if bold:
        rpr += "<w:b/>"
    if color:
        rpr += f'<w:color w:val="{color}"/>'
    if size:
        rpr += f'<w:sz w:val="{size * 2}"/>'
    ppr = f'<w:spacing w:before="{before}" w:after="{after}"/>'
    if align:
        ppr += f'<w:jc w:val="{align}"/>'
    t = f'<w:r><w:rPr>{rpr}</w:rPr><w:t xml:space="preserve">{text}</w:t></w:r>' if text else ""
    return f"<w:p><w:pPr>{ppr}</w:pPr>{runs}{t}</w:p>"


def table(rows):
    border = '<w:{0} w:val="single" w:sz="6" w:space="0" w:color="555555"/>'
    borders = "".join(border.format(s) for s in ["top", "left", "bottom", "right", "insideH", "insideV"])
    widths = [3600, 1800, 1800, 1826]
    grid = "".join(f'<w:gridCol w:w="{w}"/>' for w in widths)
    out = f'<w:tbl><w:tblPr><w:tblW w:w="9026" w:type="dxa"/><w:tblBorders>{borders}</w:tblBorders></w:tblPr><w:tblGrid>{grid}</w:tblGrid>'
    for i, row in enumerate(rows):
        out += "<w:tr>"
        for w, cell in zip(widths, row):
            shade = '<w:shd w:val="clear" w:color="auto" w:fill="1C1D21"/>' if i == 0 else ""
            color = "ECE5D6" if i == 0 else None
            align = "right" if cell[:1].isdigit() or cell[:1] == "€" else None
            out += f'<w:tc><w:tcPr><w:tcW w:w="{w}" w:type="dxa"/>{shade}</w:tcPr>{para(cell, bold=i == 0, color=color, align=align, after=40, before=40)}</w:tc>'
        out += "</w:tr>"
    return out + "</w:tbl>"


LOREM = (
    "Thank you for your order of 14 October. We are pleased to confirm that the items listed below "
    "have been reserved for you and will leave our warehouse within five working days. Our "
    "engineers have inspected every unit, and the test certificates are enclosed with this letter."
)
LOREM2 = (
    "Should you need the delivery split across several sites, please let us know before Friday so "
    "that we can arrange separate pallets. Payment is due thirty days after delivery; bank details "
    "are printed at the foot of the invoice that accompanies the goods."
)


def letterhead_docx(path):
    inch = EMU
    header = (
        f"<w:hdr {NS}>"
        + para(
            runs=anchor(shape("1C1D21"), int(8.27 * inch), int(1.25 * inch), 0, 0, "page", "page", behind=True, z=0)
            + inline(picture("rIdLogo"), int(1.6 * inch), int(0.62 * inch))
            + '<w:r><w:tab/></w:r>',
            after=0,
        )
        + para("ACME Industries Ltd · 12 Foundry Lane · Leeds LS1 4AB · +44 113 496 0000", size=8, color="ECE5D6", after=0)
        + "</w:hdr>"
    )
    footer = f"<w:ftr {NS}>" + para("Registered in England No. 01234567 · VAT GB 123 4567 89", size=8, color="777777", align="center") + "</w:ftr>"

    body = [
        para("21 October 2026", align="right", before=240),
        para("Ms R. Sharma"),
        para("Head of Procurement"),
        para("Northwind Traders", after=240),
        para(
            "Subject: Confirmation of order NW-2026-1187",
            bold=True,
            size=13,
            runs=anchor(shape("FBE3D6", "D9663A"), int(6.27 * inch), int(0.42 * inch), 0, -int(0.08 * inch), "margin", "paragraph", behind=True, z=2),
            after=240,
            before=120,
        ),
        para("Dear Ms Sharma,"),
        para(
            LOREM,
            runs=anchor(
                picture("rIdPhoto"),
                int(2.1 * inch),
                int(1.4 * inch),
                int(4.17 * inch),
                0,
                "margin",
                "paragraph",
                wrap='<wp:wrapSquare wrapText="bothSides"/>',
                z=3,
            ),
        ),
        para(LOREM2, after=240),
        table(
            [
                ["Item", "Quantity", "Unit price", "Total"],
                ["Hydraulic press HP-40", "2", "€ 18,400", "€ 36,800"],
                ["Spare seal kit", "6", "€ 240", "€ 1,440"],
                ["Commissioning (days)", "3", "€ 950", "€ 2,850"],
            ]
        ),
        para("", after=240),
        para("We look forward to working with you."),
        para("Yours sincerely,", after=0),
        para(
            runs=anchor(picture("rIdSignature"), int(2.0 * inch), int(0.68 * inch), 0, int(0.05 * inch), "margin", "paragraph", z=4)
            + anchor(picture("rIdStamp"), int(1.1 * inch), int(1.1 * inch), int(1.5 * inch), -int(0.15 * inch), "margin", "paragraph", z=5),
            after=0,
        ),
        para("", after=900),
        para("Daniel Okafor", bold=True, after=0),
        para("Sales Director, ACME Industries"),
    ]
    sect = (
        '<w:sectPr><w:headerReference w:type="default" r:id="rIdHeader"/><w:footerReference w:type="default" r:id="rIdFooter"/>'
        '<w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="2268" w:right="1440" w:bottom="1440" w:left="1440" w:header="340" w:footer="567" w:gutter="0"/></w:sectPr>'
    )
    document = f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {NS} mc:Ignorable=""><w:body>{"".join(body)}{sect}</w:body></w:document>'
    styles = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults>
<w:rPrDefault><w:rPr><w:rFonts w:ascii="Liberation Sans" w:hAnsi="Liberation Sans" w:cs="Liberation Sans"/><w:sz w:val="21"/><w:lang w:val="en-GB"/></w:rPr></w:rPrDefault>
<w:pPrDefault><w:pPr><w:spacing w:after="120" w:line="264" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style></w:styles>"""
    rel = '<Relationship Id="{0}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{1}" Target="{2}"/>'
    rels_head = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    doc_rels = rels_head + "".join(
        rel.format(*r)
        for r in [
            ("rIdStyles", "styles", "styles.xml"),
            ("rIdHeader", "header", "header1.xml"),
            ("rIdFooter", "footer", "footer1.xml"),
            ("rIdPhoto", "image", "media/photo.png"),
            ("rIdSignature", "image", "media/signature.png"),
            ("rIdStamp", "image", "media/stamp.png"),
        ]
    ) + "</Relationships>"
    header_rels = rels_head + rel.format("rIdLogo", "image", "media/logo.png") + "</Relationships>"
    root_rels = rels_head + rel.format("rId1", "officeDocument", "word/document.xml") + "</Relationships>"
    ct = "application/vnd.openxmlformats-officedocument.wordprocessingml"
    types = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/>'
        f'<Override PartName="/word/document.xml" ContentType="{ct}.document.main+xml"/>'
        f'<Override PartName="/word/styles.xml" ContentType="{ct}.styles+xml"/>'
        f'<Override PartName="/word/header1.xml" ContentType="{ct}.header+xml"/>'
        f'<Override PartName="/word/footer1.xml" ContentType="{ct}.footer+xml"/></Types>'
    )
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", types)
        z.writestr("_rels/.rels", root_rels)
        z.writestr("word/document.xml", document)
        z.writestr("word/styles.xml", styles)
        z.writestr("word/header1.xml", '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' + header)
        z.writestr("word/footer1.xml", '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>' + footer)
        z.writestr("word/_rels/document.xml.rels", doc_rels)
        z.writestr("word/_rels/header1.xml.rels", header_rels)
        z.writestr("word/media/logo.png", logo())
        z.writestr("word/media/photo.png", photo())
        z.writestr("word/media/signature.png", signature())
        z.writestr("word/media/stamp.png", stamp())


# ---- flat ODF sources for the PPTX and XLSX ----------------------------------------------

ODF_NS = (
    'xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
    'xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" '
    'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" '
    'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" '
    'xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" '
    'xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" '
    'xmlns:xlink="http://www.w3.org/1999/xlink" '
    'xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" '
    'xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" '
    'xmlns:number="urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0" '
    'office:version="1.3"'
)


def deck_fodp():
    pic = base64.b64encode(photo()).decode()

    def box(style, x, y, w, h):
        return f'<draw:custom-shape draw:style-name="{style}" svg:x="{x}cm" svg:y="{y}cm" svg:width="{w}cm" svg:height="{h}cm"><draw:enhanced-geometry draw:type="rectangle" svg:viewBox="0 0 21600 21600"/></draw:custom-shape>'

    def text(x, y, w, h, paras, size="20pt"):
        ps = "".join(f'<text:p text:style-name="P{size}">{p}</text:p>' for p in paras)
        return f'<draw:frame draw:style-name="plain" svg:x="{x}cm" svg:y="{y}cm" svg:width="{w}cm" svg:height="{h}cm"><draw:text-box>{ps}</draw:text-box></draw:frame>'

    slides = [
        box("dark", 0, 0, 28, 15.75)
        + box("accent", 0, 11.5, 28, 0.6)
        + text(2, 4, 24, 3, ["Quarterly review"], "44pt")
        + text(2, 7.2, 24, 2, ["ACME Industries · Q3 2026"], "24pt"),
        text(1.5, 0.8, 25, 2, ["Highlights"], "32pt")
        + text(1.5, 3.2, 13, 10, ["• Orders up 18% on Q2", "• Two new distributors in the Nordics", "• Press line HP-40 in production", "• Warranty claims down to 0.4%"], "20pt")
        + f'<draw:frame draw:style-name="plain" svg:x="15.5cm" svg:y="3.2cm" svg:width="11cm" svg:height="7.33cm"><draw:image><office:binary-data>{pic}</office:binary-data></draw:image></draw:frame>',
        text(1.5, 0.8, 25, 2, ["Revenue by region"], "32pt")
        + "".join(
            box("accent" if i % 2 else "dark", 3 + i * 5.5, 13 - h, 3.5, h) + text(2.6 + i * 5.5, 13.2, 4.5, 1.2, [name], "16pt")
            for i, (name, h) in enumerate([("North", 6), ("South", 8.5), ("East", 4.2), ("West", 7.1)])
        ),
    ]
    pages = "".join(f'<draw:page draw:name="s{i + 1}" draw:master-page-name="M">{s}</draw:page>' for i, s in enumerate(slides))
    para_styles = "".join(
        f'<style:style style:name="P{s}" style:family="paragraph"><style:text-properties fo:font-size="{s}" fo:font-family="Liberation Sans" fo:color="{"#ece5d6" if s in ("44pt", "24pt") else "#1c1d21"}"/></style:style>'
        for s in ["44pt", "32pt", "24pt", "20pt", "16pt"]
    )
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<office:document {ODF_NS} office:mimetype="application/vnd.oasis.opendocument.presentation">
<office:automatic-styles>{para_styles}
<style:page-layout style:name="PL"><style:page-layout-properties fo:page-width="28cm" fo:page-height="15.75cm" fo:margin-top="0cm" fo:margin-bottom="0cm" fo:margin-left="0cm" fo:margin-right="0cm" style:print-orientation="landscape"/></style:page-layout>
<style:style style:name="dark" style:family="graphic"><style:graphic-properties draw:fill="solid" draw:fill-color="#1c1d21" draw:stroke="none"/></style:style>
<style:style style:name="accent" style:family="graphic"><style:graphic-properties draw:fill="solid" draw:fill-color="#d9663a" draw:stroke="none"/></style:style>
<style:style style:name="plain" style:family="graphic"><style:graphic-properties draw:fill="none" draw:stroke="none"/></style:style>
</office:automatic-styles>
<office:master-styles><style:master-page style:name="M" style:page-layout-name="PL"/></office:master-styles>
<office:body><office:presentation>{pages}</office:presentation></office:body></office:document>"""


def styled_fods():
    rows = [
        ("Hydraulic press HP-40", 2, 18400.0, "2026-10-02"),
        ("Spare seal kit", 6, 240.0, "2026-10-03"),
        ("Commissioning (days)", 3, 950.0, "2026-10-09"),
        ("Pallet freight", 4, 310.5, "2026-10-12"),
        ("Extended warranty", 2, 1200.0, "2026-10-14"),
    ]

    def cell(style, value=None, kind="string"):
        if kind == "string":
            return f'<table:table-cell table:style-name="{style}" office:value-type="string"><text:p>{value}</text:p></table:table-cell>'
        if kind == "date":
            return f'<table:table-cell table:style-name="{style}" office:value-type="date" office:date-value="{value}"><text:p>{value}</text:p></table:table-cell>'
        return f'<table:table-cell table:style-name="{style}" office:value-type="float" office:value="{value}"><text:p>{value}</text:p></table:table-cell>'

    body = '<table:table-row table:style-name="tall"><table:table-cell table:style-name="title" table:number-columns-spanned="5" office:value-type="string"><text:p>Order NW-2026-1187</text:p></table:table-cell><table:covered-table-cell/><table:covered-table-cell/><table:covered-table-cell/><table:covered-table-cell/></table:table-row>'
    body += "<table:table-row>" + "".join(cell("head", h) for h in ["Item", "Qty", "Unit price", "Shipped", "Total"]) + "</table:table-row>"
    for i, (item, qty, price, date) in enumerate(rows):
        band = "odd" if i % 2 else "even"
        body += (
            "<table:table-row>"
            + cell(band, item)
            + cell(band + "n", qty, "float")
            + cell(band + "eur", price, "float")
            + cell(band + "date", date, "date")
            + cell(band + "eur", qty * price, "float")
            + "</table:table-row>"
        )
    total = sum(q * p for _, q, p, _ in rows)
    body += "<table:table-row>" + cell("totlabel", "Total") + '<table:table-cell table:style-name="tot"/>' * 3 + cell("toteur", total, "float") + "</table:table-row>"

    def cs(name, bg=None, bold=False, color=None, fmt=None, border=True, size=None, align=None):
        cp = f' fo:background-color="{bg}"' if bg else ""
        if border:
            cp += ' fo:border="0.5pt solid #555555"'
        tp = ' fo:font-weight="bold"' if bold else ""
        if color:
            tp += f' fo:color="{color}"'
        if size:
            tp += f' fo:font-size="{size}"'
        pp = f'<style:paragraph-properties fo:text-align="{align}"/>' if align else ""
        dn = f' style:data-style-name="{fmt}"' if fmt else ""
        return f'<style:style style:name="{name}" style:family="table-cell"{dn}><style:table-cell-properties{cp} style:vertical-align="middle"/>{pp}<style:text-properties fo:font-family="Liberation Sans"{tp}/></style:style>'

    styles = "".join(
        [
            cs("title", "#1c1d21", True, "#ece5d6", size="16pt", border=False),
            cs("head", "#d9663a", True, "#ffffff", align="center"),
            cs("even", "#ffffff"),
            cs("odd", "#f3efe6"),
            cs("evenn", "#ffffff", fmt="N0"),
            cs("oddn", "#f3efe6", fmt="N0"),
            cs("eveneur", "#ffffff", fmt="EUR"),
            cs("oddeur", "#f3efe6", fmt="EUR"),
            cs("evendate", "#ffffff", fmt="D"),
            cs("odddate", "#f3efe6", fmt="D"),
            cs("totlabel", "#ece5d6", True),
            cs("tot", "#ece5d6"),
            cs("toteur", "#ece5d6", True, fmt="EUR"),
        ]
    )
    widths = ["5.5cm", "1.5cm", "2.8cm", "3cm", "3cm"]
    cols = "".join(f'<style:style style:name="co{i}" style:family="table-column"><style:table-column-properties style:column-width="{w}"/></style:style>' for i, w in enumerate(widths))
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<office:document {ODF_NS} office:mimetype="application/vnd.oasis.opendocument.spreadsheet">
<office:automatic-styles>
<number:number-style style:name="N0"><number:number number:decimal-places="0" number:min-integer-digits="1"/></number:number-style>
<number:currency-style style:name="EUR"><number:currency-symbol>€</number:currency-symbol><number:text> </number:text><number:number number:decimal-places="2" number:min-integer-digits="1" number:grouping="true"/></number:currency-style>
<number:date-style style:name="D"><number:day number:style="long"/><number:text> </number:text><number:month number:textual="true"/><number:text> </number:text><number:year number:style="long"/></number:date-style>
{cols}
<style:style style:name="tall" style:family="table-row"><style:table-row-properties style:row-height="1.1cm"/></style:style>
{styles}
</office:automatic-styles>
<office:body><office:spreadsheet><table:table table:name="Order">
{"".join(f'<table:table-column table:style-name="co{i}"/>' for i in range(len(widths)))}
{body}
</table:table></office:spreadsheet></office:body></office:document>"""


def soffice_convert(soffice, src, filt, outdir):
    with tempfile.TemporaryDirectory() as profile:
        args = [soffice, "--headless", "--norestore", f"-env:UserInstallation=file://{profile}", "--convert-to", filt, "--outdir", str(outdir), str(src)]
        for _ in range(2):  # a fresh profile exits 81 once
            if subprocess.run(args, capture_output=True).returncode != 81:
                break


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    soffice = sys.argv[1]
    letterhead_docx(HERE / "letterhead.docx")
    with tempfile.TemporaryDirectory() as work:
        work = Path(work)
        (work / "deck.fodp").write_text(deck_fodp(), encoding="utf-8")
        (work / "styled.fods").write_text(styled_fods(), encoding="utf-8")
        soffice_convert(soffice, work / "deck.fodp", "pptx:Impress MS PowerPoint 2007 XML", work)
        soffice_convert(soffice, work / "styled.fods", "xlsx:Calc MS Excel 2007 XML", work)
        soffice_convert(soffice, HERE / "letterhead.docx", "pdf:writer_pdf_Export", work)
        for name in ["deck.pptx", "styled.xlsx", "letterhead.pdf"]:
            shutil.copyfile(work / name, HERE / name)
            print(HERE / name, os.path.getsize(HERE / name))


if __name__ == "__main__":
    main()
