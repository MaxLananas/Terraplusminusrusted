//! RFC 1951 DEFLATE decoder with an output limit and optional streaming sink.

use crate::DecodeError;

const MAX_CODE_BITS: usize = 15;
const FAST_BITS: u32 = 10;
const WINDOW_SIZE: usize = 32 * 1024;
const STREAM_FLUSH_THRESHOLD: usize = 4 * WINDOW_SIZE;

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CODE_LENGTH_ORDER: [usize; 19] =
    [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u64,
    available: u32,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0, buffer: 0, available: 0 }
    }

    fn refill(&mut self) {
        while self.available <= 56 {
            let Some(&byte) = self.data.get(self.position) else { break };
            self.buffer |= u64::from(byte) << self.available;
            self.position += 1;
            self.available += 8;
        }
    }

    fn bits(&mut self, count: u32) -> Result<u32, DecodeError> {
        if count == 0 {
            return Ok(0);
        }
        if self.available < count {
            self.refill();
            if self.available < count {
                return Err(DecodeError::Truncated);
            }
        }
        let value = (self.buffer & ((1u64 << count) - 1)) as u32;
        self.buffer >>= count;
        self.available -= count;
        Ok(value)
    }

    fn align_to_byte(&mut self) {
        let drop = self.available % 8;
        self.buffer >>= drop;
        self.available -= drop;
    }

    /// Bytes consumed from the input, excluding whole bytes still buffered.
    pub(crate) fn consumed_bytes(&self) -> usize {
        self.position - (self.available / 8) as usize
    }
}

struct Huffman {
    counts: [u16; MAX_CODE_BITS + 1],
    symbols: Vec<u16>,
    /// Indexed by the next `FAST_BITS` input bits (LSB-first). Entry = symbol << 4 | length, 0 = slow path.
    fast: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, DecodeError> {
        let mut counts = [0u16; MAX_CODE_BITS + 1];
        for &length in lengths {
            counts[usize::from(length)] += 1;
        }
        counts[0] = 0;
        let mut left: i32 = 1;
        for &count in &counts[1..] {
            left <<= 1;
            left -= i32::from(count);
            if left < 0 {
                return Err(DecodeError::InvalidHuffmanTable("over-subscribed code lengths"));
            }
        }
        let mut offsets = [0u16; MAX_CODE_BITS + 2];
        for length in 1..=MAX_CODE_BITS {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let slot = &mut offsets[usize::from(length)];
                symbols[usize::from(*slot)] = symbol as u16;
                *slot += 1;
            }
        }

        let mut fast = vec![0u16; 1 << FAST_BITS];
        let mut code: u32 = 0;
        let mut index = 0usize;
        for length in 1..=MAX_CODE_BITS as u32 {
            for _ in 0..counts[length as usize] {
                if length <= FAST_BITS {
                    let reversed = code.reverse_bits() >> (32 - length);
                    let entry = (symbols[index] << 4) | length as u16;
                    let mut slot = reversed as usize;
                    while slot < fast.len() {
                        fast[slot] = entry;
                        slot += 1 << length;
                    }
                }
                code += 1;
                index += 1;
            }
            code <<= 1;
        }
        Ok(Self { counts, symbols, fast })
    }

    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16, DecodeError> {
        if reader.available < MAX_CODE_BITS as u32 {
            reader.refill();
        }
        let entry = self.fast[(reader.buffer & ((1 << FAST_BITS) - 1)) as usize];
        let length = u32::from(entry & 0xF);
        if entry != 0 && length <= reader.available {
            reader.buffer >>= length;
            reader.available -= length;
            return Ok(entry >> 4);
        }
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for length in 1..=MAX_CODE_BITS {
            code |= reader.bits(1)? as i32;
            let count = i32::from(self.counts[length]);
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(DecodeError::InvalidSymbol)
    }
}

type Sink<'s> = &'s mut dyn FnMut(&[u8]) -> Result<(), String>;

struct Output<'s> {
    window: Vec<u8>,
    flushed: usize,
    limit: usize,
    sink: Option<Sink<'s>>,
}

