//! JPEG decoding to packed ARGB, matching `BufferedImage.getRGB` after `ImageIO.read`.
//!
//! OpenJDK's JPEG reader is IJG libjpeg 6b with its default decompression parameters, so this is
//! that decoder: Huffman sequential (SOF0, SOF1) and progressive (SOF2) scans (`jdhuff.c`,
//! `jdphuff.c`), the accurate integer IDCT (`jidctint.c`, `JDCT_ISLOW`) with its post-IDCT range
//! table, "fancy" triangle-filter upsampling for 2:1 chroma (`jdsample.c`), box replication for
//! other integral ratios, and the fixed-point YCbCr to RGB tables of `jdcolor.c`. Colour space
//! detection is libjpeg's `default_decompress_parms`: JFIF or component ids 1, 2, 3 mean YCbCr,
//! an Adobe marker's transform flag decides otherwise, ids `R`, `G`, `B` mean RGB.
//!
//! Grayscale images come back as Java's `TYPE_BYTE_GRAY`, whose `getRGB` applies the linear gray
//! to sRGB curve (see `png.rs`). What libjpeg 6b itself refuses is refused here too: arithmetic
//! coding, lossless and hierarchical processes, 12-bit samples, and CMYK/YCCK (which ImageIO's
//! reader does not turn into an RGB image either). Embedded ICC profiles are not applied.

use crate::DecodeError;
use crate::png::{PngImage, gray8_to_srgb};

#[derive(Debug, Clone, Copy)]
pub struct JpegLimits {
    pub max_width: u32,
    pub max_height: u32,
}

impl Default for JpegLimits {
    fn default() -> Self {
        Self { max_width: 4096, max_height: 4096 }
    }
}

fn invalid(reason: &'static str) -> DecodeError {
    DecodeError::InvalidJpeg(reason)
}

fn unsupported(reason: impl Into<String>) -> DecodeError {
    DecodeError::UnsupportedJpeg(reason.into())
}

/// `jpeg_natural_order`, with libjpeg's 16 guard entries for corrupt run lengths.
const NATURAL_ORDER: [usize; 80] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30,
    37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63,
];

fn natural(k: usize) -> usize {
    NATURAL_ORDER[k.min(79)]
}

/// A Huffman table in `jdhuff.c`'s canonical form.
#[derive(Clone)]
struct Huffman {
    /// For each code length 1..=16: largest code of that length, or -1.
    max_code: [i32; 18],
    /// Index into `values` of the first code of each length, minus that code.
    offset: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8; 16], values: Vec<u8>) -> Result<Self, DecodeError> {
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        if total > 256 || total != values.len() {
            return Err(invalid("bad Huffman table"));
        }
        let mut max_code = [-1i32; 18];
        let mut offset = [0i32; 17];
        let (mut code, mut index) = (0i32, 0i32);
        for length in 1..=16 {
            let count = i32::from(counts[length - 1]);
            offset[length] = index - code;
            if count > 0 {
                code += count;
                index += count;
                max_code[length] = code - 1;
            }
            if code > (1 << length) {
                return Err(invalid("bad Huffman table"));
            }
            code <<= 1;
        }
        max_code[17] = i32::MAX;
        Ok(Self { max_code, offset, values })
    }
}

