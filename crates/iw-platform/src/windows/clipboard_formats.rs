//! How clipboard content is laid out on Windows, and how it maps to what
//! devices exchange (`ClipboardFormat`). Free of FFI, so it is tested on
//! every OS; `clipboard` moves the bytes in and out of the clipboard.
//!
//! - Text: `CF_UNICODETEXT` is NUL-terminated UTF-16 with CRLF line ends;
//!   devices exchange UTF-8 with LF.
//! - HTML: "HTML Format" is UTF-8 behind a header of byte offsets
//!   (`Version`, `StartHTML`, `EndHTML`, `StartFragment`, `EndFragment`).
//!   Devices exchange the document between `StartHTML` and `EndHTML`,
//!   which keeps the `<!--StartFragment-->` and `<!--EndFragment-->`
//!   comments; written back, they mark the fragment again.
//! - RTF: "Rich Text Format" as it is, without the NULs some apps end it
//!   with.
//! - Images: the registered "PNG" format as it is. Apps that only put a
//!   bitmap (`CF_DIB`) on the clipboard have it converted to PNG; a PNG
//!   written to the clipboard also goes there as a 24-bit `CF_DIB` drawn
//!   over white, for the many apps that only paste bitmaps.
//! - Content marked `ExcludeClipboardContentFromMonitorProcessing`, or
//!   with `CanIncludeInClipboardHistory` or `CanUploadToCloudClipboard` set
//!   to 0, is sensitive and never read.

/// Bitmaps larger than this many pixels are not converted, as decoding
/// them would take more memory than a clipboard InputWeave carries is
/// worth.
pub(crate) const MAX_PIXELS: u64 = 64 * 1024 * 1024;

pub(crate) const CF_DIB: u32 = 8;
pub(crate) const CF_UNICODETEXT: u32 = 13;
/// Registered format names.
pub(crate) const HTML_FORMAT: &str = "HTML Format";
pub(crate) const RTF_FORMAT: &str = "Rich Text Format";
pub(crate) const PNG_FORMAT: &str = "PNG";

/// Whether content with these markers is not to be shared: `exclude` if
/// the exclusion format is present, and the data of the two opt-out
/// formats if present, each a DWORD that is 0 to opt out.
pub(crate) fn sensitive(exclude: bool, history: Option<&[u8]>, cloud: Option<&[u8]>) -> bool {
    let opted_out = |data: Option<&[u8]>| {
        data.and_then(|d| d.get(..4))
            .is_some_and(|d| u32::from_le_bytes([d[0], d[1], d[2], d[3]]) == 0)
    };
    exclude || opted_out(history) || opted_out(cloud)
}

/// UTF-8 text with LF line ends from `CF_UNICODETEXT`, which ends at the
/// first NUL.
pub(crate) fn text_from_utf16(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end]).replace("\r\n", "\n")
}

