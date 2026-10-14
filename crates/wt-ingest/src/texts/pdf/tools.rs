//! The PDF tools (Spec 08, PDFs and OCR): `pdfinfo`, `pdftotext` and `pdftoppm` from
//! poppler-utils, and Tesseract 5, run as subprocesses behind a trait so tests can use them or
//! not. `WT_POPPLER_BIN` names poppler's directory and `WT_TESSERACT` Tesseract's program when
//! they are not on the `PATH`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::super::TextError;
use super::layout::{self, Page};

/// A rectangle of a rendered page, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

/// What the importer needs from the PDF tools.
pub trait PdfTools: Send + Sync {
    /// The number of pages.
    fn page_count(&self, pdf: &Path) -> Result<u32, TextError>;
    /// One page's text layer, with geometry.
    fn text_layer(&self, pdf: &Path, page: u32) -> Result<Page, TextError>;
    /// Renders one page, or part of it, to a PNG at `out`.
    fn render(
        &self,
        pdf: &Path,
        page: u32,
        dpi: u32,
        crop: Option<Crop>,
        out: &Path,
    ) -> Result<(), TextError>;
    /// Reads a page image with Tesseract (`--oem 1`, automatic page segmentation, TSV).
    fn ocr(&self, png: &Path, languages: &[String], page: u32) -> Result<Page, TextError>;
    /// The OCR engine and its version, for `text_edition.ocr_engine`.
    fn engine(&self) -> Result<String, TextError>;
}

/// The real tools.
#[derive(Debug, Clone)]
pub struct Poppler {
    bin: Option<PathBuf>,
    tesseract: PathBuf,
}

/// Runs a tool to completion and returns its stdout; a tool that will not start or exits nonzero is
/// an error naming `what` and its stderr.
fn run(cmd: &mut Command, what: &str) -> Result<Vec<u8>, TextError> {
    let out = cmd
        .output()
        .map_err(|e| TextError::Tool(format!("{what} could not start: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(TextError::Tool(format!(
            "{what} failed ({}): {}",
            out.status,
            stderr.lines().take(3).collect::<Vec<_>>().join(" ")
        )));
    }
    Ok(out.stdout)
}

impl Poppler {
    /// The tools named by `WT_POPPLER_BIN` and `WT_TESSERACT`, or on the `PATH`.
    pub fn from_env() -> Self {
        Self {
            bin: std::env::var_os("WT_POPPLER_BIN").map(PathBuf::from),
            tesseract: std::env::var_os("WT_TESSERACT")
                .map_or_else(|| PathBuf::from("tesseract"), PathBuf::from),
        }
    }

    /// A poppler command, from `WT_POPPLER_BIN` when set, else from the path.
    fn poppler(&self, name: &str) -> Command {
        Command::new(
            self.bin
                .as_ref()
                .map_or_else(|| PathBuf::from(name), |b| b.join(name)),
        )
    }

    /// Whether poppler's `pdftotext` and Tesseract both answer. xpdf also ships a `pdftotext`,
    /// without `-bbox-layout`, so the version text must name Poppler.
    pub fn available(&self) -> bool {
        let poppler = self.poppler("pdftotext").arg("-v").output().is_ok_and(|o| {
            String::from_utf8_lossy(&o.stderr).contains("Poppler")
                || String::from_utf8_lossy(&o.stdout).contains("Poppler")
        });
        poppler
            && Command::new(&self.tesseract)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
    }
}

impl PdfTools for Poppler {
    fn page_count(&self, pdf: &Path) -> Result<u32, TextError> {
        let out = run(self.poppler("pdfinfo").arg(pdf), "pdfinfo")?;
        String::from_utf8_lossy(&out)
            .lines()
            .find_map(|l| l.strip_prefix("Pages:").and_then(|n| n.trim().parse().ok()))
            .ok_or_else(|| TextError::Tool("pdfinfo printed no page count".into()))
    }

    fn text_layer(&self, pdf: &Path, page: u32) -> Result<Page, TextError> {
        let n = page.to_string();
        let out = run(
            self.poppler("pdftotext")
                .args(["-f", &n, "-l", &n, "-bbox-layout", "-enc", "UTF-8"])
                .arg(pdf)
                .arg("-"),
            "pdftotext",
        )?;
        layout::from_bbox_layout(&String::from_utf8_lossy(&out), page)
    }

    fn render(
        &self,
        pdf: &Path,
        page: u32,
        dpi: u32,
        crop: Option<Crop>,
        out: &Path,
    ) -> Result<(), TextError> {
        let n = page.to_string();
        let mut cmd = self.poppler("pdftoppm");
        cmd.args([
            "-png",
            "-singlefile",
            "-f",
            &n,
            "-l",
            &n,
            "-r",
            &dpi.to_string(),
        ]);
        if let Some(c) = crop {
            cmd.args([
                "-x",
                &c.x.to_string(),
                "-y",
                &c.y.to_string(),
                "-W",
                &c.w.to_string(),
                "-H",
                &c.h.to_string(),
            ]);
        }
        // pdftoppm adds `.png` to the root it is given.
        let root = out.with_extension("");
        run(cmd.arg(pdf).arg(&root), "pdftoppm")?;
        Ok(())
    }

    fn ocr(&self, png: &Path, languages: &[String], page: u32) -> Result<Page, TextError> {
        let langs = if languages.is_empty() {
            "eng".to_owned()
        } else {
            languages.join("+")
        };
        let out = run(
            Command::new(&self.tesseract)
                .arg(png)
                .arg("stdout")
                .args(["--oem", "1", "--psm", "3", "-l", &langs, "tsv"]),
            "tesseract",
        )?;
        layout::from_tesseract_tsv(&String::from_utf8_lossy(&out), page)
    }

    fn engine(&self) -> Result<String, TextError> {
        let out = Command::new(&self.tesseract)
            .arg("--version")
            .output()
            .map_err(|e| TextError::Tool(format!("tesseract could not start: {e}")))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(text
            .lines()
            .next()
            .map_or_else(|| "tesseract".to_owned(), |l| l.trim().to_owned()))
    }
}