/// Entropy-coded data reader: byte stuffing, and zeros once a marker is reached (libjpeg's
/// behaviour on premature markers).
struct Bits<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u32,
    count: u32,
    /// A marker was reached; only zero bits follow until the decoder resynchronises.
    at_marker: bool,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], position: usize) -> Self {
        Self { data, position, buffer: 0, count: 0, at_marker: false }
    }

    fn fill(&mut self) {
        while self.count <= 24 {
            let mut byte = 0u32;
            if !self.at_marker {
                match self.data.get(self.position) {
                    Some(0xFF) => match self.data.get(self.position + 1) {
                        Some(0x00) => {
                            byte = 0xFF;
                            self.position += 2;
                        }
                        _ => self.at_marker = true,
                    },
                    Some(&value) => {
                        byte = u32::from(value);
                        self.position += 1;
                    }
                    None => self.at_marker = true,
                }
            }
            self.buffer |= byte << (24 - self.count);
            self.count += 8;
        }
    }

    fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        self.fill();
        let value = self.buffer >> (32 - n);
        self.buffer <<= n;
        self.count -= n;
        value
    }

    fn bit(&mut self) -> u32 {
        self.bits(1)
    }

    fn decode(&mut self, table: &Huffman) -> Result<u8, DecodeError> {
        let mut code = 0i32;
        for length in 1..=16 {
            code = (code << 1) | self.bit() as i32;
            if code <= table.max_code[length] {
                let index = table.offset[length] + code;
                return table.values.get(index as usize).copied().ok_or(invalid("corrupt Huffman data"));
            }
        }
        // libjpeg warns and returns 0 for an impossible code.
        Ok(0)
    }

    /// `HUFF_EXTEND(receive(s), s)`.
    fn receive_extend(&mut self, s: u8) -> i32 {
        if s == 0 {
            return 0;
        }
        let s = u32::from(s.min(16));
        let value = self.bits(s) as i32;
        if value < (1 << (s - 1)) { value - (1 << s) + 1 } else { value }
    }

    /// Drops buffered bits and steps over an `RSTn` marker if one is next.
    fn restart(&mut self) {
        self.buffer = 0;
        self.count = 0;
        self.at_marker = false;
        if self.data.get(self.position) == Some(&0xFF) {
            let mut next = self.position + 1;
            while self.data.get(next) == Some(&0xFF) {
                next += 1;
            }
            if let Some(0xD0..=0xD7) = self.data.get(next) {
                self.position = next + 1;
            }
        }
    }
}

#[derive(Clone)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    quant_index: usize,
    /// Quantisation table latched at the component's first scan (`latch_quant_tables`).
    quant: Option<[u16; 64]>,
    /// Blocks per line and per column of the MCU-padded coefficient array.
    blocks_wide: usize,
    blocks_high: usize,
    coefficients: Vec<[i16; 64]>,
    dc_prediction: i32,
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    components: Vec<Component>,
    h_max: usize,
    v_max: usize,
    mcus_wide: usize,
    mcus_high: usize,
}

struct Scan {
    components: Vec<(usize, usize, usize)>,
    start: usize,
    end: usize,
    high: u8,
    low: u8,
}

pub fn decode_jpeg(bytes: &[u8], limits: &JpegLimits) -> Result<PngImage, DecodeError> {
    if bytes.get(..2) != Some(&[0xFF, 0xD8][..]) {
        return Err(invalid("missing SOI marker"));
    }
    let mut position = 2;
    let mut quant: [Option<[u16; 64]>; 4] = [None; 4];
    let mut dc_tables: [Option<Huffman>; 4] = [None, None, None, None];
    let mut ac_tables: [Option<Huffman>; 4] = [None, None, None, None];
    let mut frame: Option<Frame> = None;
    let mut restart_interval = 0usize;
    let mut jfif = false;
    let mut adobe_transform: Option<u8> = None;
    let mut eob_run = 0u32;
    loop {
        // Find the next marker, skipping fill bytes (and, leniently, garbage as libjpeg does).
        while bytes.get(position).is_some_and(|&b| b != 0xFF) {
            position += 1;
        }
        while bytes.get(position) == Some(&0xFF) {
            position += 1;
        }
        let Some(&marker) = bytes.get(position) else {
            // A missing EOI: libjpeg warns and finishes with what it has.
            break;
        };
        position += 1;
        match marker {
            0xD8 => return Err(invalid("duplicate SOI")),
            0xD9 => break,
            0xD0..=0xD7 | 0x01 => continue,
            _ => {}
        }
        let length_bytes = bytes.get(position..position + 2).ok_or(DecodeError::Truncated)?;
        let length = usize::from(u16::from_be_bytes([length_bytes[0], length_bytes[1]]));
        if length < 2 {
            return Err(invalid("segment length below 2"));
        }
        let segment = bytes.get(position + 2..position + length).ok_or(DecodeError::Truncated)?;
        position += length;
        match marker {
            0xC0..=0xC2 => {
                if frame.is_some() {
                    return Err(invalid("more than one frame"));
                }
                frame = Some(parse_frame(segment, marker == 0xC2, limits)?);
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Err(unsupported(match marker {
                    0xC9..=0xCB | 0xCD..=0xCF => "arithmetic coding",
                    0xC3 | 0xC7 => "lossless JPEG",
                    _ => "hierarchical JPEG",
                }));
            }
            0xC4 => parse_huffman(segment, &mut dc_tables, &mut ac_tables)?,
            0xDB => parse_quant(segment, &mut quant)?,
            0xDD => {
                let interval = segment.get(..2).ok_or(invalid("short DRI"))?;
                restart_interval = usize::from(u16::from_be_bytes([interval[0], interval[1]]));
            }
            0xDC => return Err(unsupported("DNL marker")),
            0xE0 => jfif |= segment.starts_with(b"JFIF\0"),
            0xEE => {
                if segment.len() >= 12 && segment.starts_with(b"Adobe") {
                    adobe_transform = Some(segment[11]);
                }
            }
            0xDA => {
                let frame = frame.as_mut().ok_or(invalid("SOS before SOF"))?;
                let scan = parse_scan(segment, frame)?;
                for &(index, _, _) in &scan.components {
                    let component = &mut frame.components[index];
                    if component.quant.is_none() {
                        component.quant = Some(quant[component.quant_index].ok_or(invalid("missing quantisation table"))?);
                    }
                }
                position = decode_scan(bytes, position, frame, &scan, &dc_tables, &ac_tables, restart_interval, &mut eob_run)?;
            }
            _ => {}
        }
    }
    let frame = frame.ok_or(invalid("no frame"))?;
    if frame.components.iter().any(|component| component.quant.is_none()) {
        return Err(invalid("a component never appeared in a scan"));
    }
    render(&frame, jfif, adobe_transform)
}

