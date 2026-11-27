//! HAP frame parser (public spec: github.com/Vidvox/hap/blob/master/documentation/HapVideoDRAFT.md).
//!
//! A HAP packet is a tree of sections. We unwrap the compression
//! (none / Snappy / chunked "complex") and hand the raw BCn blocks to the
//! GPU untouched, so HAP playback costs no CPU-side texture decoding.

use crate::FrameFormat;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HapError {
    #[error("truncated HAP section")]
    Truncated,
    #[error("unsupported HAP texture type {0:#04x}")]
    UnsupportedFormat(u8),
    #[error("unsupported HAP compressor {0:#04x}")]
    UnsupportedCompressor(u8),
    #[error("malformed HAP decode instructions")]
    BadInstructions,
    #[error("snappy: {0}")]
    Snappy(String),
    #[error("HAP texture is {got} bytes, expected {want}")]
    SizeMismatch { got: usize, want: usize },
}

/// Decompressed texture payload of one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HapTexture {
    pub format: FrameFormat,
    pub data: Vec<u8>,
}

/// Bytes per 4×4 block.
pub fn block_bytes(format: FrameFormat) -> usize {
    match format {
        FrameFormat::Bc1 => 8,
        _ => 16,
    }
}

/// Size of a BCn image whose pixel size is `w`×`h` (padded to 4×4 blocks).
pub fn bc_size(format: FrameFormat, w: u32, h: u32) -> usize {
    w.div_ceil(4) as usize * h.div_ceil(4) as usize * block_bytes(format)
}

/// Returns (section type, payload) and the remaining bytes after the section.
fn section(buf: &[u8]) -> Result<(u8, &[u8], &[u8]), HapError> {
    if buf.len() < 4 {
        return Err(HapError::Truncated);
    }
    let mut len = u32::from_le_bytes([buf[0], buf[1], buf[2], 0]) as usize;
    let ty = buf[3];
    let mut hdr: usize = 4;
    if len == 0 {
        if buf.len() < 8 {
            return Err(HapError::Truncated);
        }
        len = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
        hdr = 8;
    }
    let end = hdr.checked_add(len).ok_or(HapError::Truncated)?;
    if buf.len() < end {
        return Err(HapError::Truncated);
    }
    Ok((ty, &buf[hdr..end], &buf[end..]))
}

fn texture_format(ty: u8) -> Result<FrameFormat, HapError> {
    match ty & 0x0F {
        0xB => Ok(FrameFormat::Bc1),
        0xE => Ok(FrameFormat::Bc3),
        0xF => Ok(FrameFormat::Bc3YCoCg),
        0xC => Ok(FrameFormat::Bc7),
        _ => Err(HapError::UnsupportedFormat(ty)),
    }
}

fn snappy(data: &[u8]) -> Result<Vec<u8>, HapError> {
    snap::raw::Decoder::new()
        .decompress_vec(data)
        .map_err(|e| HapError::Snappy(e.to_string()))
}

fn u32s(b: &[u8]) -> impl Iterator<Item = usize> + '_ {
    b.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c) as usize)
}

/// "Complex" compressor: decode-instructions container followed by chunk data.
fn decompress_chunked(payload: &[u8]) -> Result<Vec<u8>, HapError> {
    let (ty, instructions, data) = section(payload)?;
    if ty != 0x01 {
        return Err(HapError::BadInstructions);
    }
    let (mut compressors, mut sizes, mut offsets) = (None, None, None);
    let mut rest = instructions;
    while !rest.is_empty() {
        let (ty, body, next) = section(rest)?;
        match ty {
            0x02 => compressors = Some(body),
            0x03 => sizes = Some(body),
            0x04 => offsets = Some(body),
            _ => {} // unknown instruction: skip, as the spec requires
        }
        rest = next;
    }
    let compressors = compressors.ok_or(HapError::BadInstructions)?;
    let sizes: Vec<usize> = u32s(sizes.ok_or(HapError::BadInstructions)?).collect();
    if sizes.len() != compressors.len() {
        return Err(HapError::BadInstructions);
    }
    let offsets: Vec<usize> = match offsets {
        Some(o) => u32s(o).collect(),
        None => sizes
            .iter()
            .scan(0, |acc, s| Some(std::mem::replace(acc, *acc + s)))
            .collect(),
    };
    if offsets.len() != sizes.len() {
        return Err(HapError::BadInstructions);
    }

    let mut out = Vec::new();
    for ((&comp, &size), &off) in compressors.iter().zip(&sizes).zip(&offsets) {
        let chunk = data.get(off..off + size).ok_or(HapError::Truncated)?;
        match comp {
            0x0A => out.extend_from_slice(chunk),
            0x0B => out.extend(snappy(chunk)?),
            c => return Err(HapError::UnsupportedCompressor(c)),
        }
    }
    Ok(out)
}