/// `CF_UNICODETEXT` for UTF-8 text: CRLF line ends, NUL-terminated.
pub(crate) fn utf16_from_text(text: &str) -> Vec<u16> {
    let crlf = text.replace("\r\n", "\n").replace('\n', "\r\n");
    crlf.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `data` without the NULs at its end.
pub(crate) fn trim_nuls(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    &data[..end]
}

/// The HTML in "HTML Format": the document between `StartHTML` and
/// `EndHTML`, or the fragment when the document is not given. `None` if
/// the header is missing or its offsets do not fit the data.
pub(crate) fn html_from_cf_html(data: &[u8]) -> Option<Vec<u8>> {
    let data = trim_nuls(data);
    let offset = |name: &str| -> Option<usize> {
        let key = format!("{name}:");
        let header = &data[..data.len().min(1024)];
        let at = find(header, key.as_bytes())? + key.len();
        let digits: Vec<u8> = header[at..]
            .iter()
            .copied()
            .take_while(|b| b.is_ascii_digit() || *b == b'-')
            .collect();
        let value: i64 = std::str::from_utf8(&digits).ok()?.parse().ok()?;
        usize::try_from(value).ok()
    };
    let range = |start: Option<usize>, end: Option<usize>| match (start, end) {
        (Some(start), Some(end)) if start <= end && end <= data.len() => {
            Some(data[start..end].to_vec())
        }
        _ => None,
    };
    range(offset("StartHTML"), offset("EndHTML"))
        .or_else(|| range(offset("StartFragment"), offset("EndFragment")))
}

const START_FRAGMENT: &[u8] = b"<!--StartFragment-->";
const END_FRAGMENT: &[u8] = b"<!--EndFragment-->";

/// "HTML Format" for `html`. A document that marks its fragment, as one
/// read from "HTML Format" does, keeps it; anything else becomes the
/// fragment of a document around it.
pub(crate) fn cf_html_from_html(html: &[u8]) -> Vec<u8> {
    let marked = find(html, START_FRAGMENT)
        .map(|start| start + START_FRAGMENT.len())
        .and_then(|start| Some((start, start + find(&html[start..], END_FRAGMENT)?)));
    let (document, fragment) = match marked {
        Some(fragment) => (html.to_vec(), fragment),
        None => {
            let prefix = b"<html>\r\n<body>\r\n<!--StartFragment-->";
            let mut document = prefix.to_vec();
            document.extend_from_slice(html);
            document.extend_from_slice(b"<!--EndFragment-->\r\n</body>\r\n</html>");
            (document, (prefix.len(), prefix.len() + html.len()))
        }
    };
    // Every offset is ten digits, so the header's length is known before
    // the offsets are.
    let header_len = header(0, 0, 0, 0).len();
    let start = header_len;
    let end = header_len + document.len();
    let mut out = header(start, end, start + fragment.0, start + fragment.1).into_bytes();
    out.extend_from_slice(&document);
    out
}

fn header(
    start_html: usize,
    end_html: usize,
    start_fragment: usize,
    end_fragment: usize,
) -> String {
    format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\n\
         StartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
    )
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

const BI_BITFIELDS: u32 = 3;
const FILE_HEADER_LEN: usize = 14;

/// A BMP file of the bitmap in `CF_DIB`, for the image decoder: the file
/// header in front, pointing past the bitmap's header, color masks and
/// palette to its pixels. `None` if the header does not make sense or
/// the bitmap has more than [`MAX_PIXELS`] pixels.
pub(crate) fn bmp_file_from_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(dib.get(at..at + 4)?.try_into().ok()?))
    };
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(dib.get(at..at + 2)?.try_into().ok()?))
    };
    let header_len = u32_at(0)? as usize;
    // BITMAPINFOHEADER and its later versions, up to BITMAPV5HEADER.
    if !(40..=124).contains(&header_len) || dib.len() < header_len {
        return None;
    }
    let width = i32::from_le_bytes(dib.get(4..8)?.try_into().ok()?);
    let height = i32::from_le_bytes(dib.get(8..12)?.try_into().ok()?);
    let pixels = u64::from(width.unsigned_abs()) * u64::from(height.unsigned_abs());
    if width <= 0 || height == 0 || pixels > MAX_PIXELS {
        return None;
    }
    let bit_count = u16_at(14)?;
    let compression = u32_at(16)?;
    let colors_used = u32_at(32)?;
    // The three color masks follow a plain BITMAPINFOHEADER; later
    // versions hold them in the header.
    let masks = if compression == BI_BITFIELDS && header_len == 40 {
        12
    } else {
        0
    };
    let colors = match colors_used {
        0 if (1..=8).contains(&bit_count) => 1usize << bit_count,
        n => n as usize,
    };
    let pixels_at = header_len + masks + colors.checked_mul(4)?;
    if pixels_at > dib.len() {
        return None;
    }
    let total = u32::try_from(FILE_HEADER_LEN + dib.len()).ok()?;
    let offset = u32::try_from(FILE_HEADER_LEN + pixels_at).ok()?;
    let mut file = Vec::with_capacity(FILE_HEADER_LEN + dib.len());
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&total.to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&offset.to_le_bytes());
    file.extend_from_slice(dib);
    Some(file)
}