fn parse_frame(segment: &[u8], progressive: bool, limits: &JpegLimits) -> Result<Frame, DecodeError> {
    if segment.len() < 6 {
        return Err(invalid("short SOF"));
    }
    if segment[0] != 8 {
        return Err(unsupported(format!("{}-bit samples", segment[0])));
    }
    let height = usize::from(u16::from_be_bytes([segment[1], segment[2]]));
    let width = usize::from(u16::from_be_bytes([segment[3], segment[4]]));
    if width == 0 || height == 0 {
        return Err(invalid("zero image dimension (DNL is not supported)"));
    }
    if width > limits.max_width as usize || height > limits.max_height as usize {
        return Err(DecodeError::ImageTooLarge { width: width as u32, height: height as u32 });
    }
    let count = usize::from(segment[5]);
    match count {
        1 | 3 => {}
        4 => return Err(unsupported("CMYK or YCCK JPEG")),
        _ => return Err(unsupported(format!("{count}-component JPEG"))),
    }
    if segment.len() < 6 + 3 * count {
        return Err(invalid("short SOF"));
    }
    let mut components = Vec::with_capacity(count);
    for c in 0..count {
        let entry = &segment[6 + 3 * c..9 + 3 * c];
        let (h, v) = (usize::from(entry[1] >> 4), usize::from(entry[1] & 15));
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || entry[2] > 3 {
            return Err(invalid("bad sampling factor or table index"));
        }
        components.push(Component { id: entry[0], h, v, quant_index: usize::from(entry[2]), quant: None, blocks_wide: 0, blocks_high: 0, coefficients: Vec::new(), dc_prediction: 0 });
    }
    let h_max = components.iter().map(|c| c.h).max().unwrap_or(1);
    let v_max = components.iter().map(|c| c.v).max().unwrap_or(1);
    let mcus_wide = width.div_ceil(8 * h_max);
    let mcus_high = height.div_ceil(8 * v_max);
    for component in &mut components {
        component.blocks_wide = mcus_wide * component.h;
        component.blocks_high = mcus_high * component.v;
        component.coefficients = vec![[0i16; 64]; component.blocks_wide * component.blocks_high];
    }
    Ok(Frame { width, height, progressive, components, h_max, v_max, mcus_wide, mcus_high })
}

fn parse_huffman(mut segment: &[u8], dc: &mut [Option<Huffman>; 4], ac: &mut [Option<Huffman>; 4]) -> Result<(), DecodeError> {
    while !segment.is_empty() {
        if segment.len() < 17 {
            return Err(invalid("short DHT"));
        }
        let (class, id) = (segment[0] >> 4, usize::from(segment[0] & 15));
        if class > 1 || id > 3 {
            return Err(invalid("bad DHT class or id"));
        }
        let mut counts = [0u8; 16];
        counts.copy_from_slice(&segment[1..17]);
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        let values = segment.get(17..17 + total).ok_or(invalid("short DHT"))?.to_vec();
        let table = Huffman::new(&counts, values)?;
        if class == 0 { dc[id] = Some(table) } else { ac[id] = Some(table) }
        segment = &segment[17 + total..];
    }
    Ok(())
}

