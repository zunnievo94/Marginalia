//! Just enough of NumPy's `.npz` format to read VieNeu's weight files: a zip
//! of `.npy` arrays, little-endian, C order.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};

/// One array, converted to `f32` (floats) or `i64` (integers and bools).
#[derive(Debug, Clone)]
pub(crate) struct Array {
    pub shape: Vec<usize>,
    pub data: Data,
}

#[derive(Debug, Clone)]
pub(crate) enum Data {
    F32(Vec<f32>),
    I64(Vec<i64>),
}

impl Array {
    pub fn into_f32(self) -> Vec<f32> {
        match self.data {
            Data::F32(v) => v,
            Data::I64(v) => v.into_iter().map(|x| x as f32).collect(),
        }
    }

    pub fn scalar(&self) -> f64 {
        match &self.data {
            Data::F32(v) => v.first().copied().unwrap_or_default() as f64,
            Data::I64(v) => v.first().copied().unwrap_or_default() as f64,
        }
    }
}

/// All arrays of an `.npz` file, by name (without `.npy`).
pub(crate) fn load(path: &Path) -> Result<HashMap<String, Array>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut zip =
        zip::ZipArchive::new(file).with_context(|| format!("reading {}", path.display()))?;
    let mut out = HashMap::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().trim_end_matches(".npy").to_string();
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;
        let array =
            parse_npy(&bytes).with_context(|| format!("array {name} in {}", path.display()))?;
        out.insert(name, array);
    }
    Ok(out)
}

fn parse_npy(bytes: &[u8]) -> Result<Array> {
    if bytes.len() < 10 || &bytes[..6] != b"\x93NUMPY" {
        bail!("not a .npy array");
    }
    let (header_len, start) = match bytes[6] {
        1 => (u16::from_le_bytes([bytes[8], bytes[9]]) as usize, 10),
        _ => {
            let b = bytes.get(8..12).context("truncated header")?;
            (u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize, 12)
        }
    };
    let header = std::str::from_utf8(
        bytes
            .get(start..start + header_len)
            .context("truncated header")?,
    )?;
    let body = &bytes[start + header_len..];

    let field = |key: &str| -> Result<&str> {
        let at = header
            .find(key)
            .with_context(|| format!("no {key} in header"))?
            + key.len();
        Ok(header[at..].trim_start_matches([' ', ':']))
    };
    let descr = field("'descr'")?;
    let descr = descr.trim_start_matches(['\'', '"']);
    let descr = &descr[..descr.find(['\'', '"']).context("bad descr")?];
    if field("'fortran_order'")?.starts_with("True") {
        bail!("Fortran-ordered arrays are not supported");
    }
    let shape_text = field("'shape'")?;
    let shape_text = &shape_text[1..shape_text.find(')').context("bad shape")?];
    let shape: Vec<usize> = shape_text
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().context("bad shape"))
        .collect::<Result<_>>()?;
    let count: usize = shape.iter().product();

    if descr.starts_with('>') {
        bail!("big-endian arrays are not supported");
    }
    let kind = descr.trim_start_matches(['<', '|', '=']);
    let need = |size: usize| -> Result<&[u8]> {
        body.get(..count * size).context("array data is truncated")
    };
    let data = match kind {
        "f4" => Data::F32(
            need(4)?
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect(),
        ),
        "f8" => Data::F32(
            need(8)?
                .chunks_exact(8)
                .map(|c| f64::from_le_bytes(c.try_into().unwrap()) as f32)
                .collect(),
        ),
        "f2" => Data::F32(
            need(2)?
                .chunks_exact(2)
                .map(|c| f16_to_f32(u16::from_le_bytes([c[0], c[1]])))
                .collect(),
        ),
        "i8" => Data::I64(
            need(8)?
                .chunks_exact(8)
                .map(|c| i64::from_le_bytes(c.try_into().unwrap()))
                .collect(),
        ),
        "i4" => Data::I64(
            need(4)?
                .chunks_exact(4)
                .map(|c| i32::from_le_bytes(c.try_into().unwrap()) as i64)
                .collect(),
        ),
        "i2" => Data::I64(
            need(2)?
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as i64)
                .collect(),
        ),
        "b1" | "u1" => Data::I64(need(1)?.iter().map(|&b| b as i64).collect()),
        "i1" => Data::I64(need(1)?.iter().map(|&b| b as i8 as i64).collect()),
        other => bail!("unsupported dtype {other}"),
    };
    Ok(Array { shape, data })
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let frac = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if frac == 0 => sign,
        0 => {
            // Subnormal: renormalize.
            let mut e = 127 - 15 + 1;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((f & 0x3ff) << 13)
        }
        0x1f => sign | 0x7f80_0000 | (frac << 13),
        _ => sign | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn npy(descr: &str, shape: &str, body: &[u8]) -> Vec<u8> {
        let mut header =
            format!("{{'descr': '{descr}', 'fortran_order': False, 'shape': {shape}, }}");
        while (10 + header.len() + 1) % 64 != 0 {
            header.push(' ');
        }
        header.push('\n');
        let mut out = b"\x93NUMPY\x01\x00".to_vec();
        out.extend((header.len() as u16).to_le_bytes());
        out.extend(header.as_bytes());
        out.extend(body);
        out
    }

    #[test]
    fn parses_arrays() {
        let body: Vec<u8> = [1.5f32, -2.0, 3.25, 0.0, 1.0, 2.0]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        let a = parse_npy(&npy("<f4", "(2, 3)", &body)).unwrap();
        assert_eq!(a.shape, vec![2, 3]);
        assert_eq!(a.into_f32(), vec![1.5, -2.0, 3.25, 0.0, 1.0, 2.0]);

        let a = parse_npy(&npy("<f8", "()", &0.5f64.to_le_bytes())).unwrap();
        assert!(a.shape.is_empty());
        assert_eq!(a.scalar(), 0.5);

        let body: Vec<u8> = [0x3c00u16, 0xc000, 0x0001]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        let a = parse_npy(&npy("<f2", "(3,)", &body)).unwrap().into_f32();
        assert_eq!(&a[..2], &[1.0, -2.0]);
        assert!((a[2] - 5.960_464_5e-8).abs() < 1e-12);

        assert!(parse_npy(&npy("<f4", "(4,)", &[0; 8])).is_err());
    }
}