/// A 24-bit bottom-up `CF_DIB` of an image given as top-down rows of
/// 32-bit BGRA pixels, `stride` bytes apart, drawn over white so apps that
/// ignore transparency show what the image shows. `None` if `pixels` is
/// shorter than the image or the image is larger than [`MAX_PIXELS`].
pub(crate) fn dib_from_bgra(
    width: u32,
    height: u32,
    stride: usize,
    pixels: &[u8],
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    if stride < w * 4 || pixels.len() < stride * (h - 1) + w * 4 {
        return None;
    }
    let row = (w * 3 + 3) & !3;
    let image = row * h;
    let mut dib = Vec::with_capacity(40 + image);
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(width as i32).to_le_bytes());
    // Positive: the rows go bottom up, which every app reads.
    dib.extend_from_slice(&(height as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&24u16.to_le_bytes());
    // BI_RGB, the image size, then resolution and palette unset.
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&(image as u32).to_le_bytes());
    dib.extend_from_slice(&[0; 16]);
    for y in (0..h).rev() {
        let start = dib.len();
        for px in pixels[y * stride..y * stride + w * 4].as_chunks::<4>().0 {
            let alpha = u32::from(px[3]);
            for &channel in &px[..3] {
                // Straight alpha over white.
                let over_white = (u32::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255;
                dib.push(over_white as u8);
            }
        }
        dib.resize(start + row, 0);
    }
    Some(dib)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_outs_and_exclusion_make_content_sensitive() {
        let zero = 0u32.to_le_bytes();
        let one = 1u32.to_le_bytes();
        assert!(!sensitive(false, None, None));
        assert!(sensitive(true, None, None));
        assert!(sensitive(false, Some(&zero), None));
        assert!(sensitive(false, None, Some(&zero)));
        assert!(!sensitive(false, Some(&one), Some(&one)));
        // Too short to say anything.
        assert!(!sensitive(false, Some(&[0, 0]), None));
    }

    #[test]
    fn text_travels_as_utf8_with_lf_and_lands_with_crlf() {
        let mut units: Vec<u16> = "第一行\r\nsecond\r\n".encode_utf16().collect();
        units.extend([0, b'x' as u16]);
        assert_eq!(text_from_utf16(&units), "第一行\nsecond\n");
        assert_eq!(text_from_utf16(&[]), "");

        let written = utf16_from_text("a\nb\r\nc");
        assert_eq!(written.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&written[..written.len() - 1]).unwrap(),
            "a\r\nb\r\nc"
        );
        assert_eq!(text_from_utf16(&written), "a\nb\nc");
    }

    #[test]
    fn rtf_loses_its_trailing_nuls() {
        assert_eq!(trim_nuls(b"{\\rtf1 hi}\0\0"), b"{\\rtf1 hi}");
        assert_eq!(trim_nuls(b"\0"), b"");
        assert_eq!(trim_nuls(b"{\\rtf1}"), b"{\\rtf1}");
    }

    #[test]
    fn html_gets_a_header_whose_offsets_point_at_it() {
        let cf = cf_html_from_html("<b>粗體</b>".as_bytes());
        let text = std::str::from_utf8(&cf).unwrap();
        let offset = |name: &str| -> usize {
            let at = text.find(&format!("{name}:")).unwrap() + name.len() + 1;
            text[at..at + 10].parse().unwrap()
        };
        assert_eq!(&cf[offset("StartHTML")..offset("StartHTML") + 6], b"<html>");
        assert_eq!(offset("EndHTML"), cf.len());
        assert_eq!(
            &cf[offset("StartFragment")..offset("EndFragment")],
            "<b>粗體</b>".as_bytes()
        );
        // Read back, it is the whole document, fragment marks and all.
        let read = html_from_cf_html(&cf).unwrap();
        assert!(read.starts_with(b"<html>"));
        assert!(
            find(
                &read,
                "<!--StartFragment--><b>粗體</b><!--EndFragment-->".as_bytes()
            )
            .is_some()
        );
        // A document that marks its fragment keeps it when written again.
        let again = cf_html_from_html(&read);
        assert_eq!(html_from_cf_html(&again).unwrap(), read);
        let text = std::str::from_utf8(&again).unwrap();
        let at = text.find("StartFragment:").unwrap() + 14;
        let start: usize = text[at..at + 10].parse().unwrap();
        assert!(again[start..].starts_with("<b>粗體</b>".as_bytes()));
    }

    #[test]
    fn html_from_other_apps_is_read_by_its_offsets() {
        // As browsers write it: the document is given, with a fragment.
        let body = "<html><body><!--StartFragment--><i>x</i><!--EndFragment--></body></html>";
        let header_len = "Version:0.9\r\nStartHTML:000105\r\nEndHTML:000000\r\nStartFragment:000000\r\nEndFragment:000000\r\n".len();
        let header = format!(
            "Version:0.9\r\nStartHTML:{:06}\r\nEndHTML:{:06}\r\nStartFragment:{:06}\r\nEndFragment:{:06}\r\n",
            header_len,
            header_len + body.len(),
            header_len + body.find("<i>").unwrap(),
            header_len + body.find("<!--End").unwrap(),
        );
        let data = format!("{header}{body}\0");
        assert_eq!(html_from_cf_html(data.as_bytes()).unwrap(), body.as_bytes());

        // Without the document, the fragment.
        let fragment_only = |start: usize| {
            format!(
                "Version:1.0\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:{:06}\r\nEndFragment:{:06}\r\n",
                start,
                start + 8
            )
        };
        let data = format!("{}<u>u</u>", fragment_only(fragment_only(0).len()));
        assert_eq!(html_from_cf_html(data.as_bytes()).unwrap(), b"<u>u</u>");

        // Offsets past the end, or no header at all.
        assert_eq!(
            html_from_cf_html(b"StartHTML:0000000099\r\nEndHTML:0000000200\r\n"),
            None
        );
        assert_eq!(html_from_cf_html(b"<p>plain</p>"), None);
    }

    /// A bottom-up BITMAPINFOHEADER bitmap.
    fn dib_header(width: i32, height: i32, bits: u16, compression: u32, colors: u32) -> Vec<u8> {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&width.to_le_bytes());
        dib.extend_from_slice(&height.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&bits.to_le_bytes());
        dib.extend_from_slice(&compression.to_le_bytes());
        dib.extend_from_slice(&[0; 12]);
        dib.extend_from_slice(&colors.to_le_bytes());
        dib.extend_from_slice(&[0; 4]);
        dib
    }

    fn pixel_offset(bmp: &[u8]) -> usize {
        u32::from_le_bytes(bmp[10..14].try_into().unwrap()) as usize
    }

    #[test]
    fn bitmaps_become_files_pointing_past_masks_and_palettes() {
        let mut dib = dib_header(2, 2, 32, 0, 0);
        dib.extend_from_slice(&[0xab; 16]);
        let bmp = bmp_file_from_dib(&dib).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(
            u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize,
            bmp.len()
        );
        assert_eq!(pixel_offset(&bmp), 14 + 40);
        assert_eq!(&bmp[14..], &dib[..]);

        // Color masks after a plain header.
        let mut dib = dib_header(2, 2, 32, BI_BITFIELDS, 0);
        dib.extend_from_slice(&[0; 12 + 16]);
        assert_eq!(
            pixel_offset(&bmp_file_from_dib(&dib).unwrap()),
            14 + 40 + 12
        );

        // A palette of 2^8 colors, or as many as the header says.
        let mut dib = dib_header(4, 1, 8, 0, 0);
        dib.extend_from_slice(&[0; 256 * 4 + 4]);
        assert_eq!(
            pixel_offset(&bmp_file_from_dib(&dib).unwrap()),
            14 + 40 + 1024
        );
        let mut dib = dib_header(4, 1, 8, 0, 16);
        dib.extend_from_slice(&[0; 16 * 4 + 4]);
        assert_eq!(
            pixel_offset(&bmp_file_from_dib(&dib).unwrap()),
            14 + 40 + 64
        );

        // Nonsense, truncated, or too large.
        assert_eq!(bmp_file_from_dib(&[1, 2, 3]), None);
        assert_eq!(bmp_file_from_dib(&dib_header(4, 1, 8, 0, 0)), None);
        assert_eq!(bmp_file_from_dib(&dib_header(0, 1, 32, 0, 0)), None);
        assert_eq!(
            bmp_file_from_dib(&dib_header(100_000, 100_000, 32, 0, 0)),
            None
        );
    }

    #[test]
    fn images_become_bottom_up_bitmaps_over_white() {
        // 2 × 2, top-down: opaque red, transparent, half-transparent black,
        // opaque blue; rows 12 bytes apart.
        let pixels = [
            0, 0, 255, 255, 9, 9, 9, 0, 0, 0, 0, 0, //
            0, 0, 0, 128, 255, 0, 0, 255, 0, 0, 0, 0,
        ];
        let dib = dib_from_bgra(2, 2, 12, &pixels).unwrap();
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(
            i32::from_le_bytes(dib[8..12].try_into().unwrap()),
            2,
            "bottom up"
        );
        assert_eq!(u16::from_le_bytes(dib[14..16].try_into().unwrap()), 24);
        // Rows of 6 bytes padded to 8; the bottom row comes first.
        assert_eq!(dib.len(), 40 + 16);
        assert_eq!(&dib[40..48], &[127, 127, 127, 255, 0, 0, 0, 0]);
        assert_eq!(&dib[48..56], &[0, 0, 255, 255, 255, 255, 0, 0]);

        assert_eq!(dib_from_bgra(2, 2, 8, &pixels[..15]), None);
        assert_eq!(dib_from_bgra(0, 2, 8, &pixels), None);
    }
}