fn parse_quant(mut segment: &[u8], quant: &mut [Option<[u16; 64]>; 4]) -> Result<(), DecodeError> {
    while !segment.is_empty() {
        let (precision, id) = (segment[0] >> 4, usize::from(segment[0] & 15));
        if precision > 1 || id > 3 {
            return Err(invalid("bad DQT precision or id"));
        }
        let size = if precision == 0 { 64 } else { 128 };
        let data = segment.get(1..1 + size).ok_or(invalid("short DQT"))?;
        let mut table = [0u16; 64];
        for k in 0..64 {
            let value = if precision == 0 { u16::from(data[k]) } else { u16::from_be_bytes([data[2 * k], data[2 * k + 1]]) };
            // Stored in zigzag order; kept in natural order.
            table[NATURAL_ORDER[k]] = value;
        }
        quant[id] = Some(table);
        segment = &segment[1 + size..];
    }
    Ok(())
}

fn parse_scan(segment: &[u8], frame: &Frame) -> Result<Scan, DecodeError> {
    let count = usize::from(*segment.first().ok_or(invalid("short SOS"))?);
    if count == 0 || count > 4 || segment.len() < 4 + 2 * count {
        return Err(invalid("bad SOS"));
    }
    let mut components = Vec::with_capacity(count);
    for c in 0..count {
        let (id, tables) = (segment[1 + 2 * c], segment[2 + 2 * c]);
        let index = frame.components.iter().position(|component| component.id == id).ok_or(invalid("SOS names an unknown component"))?;
        let (dc, ac) = (usize::from(tables >> 4), usize::from(tables & 15));
        if dc > 3 || ac > 3 {
            return Err(invalid("bad Huffman table index"));
        }
        components.push((index, dc, ac));
    }
    let tail = &segment[1 + 2 * count..];
    let (start, end, high, low) = (usize::from(tail[0]), usize::from(tail[1]), tail[2] >> 4, tail[2] & 15);
    if frame.progressive {
        let valid = if start == 0 { end == 0 } else { start <= end && end <= 63 && count == 1 };
        if !valid || high > 13 || low > 13 {
            return Err(invalid("bad progressive scan parameters"));
        }
    }
    Ok(Scan { components, start, end, high, low })
}

#[allow(clippy::too_many_arguments)]
fn decode_scan(
    bytes: &[u8],
    position: usize,
    frame: &mut Frame,
    scan: &Scan,
    dc_tables: &[Option<Huffman>; 4],
    ac_tables: &[Option<Huffman>; 4],
    restart_interval: usize,
    eob_run: &mut u32,
) -> Result<usize, DecodeError> {
    let mut bits = Bits::new(bytes, position);
    for component in &mut frame.components {
        component.dc_prediction = 0;
    }
    *eob_run = 0;
    let table = |tables: &[Option<Huffman>; 4], index: usize| tables[index].clone().ok_or(invalid("missing Huffman table"));
    let mut huffman = Vec::with_capacity(scan.components.len());
    for &(_, dc, ac) in &scan.components {
        let needs_dc = !frame.progressive || (scan.start == 0 && scan.high == 0);
        let needs_ac = !frame.progressive || scan.start > 0;
        huffman.push((if needs_dc { Some(table(dc_tables, dc)?) } else { None }, if needs_ac { Some(table(ac_tables, ac)?) } else { None }));
    }
    // A single-component scan covers that component's own blocks, not the MCU-padded array.
    let blocks: Vec<(usize, usize, usize)> = if scan.components.len() == 1 {
        let index = scan.components[0].0;
        let component = &frame.components[index];
        let wide = (frame.width * component.h).div_ceil(frame.h_max).div_ceil(8);
        let high = (frame.height * component.v).div_ceil(frame.v_max).div_ceil(8);
        (0..high).flat_map(|by| (0..wide).map(move |bx| (0, bx, by))).collect()
    } else {
        Vec::new()
    };
    let units = if scan.components.len() == 1 { blocks.len() } else { frame.mcus_wide * frame.mcus_high };
    #[allow(clippy::needless_range_loop)] // `unit` also drives restarts and MCU positions.
    for unit in 0..units {
        if restart_interval > 0 && unit > 0 && unit % restart_interval == 0 {
            bits.restart();
            for component in &mut frame.components {
                component.dc_prediction = 0;
            }
            *eob_run = 0;
        }
        if scan.components.len() == 1 {
            let (_, bx, by) = blocks[unit];
            let index = scan.components[0].0;
            decode_block(&mut bits, frame, index, by * frame.components[index].blocks_wide + bx, scan, &huffman[0], eob_run)?;
        } else {
            let (mx, my) = (unit % frame.mcus_wide, unit / frame.mcus_wide);
            for (slot, &(index, _, _)) in scan.components.iter().enumerate() {
                let (h, v, wide) = (frame.components[index].h, frame.components[index].v, frame.components[index].blocks_wide);
                for y in 0..v {
                    for x in 0..h {
                        let block = (my * v + y) * wide + mx * h + x;
                        decode_block(&mut bits, frame, index, block, scan, &huffman[slot], eob_run)?;
                    }
                }
            }
        }
    }
    // Resume marker parsing where the entropy-coded data stopped.
    let mut end = bits.position;
    while end < bytes.len() && !(bytes[end] == 0xFF && bytes.get(end + 1).is_some_and(|&b| b != 0 && !(0xD0..=0xD7).contains(&b))) {
        end += 1;
    }
    Ok(end)
}

