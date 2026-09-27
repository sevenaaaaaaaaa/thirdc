#!/usr/bin/env python3
"""ThirdC 文档解析器：PDF / DOCX / XLSX / PPTX → 纯文本（供 Rust 内核调用）"""
import sys, json

def extract_pdf(path):
    import fitz
    doc = fitz.open(path)
    pages = []
    for page in doc:
        pages.append(page.get_text())
    return "\n\n".join(pages)

def extract_docx(path):
    from docx import Document
    doc = Document(path)
    parts = []
    for para in doc.paragraphs:
        t = para.text.strip()
        if not t: continue
        style = para.style.name.lower() if para.style else ""
        if "heading" in style:
            level = "##"
            try: n = int(style.replace("heading","").strip())
            except: n = 2
            level = "#" * max(2, min(n, 4))
            parts.append(f"{level} {t}")
        else:
            parts.append(t)
    for table in doc.tables:
        rows = []
        for row in table.rows:
            cells = [c.text.strip().replace("|","\\|") for c in row.cells]
            rows.append("| " + " | ".join(cells) + " |")
        if rows:
            parts.append("\n".join(rows))
    return "\n\n".join(parts)

MAX_ROWS_PER_SHEET = 500

def extract_xlsx(path):
    import openpyxl
    wb = openpyxl.load_workbook(path, data_only=True, read_only=True)
    parts = []
    for ws in wb.worksheets:
        parts.append(f"## {ws.title}")
        rows_written = 0
        for i, row in enumerate(ws.iter_rows(values_only=True)):
            if i >= MAX_ROWS_PER_SHEET:
                total = ws.max_row or i
                parts.append(f"（仅前 {MAX_ROWS_PER_SHEET} 行，全表共 {total} 行）")
                break
            cells = ["" if c is None else str(c).replace("|", "\\|").replace("\n", " ").strip() for c in row]
            if not any(cells):
                continue
            rows_written += 1
            parts.append("| " + " | ".join(cells) + " |")
            if rows_written == 1:
                parts.append("|" + "---|" * len(cells))
        parts.append("")
    return "\n".join(parts)

MAX_SLIDES = 300

def extract_pptx(path):
    from pptx import Presentation
    prs = Presentation(path)
    parts = []
    slides = list(prs.slides)[:MAX_SLIDES]
    for i, slide in enumerate(slides, 1):
        title = ""
        try:
            if slide.shapes.title is not None:
                title = slide.shapes.title.text.strip()
        except Exception:
            title = ""
        parts.append(f"## Slide {i}" + (f"：{title}" if title else ""))
        for shape in slide.shapes:
            if getattr(shape, "has_text_frame", False):
                for para in shape.text_frame.paragraphs:
                    t = "".join(run.text for run in para.runs).strip()
                    if t and t != title:
                        parts.append(("- " if (para.level or 0) == 0 else "  - ") + t)
        try:
            if slide.has_notes_slide:
                notes = slide.notes_slide.notes_text_frame.text.strip()
                if notes:
                    parts.append("")
                    parts.append(f"> 备注：{notes}")
        except Exception:
            pass
        parts.append("")
    return "\n".join(parts)

if __name__ == "__main__":
    path = sys.argv[1]
    ext = path.rsplit(".",1)[-1].lower()
    try:
        if ext == "pdf": text = extract_pdf(path)
        elif ext == "docx": text = extract_docx(path)
        elif ext == "xlsx": text = extract_xlsx(path)
        elif ext == "pptx": text = extract_pptx(path)
        else: text = open(path, encoding="utf-8", errors="replace").read()
        print(json.dumps({"ok": True, "text": text}))
    except Exception as e:
        print(json.dumps({"ok": False, "error": str(e)}))
