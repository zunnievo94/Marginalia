//! HUFF/CDIC decompression used by some Mobipocket books (notably
//! dictionaries). Port of the algorithm in calibre's `mobi/huffcdic.py`.

use crate::{Error, Result};

fn err(msg: &str) -> Error {
    Error::Mobi(format!("HUFF/CDIC: {msg}"))
}

fn u16_at(b: &[u8], off: usize) -> Result<u16> {
    b.get(off..off + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .ok_or_else(|| err("truncated record"))
}

fn u32_at(b: &[u8], off: usize) -> Result<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| err("truncated record"))
}

enum Entry {
    /// Fully expanded phrase.
    Ready(Vec<u8>),
    /// Phrase that is itself compressed and must be unpacked once.
    Packed(Vec<u8>),
    /// Being expanded right now (guards against cyclic dictionaries).
    InProgress,
}

pub(crate) struct HuffReader {
    /// (code length, terminal, max code) indexed by the top 8 bits.
    dict1: Vec<(u32, bool, u64)>,
    min_code: [u64; 33],
    max_code: [u64; 33],
    dictionary: Vec<Entry>,
}

impl HuffReader {
    /// `records[0]` is the HUFF record, the rest are CDIC records.
    pub(crate) fn new(records: &[&[u8]]) -> Result<Self> {
        let huff = records.first().ok_or_else(|| err("missing HUFF record"))?;
        if huff.get(0..8) != Some(b"HUFF\x00\x00\x00\x18") {
            return Err(err("bad HUFF header"));
        }
        let off1 = u32_at(huff, 8)? as usize;
        let off2 = u32_at(huff, 12)? as usize;

        let mut dict1 = Vec::with_capacity(256);
        for i in 0..256 {
            let v = u32_at(huff, off1 + i * 4)?;
            let (len, term, max) = (v & 0x1f, v & 0x80 != 0, (v >> 8) as u64);
            if len == 0 || (len <= 8 && !term) {
                return Err(err("bad code table"));
            }
            dict1.push((len, term, ((max + 1) << (32 - len)) - 1));
        }

        let mut min_code = [0u64; 33];
        let mut max_code = [0u64; 33];
        max_code[0] = (1u64 << 32) - 1;
        for len in 1..=32usize {
            let min = u32_at(huff, off2 + (len - 1) * 8)? as u64;
            let max = u32_at(huff, off2 + (len - 1) * 8 + 4)? as u64;
            min_code[len] = min << (32 - len);
            max_code[len] = ((max + 1) << (32 - len)) - 1;
        }

        let mut reader = HuffReader {
            dict1,
            min_code,
            max_code,
            dictionary: Vec::new(),
        };
        for cdic in &records[1..] {
            reader.load_cdic(cdic)?;
        }
        Ok(reader)
    }

    fn load_cdic(&mut self, cdic: &[u8]) -> Result<()> {
        if cdic.get(0..8) != Some(b"CDIC\x00\x00\x00\x10") {
            return Err(err("bad CDIC header"));
        }
        let phrases = u32_at(cdic, 8)? as usize;
        let bits = u32_at(cdic, 12)?.min(31);
        let n = (1usize << bits).min(phrases.saturating_sub(self.dictionary.len()));
        for i in 0..n {
            let off = u16_at(cdic, 16 + i * 2)? as usize;
            let blen = u16_at(cdic, 16 + off)? as usize;
            let start = 18 + off;
            let bytes = cdic
                .get(start..start + (blen & 0x7fff))
                .ok_or_else(|| err("phrase out of bounds"))?
                .to_vec();
            self.dictionary.push(if blen & 0x8000 != 0 {
                Entry::Ready(bytes)
            } else {
                Entry::Packed(bytes)
            });
        }
        Ok(())
    }

    pub(crate) fn unpack(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        let mut bits_left = data.len() as i64 * 8;
        let mut padded = data.to_vec();
        padded.extend_from_slice(&[0; 8]);
        let read_q = |pos: usize| u64::from_be_bytes(padded[pos..pos + 8].try_into().unwrap());

        let mut pos = 0usize;
        let mut x = read_q(pos);
        let mut n: i64 = 32;
        let mut out = Vec::new();
        loop {
            if n <= 0 {
                pos += 4;
                if pos + 8 > padded.len() {
                    break;
                }
                x = read_q(pos);
                n += 32;
            }
            let code = (x >> n) & 0xffff_ffff;
            let (mut len, term, mut max) = self.dict1[(code >> 24) as usize];
            if !term {
                while len < 32 && code < self.min_code[len as usize] {
                    len += 1;
                }
                max = self.max_code[len as usize];
            }
            n -= len as i64;
            bits_left -= len as i64;
            if bits_left < 0 {
                break;
            }
            let index = (max.wrapping_sub(code) >> (32 - len)) as usize;
            let entry = self
                .dictionary
                .get_mut(index)
                .ok_or_else(|| err("dictionary index out of range"))?;
            match std::mem::replace(entry, Entry::InProgress) {
                Entry::Ready(bytes) => {
                    out.extend_from_slice(&bytes);
                    self.dictionary[index] = Entry::Ready(bytes);
                }
                Entry::Packed(bytes) => {
                    let expanded = self.unpack(&bytes)?;
                    out.extend_from_slice(&expanded);
                    self.dictionary[index] = Entry::Ready(expanded);
                }
                Entry::InProgress => return Err(err("cyclic dictionary")),
            }
        }
        Ok(out)
    }
}