fn decode_block(bits: &mut Bits<'_>, frame: &mut Frame, index: usize, block: usize, scan: &Scan, huffman: &(Option<Huffman>, Option<Huffman>), eob_run: &mut u32) -> Result<(), DecodeError> {
    let progressive = frame.progressive;
    let component = &mut frame.components[index];
    let coefficients = component.coefficients.get_mut(block).ok_or(invalid("block outside the frame"))?;
    let low = u32::from(scan.low);
    if !progressive {
        let (Some(dc), Some(ac)) = huffman else { return Err(invalid("missing Huffman table")) };
        let s = bits.decode(dc)?;
        component.dc_prediction = component.dc_prediction.wrapping_add(bits.receive_extend(s));
        coefficients[0] = component.dc_prediction as i16;
        let mut k = 1;
        while k < 64 {
            let rs = bits.decode(ac)?;
            let (r, s) = (usize::from(rs >> 4), rs & 15);
            if s != 0 {
                k += r;
                coefficients[natural(k)] = bits.receive_extend(s) as i16;
            } else if r == 15 {
                k += 15;
            } else {
                break;
            }
            k += 1;
        }
        return Ok(());
    }
    if scan.start == 0 {
        if scan.high == 0 {
            let Some(dc) = &huffman.0 else { return Err(invalid("missing Huffman table")) };
            let s = bits.decode(dc)?;
            component.dc_prediction = component.dc_prediction.wrapping_add(bits.receive_extend(s));
            coefficients[0] = (component.dc_prediction << low) as i16;
        } else if bits.bit() != 0 {
            coefficients[0] |= (1i32 << low) as i16;
        }
        return Ok(());
    }
    let Some(ac) = &huffman.1 else { return Err(invalid("missing Huffman table")) };
    if scan.high == 0 {
        // `decode_mcu_AC_first`.
        if *eob_run > 0 {
            *eob_run -= 1;
            return Ok(());
        }
        let mut k = scan.start;
        while k <= scan.end {
            let rs = bits.decode(ac)?;
            let (r, s) = (usize::from(rs >> 4), rs & 15);
            if s != 0 {
                k += r;
                coefficients[natural(k)] = (bits.receive_extend(s) << low) as i16;
            } else if r == 15 {
                k += 15;
            } else {
                *eob_run = 1 << r;
                if r > 0 {
                    *eob_run += bits.bits(r as u32);
                }
                *eob_run -= 1;
                break;
            }
            k += 1;
        }
        return Ok(());
    }
    // `decode_mcu_AC_refine`.
    let p1 = (1i32 << low) as i16;
    let m1 = ((-1i32) << low) as i16;
    let refine = |bits: &mut Bits<'_>, coefficient: &mut i16| {
        if bits.bit() != 0 && (*coefficient & p1) == 0 {
            *coefficient = if *coefficient >= 0 { coefficient.wrapping_add(p1) } else { coefficient.wrapping_add(m1) };
        }
    };
    let mut k = scan.start;
    if *eob_run == 0 {
        while k <= scan.end {
            let rs = bits.decode(ac)?;
            let (mut r, s) = (i32::from(rs >> 4), rs & 15);
            let mut value = 0i16;
            if s != 0 {
                value = if bits.bit() != 0 { p1 } else { m1 };
            } else if r != 15 {
                *eob_run = 1 << r;
                if r > 0 {
                    *eob_run += bits.bits(r as u32);
                }
                break;
            }
            while k <= scan.end {
                let coefficient = &mut coefficients[natural(k)];
                if *coefficient != 0 {
                    refine(bits, coefficient);
                } else {
                    r -= 1;
                    if r < 0 {
                        break;
                    }
                }
                k += 1;
            }
            if value != 0 {
                coefficients[natural(k)] = value;
            }
            k += 1;
        }
    }
    if *eob_run > 0 {
        while k <= scan.end {
            let coefficient = &mut coefficients[natural(k)];
            if *coefficient != 0 {
                refine(bits, coefficient);
            }
            k += 1;
        }
        *eob_run -= 1;
    }
    Ok(())
}