impl Output<'_> {
    fn total(&self) -> usize {
        self.flushed + self.window.len()
    }

    fn reserve(&mut self, additional: usize) -> Result<(), DecodeError> {
        if self.total().saturating_add(additional) > self.limit {
            return Err(DecodeError::OutputLimitExceeded { limit: self.limit });
        }
        Ok(())
    }

    fn push(&mut self, byte: u8) -> Result<(), DecodeError> {
        self.reserve(1)?;
        self.window.push(byte);
        self.maybe_flush()
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<(), DecodeError> {
        self.reserve(bytes.len())?;
        self.window.extend_from_slice(bytes);
        self.maybe_flush()
    }

    fn copy_back(&mut self, distance: usize, length: usize) -> Result<(), DecodeError> {
        if distance == 0 || distance > self.window.len() {
            return Err(DecodeError::DistanceTooFar);
        }
        self.reserve(length)?;
        let start = self.window.len() - distance;
        if distance >= length {
            self.window.extend_from_within(start..start + length);
        } else {
            for offset in 0..length {
                let byte = self.window[start + offset];
                self.window.push(byte);
            }
        }
        self.maybe_flush()
    }

    fn maybe_flush(&mut self) -> Result<(), DecodeError> {
        if self.window.len() >= STREAM_FLUSH_THRESHOLD
            && let Some(sink) = self.sink.as_mut()
        {
            let emit = self.window.len() - WINDOW_SIZE;
            sink(&self.window[..emit]).map_err(DecodeError::Sink)?;
            self.window.drain(..emit);
            self.flushed += emit;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), DecodeError> {
        if let Some(sink) = self.sink.as_mut() {
            sink(&self.window).map_err(DecodeError::Sink)?;
            self.flushed += self.window.len();
            self.window.clear();
        }
        Ok(())
    }
}

fn fixed_tables() -> Result<(Huffman, Huffman), DecodeError> {
    let mut literal_lengths = [0u8; 288];
    literal_lengths[..144].fill(8);
    literal_lengths[144..256].fill(9);
    literal_lengths[256..280].fill(7);
    literal_lengths[280..].fill(8);
    Ok((Huffman::new(&literal_lengths)?, Huffman::new(&[5u8; 30])?))
}

fn dynamic_tables(reader: &mut BitReader<'_>) -> Result<(Huffman, Huffman), DecodeError> {
    let literal_count = reader.bits(5)? as usize + 257;
    let distance_count = reader.bits(5)? as usize + 1;
    let code_length_count = reader.bits(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return Err(DecodeError::InvalidHuffmanTable("too many literal or distance codes"));
    }
    let mut code_lengths = [0u8; 19];
    for &position in CODE_LENGTH_ORDER.iter().take(code_length_count) {
        code_lengths[position] = reader.bits(3)? as u8;
    }
    let code_length_table = Huffman::new(&code_lengths)?;

    let total = literal_count + distance_count;
    let mut lengths = vec![0u8; total];
    let mut index = 0;
    while index < total {
        let symbol = code_length_table.decode(reader)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => {
                let Some(&previous) = index.checked_sub(1).and_then(|i| lengths.get(i)) else {
                    return Err(DecodeError::InvalidHuffmanTable("repeat without previous length"));
                };
                (previous, 3 + reader.bits(2)? as usize)
            }
            17 => (0, 3 + reader.bits(3)? as usize),
            18 => (0, 11 + reader.bits(7)? as usize),
            _ => return Err(DecodeError::InvalidSymbol),
        };
        if index + repeat > total {
            return Err(DecodeError::InvalidHuffmanTable("code length repeat overflows table"));
        }
        lengths[index..index + repeat].fill(value);
        index += repeat;
    }
    if lengths[256] == 0 {
        return Err(DecodeError::InvalidHuffmanTable("missing end-of-block code"));
    }
    Ok((Huffman::new(&lengths[..literal_count])?, Huffman::new(&lengths[literal_count..])?))
}

fn inflate_block(
    reader: &mut BitReader<'_>,
    output: &mut Output<'_>,
    literals: &Huffman,
    distances: &Huffman,
) -> Result<(), DecodeError> {
    loop {
        let symbol = literals.decode(reader)?;
        match symbol {
            0..=255 => output.push(symbol as u8)?,
            256 => return Ok(()),
            257..=285 => {
                let index = usize::from(symbol - 257);
                let length = usize::from(LENGTH_BASE[index])
                    + reader.bits(u32::from(LENGTH_EXTRA[index]))? as usize;
                let distance_symbol = usize::from(distances.decode(reader)?);
                if distance_symbol >= DISTANCE_BASE.len() {
                    return Err(DecodeError::InvalidSymbol);
                }
                let distance = usize::from(DISTANCE_BASE[distance_symbol])
                    + reader.bits(u32::from(DISTANCE_EXTRA[distance_symbol]))? as usize;
                output.copy_back(distance, length)?;
            }
            _ => return Err(DecodeError::InvalidSymbol),
        }
    }
}

