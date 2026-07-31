/// HWP record, file-header, and body-text parsers.
///
/// Consolidated from hwpers parser/record.rs, parser/header.rs, and
/// parser/body_text.rs.
use super::error::{HwpError, Result};
use super::model::{CharShape, ParaText, Paragraph, Section};
use super::reader::{StreamReader, decompress_stream};

// ---------------------------------------------------------------------------

const HWP_SIGNATURE: &[u8] = b"HWP Document File";

/// The 256-byte file header at the start of every HWP 5.0 document.
#[cfg_attr(alef, alef(skip))]
#[derive(Debug, Clone)]
pub struct FileHeader {
    /// Flags word from bytes 36–39 (little-endian u32): bit 0 = compressed, bit 1 = encrypted.
    pub flags: u32,
}

impl FileHeader {
    pub(crate) fn parse(data: Vec<u8>) -> Result<Self> {
        if data.len() < 256 {
            return Err(HwpError::InvalidFormat(
                "FileHeader must be at least 256 bytes".to_string(),
            ));
        }

        if &data[..17] != HWP_SIGNATURE {
            return Err(HwpError::InvalidFormat("Invalid HWP signature".to_string()));
        }

        let flags = u32::from_le_bytes([data[36], data[37], data[38], data[39]]);

        Ok(Self { flags })
    }

    /// Whether section streams are zlib/deflate-compressed.
    pub(crate) fn is_compressed(&self) -> bool {
        (self.flags & 0x01) != 0
    }

    /// Whether the document is password-encrypted.
    pub(crate) fn is_encrypted(&self) -> bool {
        (self.flags & 0x02) != 0
    }
}

/// A single HWP binary record decoded from a stream.
///
/// Each record starts with a 32-bit packed header containing the tag ID,
/// outline level, and payload size; followed by the raw payload bytes.
#[cfg_attr(alef, alef(skip))]
#[derive(Debug)]
pub struct Record {
    /// Tag identifier (10-bit value from the packed record header).
    pub tag_id: u16,
    /// Raw payload bytes for this record.
    pub data: Vec<u8>,
}

impl Record {
    pub(crate) fn parse(reader: &mut StreamReader) -> Result<Self> {
        if reader.remaining() < 4 {
            return Err(HwpError::ParseError("Not enough data for record header".to_string()));
        }

        let header = reader.read_u32()?;
        let tag_id = (header & 0x3FF) as u16;
        let mut size = header >> 20;

        if size == 0xFFF {
            size = reader.read_u32()?;
        }

        let data_size = size as usize;
        if data_size > reader.remaining() {
            return Err(HwpError::ParseError(format!(
                "Record size {data_size} exceeds remaining data {}",
                reader.remaining()
            )));
        }

        let data = reader.read_bytes(data_size)?;
        Ok(Self { tag_id, data })
    }

    /// Return a fresh `StreamReader` over this record's data bytes.
    pub(crate) fn data_reader(&self) -> StreamReader {
        StreamReader::new(self.data.clone())
    }
}

/// HWPTAG_BEGIN as defined by the HWP 5.x specification.
const HWPTAG_BEGIN: u16 = 0x010;
/// HWP 5.x body-text record tag: paragraph header (HWPTAG_BEGIN + 64 = 0x50).
///
/// Per the HWP 5.0 binary specification, the paragraph header record uses tag
/// offset 64 from HWPTAG_BEGIN, yielding tag ID 0x50. This matches empirical
/// data from real HWP documents where 0x50 records correspond to paragraph
/// boundaries.
const TAG_PARA_HEADER: u16 = HWPTAG_BEGIN + 64;
/// HWP 5.x body-text record tag: paragraph text, UTF-16LE (HWPTAG_BEGIN + 65 = 0x51).
///
/// Per the HWP 5.0 binary specification, the paragraph text record uses tag
/// offset 65 from HWPTAG_BEGIN, yielding tag ID 0x51. The record payload is a
/// sequence of UTF-16LE code units representing the paragraph content.
const TAG_PARA_TEXT: u16 = HWPTAG_BEGIN + 65;
/// HWP 5.x body-text record tag: paragraph shape (HWPTAG_BEGIN + 66 = 0x52).
const TAG_PARA_SHAPE: u16 = HWPTAG_BEGIN + 66;
/// HWP 5.x body-text record tag: char shape (HWPTAG_BEGIN + 67 = 0x53).
const TAG_CHAR_SHAPE: u16 = HWPTAG_BEGIN + 67;