const CONST_BITS: u32 = 13;
const PASS1_BITS: u32 = 2;
const FIX_0_298631336: i64 = 2446;
const FIX_0_390180644: i64 = 3196;
const FIX_0_541196100: i64 = 4433;
const FIX_0_765366865: i64 = 6270;
const FIX_0_899976223: i64 = 7373;
const FIX_1_175875602: i64 = 9633;
const FIX_1_501321110: i64 = 12299;
const FIX_1_847759065: i64 = 15137;
const FIX_1_961570560: i64 = 16069;
const FIX_2_053119869: i64 = 16819;
const FIX_2_562915447: i64 = 20995;
const FIX_3_072711026: i64 = 25172;

fn descale(value: i64, shift: u32) -> i64 {
    (value + (1 << (shift - 1))) >> shift
}

/// The post-IDCT `range_limit[x & RANGE_MASK]` of `jdmaster.c`: the low 10 bits taken as a
/// signed value, re-centred and clamped.
fn post_idct(value: i64) -> u8 {
    let wrapped = (value as i32) & 1023;
    let signed = if wrapped >= 512 { wrapped - 1024 } else { wrapped };
    (signed + 128).clamp(0, 255) as u8
}

/// One `jidctint.c` 1-D pass over (z0..z7), returning the eight outputs before descaling.
fn idct_1d(z: [i64; 8]) -> [i64; 8] {
    let z1 = (z[2] + z[6]) * FIX_0_541196100;
    let tmp2 = z1 + z[6] * -FIX_1_847759065;
    let tmp3 = z1 + z[2] * FIX_0_765366865;
    let tmp0 = (z[0] + z[4]) << CONST_BITS;
    let tmp1 = (z[0] - z[4]) << CONST_BITS;
    let (tmp10, tmp13, tmp11, tmp12) = (tmp0 + tmp3, tmp0 - tmp3, tmp1 + tmp2, tmp1 - tmp2);
    let (mut t0, mut t1, mut t2, mut t3) = (z[7], z[5], z[3], z[1]);
    let (mut z1, mut z2, mut z3, mut z4) = (t0 + t3, t1 + t2, t0 + t2, t1 + t3);
    let z5 = (z3 + z4) * FIX_1_175875602;
    t0 *= FIX_0_298631336;
    t1 *= FIX_2_053119869;
    t2 *= FIX_3_072711026;
    t3 *= FIX_1_501321110;
    z1 *= -FIX_0_899976223;
    z2 *= -FIX_2_562915447;
    z3 *= -FIX_1_961570560;
    z4 *= -FIX_0_390180644;
    z3 += z5;
    z4 += z5;
    t0 += z1 + z3;
    t1 += z2 + z4;
    t2 += z2 + z3;
    t3 += z1 + z4;
    [tmp10 + t3, tmp11 + t2, tmp12 + t1, tmp13 + t0, tmp13 - t0, tmp12 - t1, tmp11 - t2, tmp10 - t3]
}

