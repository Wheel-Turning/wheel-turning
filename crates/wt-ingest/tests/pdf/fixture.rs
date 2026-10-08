//! Synthetic PDFs for the PDF tests: a text-layer PDF written by hand in Helvetica, and an
//! image-only "scan" made by rendering it with `pdftoppm` and planting noise. Nothing here is a
//! real work.

#![allow(dead_code)]

use std::io::Write as _;
use std::path::Path;
use std::process::Command;

/// A line of text on a page: left edge and top edge in points from the top left, type size.
#[derive(Debug, Clone)]
pub struct TextLine {
    pub x: f32,
    pub top: f32,
    pub size: f32,
    pub text: String,
}

pub fn line(x: f32, top: f32, size: f32, text: &str) -> TextLine {
    TextLine {
        x,
        top,
        size,
        text: text.to_owned(),
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

/// Assembles objects (1-based, in order) into a PDF with a cross-reference table.
fn assemble(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

/// One page of the PDF: text lines, or a grayscale image filling a US Letter page.
pub enum PageSpec {
    Text(Vec<TextLine>),
    Image {
        width: u32,
        height: u32,
        gray: Vec<u8>,
    },
}

/// Writes a PDF of US Letter pages.
pub fn pdf(pages: &[PageSpec]) -> Vec<u8> {
    // 1 catalog, 2 pages, 3 font, then per page: page, contents, and an image if any.
    let mut objects: Vec<Vec<u8>> = vec![
        Vec::new(),
        Vec::new(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    ];
    let mut kids = Vec::new();
    for page in pages {
        let page_id = objects.len() + 1;
        kids.push(format!("{page_id} 0 R"));
        match page {
            PageSpec::Text(lines) => {
                let mut content = String::new();
                for l in lines {
                    let y = 792.0 - l.top - l.size * 0.8;
                    content.push_str(&format!(
                        "BT /F1 {} Tf {} {} Td ({}) Tj ET\n",
                        l.size,
                        l.x,
                        y,
                        escape(&l.text)
                    ));
                }
                objects.push(
                    format!(
                        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                        page_id + 1
                    )
                    .into_bytes(),
                );
                objects.push(stream("", content.as_bytes()));
            }
            PageSpec::Image {
                width,
                height,
                gray,
            } => {
                let mut z =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                z.write_all(gray).unwrap();
                let data = z.finish().unwrap();
                objects.push(
                    format!(
                        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im0 {} 0 R >> >> /Contents {} 0 R >>",
                        page_id + 2,
                        page_id + 1
                    )
                    .into_bytes(),
                );
                objects.push(stream("", b"q 612 0 0 792 0 0 cm /Im0 Do Q"));
                objects.push(stream(
                    &format!(
                        "/Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode"
                    ),
                    &data,
                ));
            }
        }
    }
    objects[0] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();
    objects[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        kids.len()
    )
    .into_bytes();
    assemble(&objects)
}

/// Renders page `page` of `pdf_path` to grayscale pixels at `dpi` with poppler's `pdftoppm`.
pub fn render_gray(pdftoppm: &Path, pdf_path: &Path, page: u32, dpi: u32) -> (u32, u32, Vec<u8>) {
    let root = pdf_path.with_file_name(format!("render-{page}"));
    let n = page.to_string();
    let status = Command::new(pdftoppm)
        .args([
            "-gray",
            "-singlefile",
            "-r",
            &dpi.to_string(),
            "-f",
            &n,
            "-l",
            &n,
        ])
        .arg(pdf_path)
        .arg(&root)
        .status()
        .unwrap();
    assert!(status.success());
    let pgm_path = root.with_extension("pgm");
    let bytes = std::fs::read(&pgm_path).unwrap();
    std::fs::remove_file(&pgm_path).unwrap();
    // P5 <w> <h> <max> then one byte a pixel.
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..i]).into_owned());
    }
    assert_eq!(fields[0], "P5");
    let (w, h): (u32, u32) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    let pixels = bytes[i + 1..].to_vec();
    assert_eq!(pixels.len(), (w * h) as usize);
    (w, h, pixels)
}

/// Plants noise: `per_mille` of the pixels, chosen by a fixed sequence, flip to black or white.
pub fn plant_noise(gray: &mut [u8], per_mille: u64, seed: u64) {
    let mut s = seed;
    for p in gray.iter_mut() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        if s % 1000 < per_mille {
            *p = if s & 1024 == 0 { 0 } else { 255 };
        }
    }
}

/// The character error rate of `got` against `want`: edit distance over the length of
/// `want`, with whitespace collapsed.
pub fn cer(want: &str, got: &str) -> f64 {
    let a: Vec<char> = want
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    let b: Vec<char> = got
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        prev = cur;
    }
    prev[b.len()] as f64 / a.len().max(1) as f64
}