/// Decode one HAP packet of a `width`×`height` frame into raw BCn blocks.
/// For two-texture variants (Hap Q Alpha) only the colour texture is returned.
pub fn decode(packet: &[u8], width: u32, height: u32) -> Result<HapTexture, HapError> {
    let (mut ty, mut payload, _) = section(packet)?;
    if ty == 0x0D {
        // Multiple images: take the first (colour) texture.
        (ty, payload, _) = section(payload)?;
    }
    let format = texture_format(ty)?;
    let data = match ty >> 4 {
        0xA => payload.to_vec(),
        0xB => snappy(payload)?,
        0xC => decompress_chunked(payload)?,
        _ => return Err(HapError::UnsupportedCompressor(ty)),
    };
    let want = bc_size(format, width, height);
    if data.len() < want {
        return Err(HapError::SizeMismatch { got: data.len(), want });
    }
    let mut data = data;
    data.truncate(want);
    Ok(HapTexture { format, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(len: usize, ty: u8) -> Vec<u8> {
        let mut v = (len as u32).to_le_bytes()[..3].to_vec();
        v.push(ty);
        v
    }

    #[test]
    fn uncompressed_dxt1() {
        let blocks = vec![7u8; 8 * 4]; // 16×4 px → 4 blocks
        let mut p = hdr(blocks.len(), 0xAB);
        p.extend(&blocks);
        let t = decode(&p, 16, 4).unwrap();
        assert_eq!(t.format, FrameFormat::Bc1);
        assert_eq!(t.data, blocks);
    }

    #[test]
    fn snappy_dxt5_long_header() {
        let blocks: Vec<u8> = (0..16 * 4).map(|i| i as u8).collect();
        let z = snap::raw::Encoder::new().compress_vec(&blocks).unwrap();
        let mut p = vec![0, 0, 0, 0xBE];
        p.extend((z.len() as u32).to_le_bytes());
        p.extend(&z);
        let t = decode(&p, 8, 8).unwrap();
        assert_eq!(t.format, FrameFormat::Bc3);
        assert_eq!(t.data, blocks);
    }

    #[test]
    fn chunked_mixed() {
        let a = vec![1u8; 16];
        let b = vec![2u8; 16];
        let bz = snap::raw::Encoder::new().compress_vec(&b).unwrap();
        let mut inst = hdr(2, 0x02);
        inst.extend([0x0A, 0x0B]);
        inst.extend(hdr(8, 0x03));
        inst.extend((a.len() as u32).to_le_bytes());
        inst.extend((bz.len() as u32).to_le_bytes());
        let mut body = hdr(inst.len(), 0x01);
        body.extend(inst);
        body.extend(&a);
        body.extend(&bz);
        let mut p = hdr(body.len(), 0xCF);
        p.extend(body);
        let t = decode(&p, 8, 4).unwrap();
        assert_eq!(t.format, FrameFormat::Bc3YCoCg);
        assert_eq!(t.data, [a, b].concat());
    }

    #[test]
    fn rejects_short() {
        let mut p = hdr(8, 0xAB);
        p.extend([0u8; 8]);
        assert!(matches!(decode(&p, 16, 16), Err(HapError::SizeMismatch { .. })));
        assert_eq!(decode(&[1, 0], 4, 4), Err(HapError::Truncated));
    }
}