/// `jpeg_idct_islow`: dequantised coefficients to 64 samples, row-major.
fn idct_block(coefficients: &[i16; 64], quant: &[u16; 64], out: &mut [u8; 64]) {
    let mut workspace = [0i64; 64];
    for column in 0..8 {
        let z: [i64; 8] = std::array::from_fn(|row| i64::from(coefficients[row * 8 + column]) * i64::from(quant[row * 8 + column]));
        let result = idct_1d(z);
        for row in 0..8 {
            // `(int) DESCALE(..., CONST_BITS - PASS1_BITS)`.
            workspace[row * 8 + column] = i64::from(descale(result[row], CONST_BITS - PASS1_BITS) as i32);
        }
    }
    for row in 0..8 {
        let z: [i64; 8] = std::array::from_fn(|column| workspace[row * 8 + column]);
        let result = idct_1d(z);
        for column in 0..8 {
            out[row * 8 + column] = post_idct(descale(result[column], CONST_BITS + PASS1_BITS + 3));
        }
    }
}

/// A component's decoded samples at its own resolution (`downsampled_width` x `downsampled_height`).
struct Plane {
    width: usize,
    height: usize,
    samples: Vec<u8>,
}

fn component_plane(frame: &Frame, component: &Component) -> Result<Plane, DecodeError> {
    let quant = component.quant.as_ref().ok_or(invalid("missing quantisation table"))?;
    let width = (frame.width * component.h).div_ceil(frame.h_max);
    let height = (frame.height * component.v).div_ceil(frame.v_max);
    let stride = component.blocks_wide * 8;
    let mut full = vec![0u8; stride * component.blocks_high * 8];
    let mut block = [0u8; 64];
    let used_high = height.div_ceil(8).min(component.blocks_high);
    let used_wide = width.div_ceil(8).min(component.blocks_wide);
    for by in 0..used_high {
        for bx in 0..used_wide {
            idct_block(&component.coefficients[by * component.blocks_wide + bx], quant, &mut block);
            for row in 0..8 {
                let start = (by * 8 + row) * stride + bx * 8;
                full[start..start + 8].copy_from_slice(&block[row * 8..row * 8 + 8]);
            }
        }
    }
    let mut samples = Vec::with_capacity(width * height);
    for y in 0..height {
        samples.extend_from_slice(&full[y * stride..y * stride + width]);
    }
    Ok(Plane { width, height, samples })
}

/// `jdsample.c`: a plane brought to the full image size.
fn upsample(plane: &Plane, h_expand: usize, v_expand: usize, width: usize, height: usize) -> Vec<u8> {
    let at = |x: usize, y: usize| i32::from(plane.samples[y.min(plane.height - 1) * plane.width + x.min(plane.width - 1)]);
    let mut out = vec![0u8; width * height];
    let fancy = plane.width > 2;
    if h_expand == 2 && v_expand == 1 && fancy {
        // `h2v1_fancy_upsample`.
        for y in 0..height {
            let row = y.min(plane.height - 1);
            let mut line = Vec::with_capacity(plane.width * 2);
            let n = plane.width;
            line.push(at(0, row));
            line.push((at(0, row) * 3 + at(1, row) + 2) >> 2);
            for x in 1..n - 1 {
                let value = at(x, row) * 3;
                line.push((value + at(x - 1, row) + 1) >> 2);
                line.push((value + at(x + 1, row) + 2) >> 2);
            }
            line.push((at(n - 1, row) * 3 + at(n - 2, row) + 1) >> 2);
            line.push(at(n - 1, row));
            for x in 0..width {
                out[y * width + x] = line[x] as u8;
            }
        }
        return out;
    }
    if h_expand == 2 && v_expand == 2 && fancy {
        // `h2v2_fancy_upsample`: the context rows are the image's own, replicated at the edges.
        let n = plane.width;
        for y in 0..height {
            let row = y / 2;
            let other = if y % 2 == 0 { row.saturating_sub(1) } else { (row + 1).min(plane.height - 1) };
            let sum = |x: usize| at(x, row) * 3 + at(x, other);
            let mut line = Vec::with_capacity(n * 2);
            let this = sum(0);
            let next = sum(1);
            line.push((this * 4 + 8) >> 4);
            line.push((this * 3 + next + 7) >> 4);
            let (mut last, mut this) = (this, next);
            for x in 2..n {
                let next = sum(x);
                line.push((this * 3 + last + 8) >> 4);
                line.push((this * 3 + next + 7) >> 4);
                (last, this) = (this, next);
            }
            line.push((this * 3 + last + 8) >> 4);
            line.push((this * 4 + 7) >> 4);
            for x in 0..width {
                out[y * width + x] = line[x] as u8;
            }
        }
        return out;
    }
    // `fullsize_upsample`, `h2v1_upsample`, `h2v2_upsample`, `int_upsample`: replication.
    for y in 0..height {
        for x in 0..width {
            out[y * width + x] = at(x / h_expand, y / v_expand) as u8;
        }
    }
    out
}