const TAG_CHAR_SHAPE_INFO: u16 = HWPTAG_BEGIN + 30;

pub(crate) fn parse_doc_info(data: Vec<u8>) -> Result<Vec<CharShape>> {
    let mut reader = StreamReader::new(data);
    let mut char_shapes = Vec::new();

    while reader.remaining() >= 4 {
        let record = match Record::parse(&mut reader) {
            Ok(r) => r,
            Err(_) => break,
        };

        if record.tag_id == TAG_CHAR_SHAPE_INFO && record.data.len() >= 4 {
            let font_attr = u32::from_le_bytes([record.data[0], record.data[1], record.data[2], record.data[3]]);
            char_shapes.push(CharShape {
                bold: (font_attr & 0x01) != 0,
                italic: (font_attr & 0x02) != 0,
                underline: (font_attr & 0x04) != 0,
            });
        }
    }

    Ok(char_shapes)
}

/// Parse a raw (possibly compressed) BodyText/SectionN stream.
///
/// Returns the list of sections found. Each section contains zero or more
/// paragraphs that carry the plain-text content.
pub(crate) fn parse_body_text(data: Vec<u8>, is_compressed: bool) -> Result<Vec<Section>> {
    let data = if is_compressed { decompress_stream(&data)? } else { data };

    let mut reader = StreamReader::new(data);
    let mut sections: Vec<Section> = Vec::new();
    let mut current_paragraphs: Vec<Paragraph> = Vec::new();
    let mut current_paragraph: Option<Paragraph> = None;

    while reader.remaining() >= 4 {
        let record = match Record::parse(&mut reader) {
            Ok(r) => r,
            Err(_) => break,
        };

        match record.tag_id {
            TAG_PARA_HEADER => {
                if let Some(para) = current_paragraph.take() {
                    current_paragraphs.push(para);
                }
                current_paragraph = Some(Paragraph::default());
            }
            TAG_PARA_TEXT => {
                if let Some(ref mut para) = current_paragraph
                    && let Ok(text) = ParaText::from_record(&record)
                {
                    para.text = Some(text);
                }
            }
            TAG_PARA_SHAPE => {
                if let Some(ref mut para) = current_paragraph
                    && record.data.len() > 18
                {
                    para.outline_level = record.data[18];
                }
            }
            TAG_CHAR_SHAPE => {
                if let Some(ref mut para) = current_paragraph {
                    let mut reader = record.data_reader();
                    while reader.remaining() >= 6 {
                        let pos = reader.read_u32().unwrap_or(0);
                        let shape_idx = reader.read_u16().unwrap_or(0);
                        para.char_shape_runs.push((pos, shape_idx));
                    }
                }
            }

            _ => {}
        }
    }

    if let Some(para) = current_paragraph {
        current_paragraphs.push(para);
    }

    sections.push(Section {
        paragraphs: current_paragraphs,
    });

    Ok(sections)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_hwp_extract_converted_output() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_documents/hwp/converted_output.hwp");
        if !path.exists() {
            println!("Skipping: test document not found at {}", path.display());
            return;
        }
        let bytes = std::fs::read(&path).expect("read file");
        let _doc = crate::extraction::hwp::extract_hwp_document(&bytes).expect("HWP extraction should succeed");
    }

    #[test]
    fn test_hwp_tag_constants() {
        assert_eq!(super::TAG_PARA_HEADER, 0x50);
        assert_eq!(super::TAG_PARA_TEXT, 0x51);
    }
}
