# PdfCraft — scan editing, release candidate 1 (`scan-edit-rc1`)

This is a test build of the PdfCraft fork that adds **editing text in scanned pages**. You can
correct a word or a line of a scanned page so that it looks as if it had been printed that way.
The new letters are real letters of the same page, and no pixel outside the edited line changes.

It is a **release candidate**: please try it on your own scans and report what goes wrong. Your
documents stay on your computer: nothing is uploaded.

## ⚠️ Unsigned builds

These builds are **not code-signed**.

- **Windows**: SmartScreen will show "Windows protected your PC" the first time you run
  `pdfcraft.exe` from the portable zip (or the MSI). Click **More info**, then **Run anyway**.
  Only do this for files you downloaded from this release page.
- **Linux**: make the AppImage executable (`chmod +x pdfcraft-*.AppImage`) and run it. If FUSE
  is missing, run it with `--appimage-extract-and-run`.

## What's new

- **Edit text & images** on a scanned page frames the lines you can edit. Click one, type the
  correction, and the page shows the real result in place. **Show original** compares it with
  the original, and **Apply** makes the edit (Undo works as usual).
- Pages without a text layer offer **Recognize text** in one click (OCR runs locally; models
  included).
- Works with JPEG, JPEG 2000, black-and-white fax/MFP scans (CCITT, JBIG2), lossless,
  indexed and CMYK scans, scans in strips, and rotated pages.
- French accents (é è ê à â î ï ô û ù ü…) are composed from a letter and an accent found
  elsewhere on the page, even when the OCR didn't read them.
- The panel shows where the letters come from, what had to be adjusted, and **how much bigger
  the scan gets** before you apply.
- When an edit would look wrong, it is refused with an explanation and a hint. Examples:
  letters the page doesn't have, text that doesn't fit, a page recognised upside down, text
  scanned too small.

## Good to know

- The edited scan is stored **without loss**. For JPEG scans this makes the file bigger:
  about ×1.7 for grey and ×6 for colour photos. The panel shows the size before you apply.
- A document with an edited scan is saved in full, so the previous image (and the old text) is
  no longer in the file.
- One line at a time; paragraphs are not reflowed yet. Handwriting, light text on dark
  backgrounds and strongly curved lines aren't edited.

Measured on real documents (office JPEG, MFP CCITT/JBIG2, phone photo, colour book scans,
multi-page, French): 76–92 % of lines editable on pages with correct text recognition, and zero
pixels changed outside the edited line. See `docs/scan-edit/RC-REPORT.md` and
`docs/scan-edit/USER-GUIDE.md` in the source.

## Downloads

| File | For |
|---|---|
| `pdfcraft-*-windows-x64-portable.zip` | Windows 10/11 x64: unzip anywhere and run `pdfcraft.exe` |
| `pdfcraft-*-windows-x64.msi` | Windows installer |
| `pdfcraft-*-linux-x86_64.AppImage` | Linux x86_64 (glibc 2.35+) |
| `pdfcraft-*-linux-x86_64.tar.gz` | Linux, unpacked |

OCR models: ocrs text detection and recognition models (Robert Knight, CC BY-SA 4.0), shipped
with their attribution files.