/// `jdcolor.c`'s `build_ycc_rgb_table` and `ycc_rgb_convert`.
fn ycc_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    const SCALEBITS: u32 = 16;
    const ONE_HALF: i64 = 1 << (SCALEBITS - 1);
    let fix = |x: f64| (x * f64::from(1u32 << SCALEBITS) + 0.5) as i64;
    let (cb, cr) = (i64::from(cb) - 128, i64::from(cr) - 128);
    let cr_r = (fix(1.402_00) * cr + ONE_HALF) >> SCALEBITS;
    let cb_b = (fix(1.772_00) * cb + ONE_HALF) >> SCALEBITS;
    let g = (-fix(0.344_14) * cb + ONE_HALF + -fix(0.714_14) * cr) >> SCALEBITS;
    let y = i64::from(y);
    let clamp = |value: i64| value.clamp(0, 255) as u8;
    [clamp(y + cr_r), clamp(y + g), clamp(y + cb_b)]
}

fn render(frame: &Frame, jfif: bool, adobe_transform: Option<u8>) -> Result<PngImage, DecodeError> {
    let (width, height) = (frame.width, frame.height);
    let mut planes = Vec::with_capacity(frame.components.len());
    for component in &frame.components {
        if !frame.h_max.is_multiple_of(component.h) || !frame.v_max.is_multiple_of(component.v) {
            return Err(unsupported("fractional sampling ratios"));
        }
        let plane = component_plane(frame, component)?;
        planes.push(upsample(&plane, frame.h_max / component.h, frame.v_max / component.v, width, height));
    }
    let argb = if planes.len() == 1 {
        planes[0].iter().map(|&gray| {
            let gray = gray8_to_srgb(gray);
            u32::from_be_bytes([0xFF, gray, gray, gray])
        }).collect()
    } else {
        let ids: Vec<u8> = frame.components.iter().map(|c| c.id).collect();
        let ycc = if jfif {
            true
        } else if let Some(transform) = adobe_transform {
            transform != 0
        } else {
            ids != [b'R', b'G', b'B']
        };
        (0..width * height)
            .map(|i| {
                let [r, g, b] = if ycc { ycc_to_rgb(planes[0][i], planes[1][i], planes[2][i]) } else { [planes[0][i], planes[1][i], planes[2][i]] };
                u32::from_be_bytes([0xFF, r, g, b])
            })
            .collect()
    };
    Ok(PngImage { width: width as u32, height: height as u32, argb })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idct_of_a_flat_block_is_flat() {
        let mut coefficients = [0i16; 64];
        coefficients[0] = 80;
        let quant = [1u16; 64];
        let mut out = [0u8; 64];
        idct_block(&coefficients, &quant, &mut out);
        // DC 80 -> 80 / 8 = 10 above mid-gray.
        assert!(out.iter().all(|&v| v == 138), "{out:?}");
        assert_eq!((post_idct(-129), post_idct(200), post_idct(600), post_idct(1020)), (0, 255, 0, 124));
    }

    #[test]
    fn ycc_tables_match_libjpeg() {
        assert_eq!(ycc_to_rgb(128, 128, 128), [128, 128, 128]);
        assert_eq!(ycc_to_rgb(0, 128, 128), [0, 0, 0]);
    }

    #[test]
    fn rejects_what_libjpeg_6b_rejects() {
        assert!(matches!(decode_jpeg(b"not a jpeg", &JpegLimits::default()), Err(DecodeError::InvalidJpeg(_))));
        let arithmetic = [0xFF, 0xD8, 0xFF, 0xC9, 0x00, 0x0B, 8, 0, 1, 0, 1, 1, 1, 0x11, 0, 0xFF, 0xD9];
        assert!(matches!(decode_jpeg(&arithmetic, &JpegLimits::default()), Err(DecodeError::UnsupportedJpeg(_))));
        let twelve_bit = [0xFF, 0xD8, 0xFF, 0xC1, 0x00, 0x0B, 12, 0, 1, 0, 1, 1, 1, 0x11, 0, 0xFF, 0xD9];
        assert!(matches!(decode_jpeg(&twelve_bit, &JpegLimits::default()), Err(DecodeError::UnsupportedJpeg(_))));
    }
}