fn run(reader: &mut BitReader<'_>, output: &mut Output<'_>) -> Result<(), DecodeError> {
    loop {
        let is_final = reader.bits(1)? == 1;
        match reader.bits(2)? {
            0 => {
                reader.align_to_byte();
                let length = reader.bits(16)?;
                let complement = reader.bits(16)?;
                if length != !complement & 0xFFFF {
                    return Err(DecodeError::InvalidStoredLength);
                }
                // The bit buffer is byte aligned here, so buffered whole bytes come first.
                let mut remaining = length as usize;
                while remaining > 0 && reader.available >= 8 {
                    output.push(reader.bits(8)? as u8)?;
                    remaining -= 1;
                }
                let start = reader.position;
                let end = start.checked_add(remaining).ok_or(DecodeError::Truncated)?;
                let bytes = reader.data.get(start..end).ok_or(DecodeError::Truncated)?;
                output.extend(bytes)?;
                reader.position = end;
            }
            1 => {
                let (literals, distances) = fixed_tables()?;
                inflate_block(reader, output, &literals, &distances)?;
            }
            2 => {
                let (literals, distances) = dynamic_tables(reader)?;
                inflate_block(reader, output, &literals, &distances)?;
            }
            _ => return Err(DecodeError::InvalidBlockType),
        }
        if is_final {
            return Ok(());
        }
    }
}

/// Inflates a raw DEFLATE stream. Returns the output and the number of input bytes consumed.
pub fn inflate(input: &[u8], output_limit: usize) -> Result<(Vec<u8>, usize), DecodeError> {
    let mut reader = BitReader::new(input);
    let mut output =
        Output { window: Vec::new(), flushed: 0, limit: output_limit, sink: None };
    run(&mut reader, &mut output)?;
    Ok((output.window, reader.consumed_bytes()))
}

/// Inflates a raw DEFLATE stream into `sink` without retaining more than a bounded window.
/// Returns (total output bytes, input bytes consumed).
pub fn inflate_streaming(
    input: &[u8],
    output_limit: usize,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<(usize, usize), DecodeError> {
    let mut reader = BitReader::new(input);
    let mut output =
        Output { window: Vec::new(), flushed: 0, limit: output_limit, sink: Some(sink) };
    run(&mut reader, &mut output)?;
    output.finish()?;
    Ok((output.flushed, reader.consumed_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_block_round_trip() {
        let payload = b"hello stored world";
        let mut stream = vec![0b0000_0001];
        let length = payload.len() as u16;
        stream.extend_from_slice(&length.to_le_bytes());
        stream.extend_from_slice(&(!length).to_le_bytes());
        stream.extend_from_slice(payload);
        let (out, consumed) = inflate(&stream, 1024).expect("valid stream");
        assert_eq!(out, payload);
        assert_eq!(consumed, stream.len());
    }

    #[test]
    fn fixed_huffman_block_from_zlib_reference() {
        // Raw DEFLATE from CPython zlib: compressobj(9, DEFLATED, -15) over 80 bytes of b"a".
        let stream = [0x4B, 0x4C, 0xA4, 0x2E, 0x00, 0x00];
        let (out, _) = inflate(&stream, 1024).expect("valid stream");
        assert_eq!(out, vec![b'a'; 80]);
    }

    #[test]
    fn output_limit_is_enforced() {
        let stream = [0x4B, 0x4C, 0xA4, 0x2E, 0x00, 0x00];
        assert_eq!(inflate(&stream, 10), Err(DecodeError::OutputLimitExceeded { limit: 10 }));
    }

    #[test]
    fn truncated_input_is_rejected() {
        let stream = [0x4B, 0x4C, 0xA4];
        assert!(inflate(&stream, 1024).is_err());
    }

    #[test]
    fn reserved_block_type_is_rejected() {
        assert_eq!(inflate(&[0b0000_0111], 16), Err(DecodeError::InvalidBlockType));
    }

    #[test]
    fn distance_before_start_is_rejected() {
        // Fixed block: length symbol 257 (3 bytes) with distance code 0 before any literal.
        // Bits (LSB-first): BFINAL=1, BTYPE=01, then 7-bit code 0000001 for symbol 257, then 5-bit distance 00000.
        let stream = [0b0000_0011, 0b0000_0010, 0x00];
        assert_eq!(inflate(&stream, 16), Err(DecodeError::DistanceTooFar));
    }
}
