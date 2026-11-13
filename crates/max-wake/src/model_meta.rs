/// io.sdk's streaming backend drops this many outputs after every reset.
pub fn right_context(model: &[u8]) -> Result<usize, String> {
    let fb = Flatbuffer(model);
    let root = fb.table(fb.uoffset(0)?)?;
    let buffers = root.vector(MODEL_BUFFERS)?.unwrap_or_default();
    for entry in root.vector(MODEL_METADATA)?.unwrap_or_default() {
        let meta = fb.table(entry)?;
        if meta.string(METADATA_NAME)? != Some(b"right_context".as_slice()) {
            continue;
        }
        let index = meta.scalar_u32(METADATA_BUFFER)?.unwrap_or(0);
        let buffer = buffers
            .get(usize::try_from(index).map_err(|_| "right_context: bad buffer")?)
            .ok_or("right_context: bad buffer")?;
        let data = fb.table(*buffer)?.bytes(BUFFER_DATA)?.unwrap_or_default();
        if data.is_empty() || data.len() > 8 {
            return Err(format!("right_context: {} bytes", data.len()));
        }
        let mut le = [0u8; 8];
        le[..data.len()].copy_from_slice(data);
        return usize::try_from(u64::from_le_bytes(le)).map_err(|e| e.to_string());
    }
    Err("model has no right_context metadata".into())
}

// Field numbers from tensorflow/lite/schema/schema.fbs.
const MODEL_BUFFERS: usize = 4;
const MODEL_METADATA: usize = 6;
const METADATA_NAME: usize = 0;
const METADATA_BUFFER: usize = 1;
const BUFFER_DATA: usize = 0;

#[derive(Clone, Copy)]
struct Flatbuffer<'a>(&'a [u8]);

#[derive(Clone, Copy)]
struct Table<'a> {
    fb: Flatbuffer<'a>,
    at: usize,
    vtable: usize,
}

impl<'a> Flatbuffer<'a> {
    fn bytes_at<const N: usize>(&self, at: usize) -> Result<[u8; N], String> {
        at.checked_add(N)
            .and_then(|end| self.0.get(at..end))
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| format!("model: truncated at byte {at}"))
    }

    fn u16(&self, at: usize) -> Result<usize, String> {
        Ok(usize::from(u16::from_le_bytes(self.bytes_at(at)?)))
    }

    fn u32(&self, at: usize) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes_at(at)?))
    }

    fn uoffset(&self, at: usize) -> Result<usize, String> {
        let off = usize::try_from(self.u32(at)?).map_err(|e| e.to_string())?;
        at.checked_add(off)
            .ok_or_else(|| "model: bad offset".to_string())
    }

    fn table(&self, at: usize) -> Result<Table<'a>, String> {
        let back = i64::from(i32::from_le_bytes(self.bytes_at(at)?));
        let vtable = usize::try_from(i64::try_from(at).map_err(|e| e.to_string())? - back)
            .map_err(|_| "model: bad vtable".to_string())?;
        self.u16(vtable)?;
        Ok(Table {
            fb: *self,
            at,
            vtable,
        })
    }

    fn vector(&self, at: usize) -> Result<(usize, usize), String> {
        let len = usize::try_from(self.u32(at)?).map_err(|e| e.to_string())?;
        Ok((at + 4, len))
    }
}

impl<'a> Table<'a> {
    fn field(&self, n: usize) -> Result<Option<usize>, String> {
        let vtable_len = self.fb.u16(self.vtable)?;
        let slot = 4 + 2 * n;
        if slot + 2 > vtable_len {
            return Ok(None);
        }
        let off = self.fb.u16(self.vtable + slot)?;
        Ok((off != 0).then_some(self.at + off))
    }

    fn scalar_u32(&self, n: usize) -> Result<Option<u32>, String> {
        self.field(n)?.map(|at| self.fb.u32(at)).transpose()
    }

    fn bytes(&self, n: usize) -> Result<Option<&'a [u8]>, String> {
        let Some(at) = self.field(n)? else {
            return Ok(None);
        };
        let (start, len) = self.fb.vector(self.fb.uoffset(at)?)?;
        start
            .checked_add(len)
            .and_then(|end| self.fb.0.get(start..end))
            .map(Some)
            .ok_or_else(|| "model: truncated vector".to_string())
    }

    fn string(&self, n: usize) -> Result<Option<&'a [u8]>, String> {
        self.bytes(n)
    }

    fn vector(&self, n: usize) -> Result<Option<Vec<usize>>, String> {
        let Some(at) = self.field(n)? else {
            return Ok(None);
        };
        let (start, len) = self.fb.vector(self.fb.uoffset(at)?)?;
        (0..len)
            .map(|k| self.fb.uoffset(start + 4 * k))
            .collect::<Result<_, _>>()
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(metadata: &[(&str, u32)], buffers: &[&[u8]]) -> Vec<u8> {
        let mut b = Vec::new();
        let push_u16 = |b: &mut Vec<u8>, v: usize| b.extend_from_slice(&(v as u16).to_le_bytes());
        let push_u32 = |b: &mut Vec<u8>, v: usize| b.extend_from_slice(&(v as u32).to_le_bytes());
        let patch = |b: &mut Vec<u8>, at: usize, target: usize| {
            b[at..at + 4].copy_from_slice(&((target - at) as u32).to_le_bytes());
        };
        let align = |b: &mut Vec<u8>| b.resize(b.len().next_multiple_of(4), 0);
        let table = |b: &mut Vec<u8>, slots: &[usize], fields: usize| {
            let vtable = b.len();
            push_u16(b, 4 + 2 * slots.len());
            push_u16(b, 4 + 4 * fields);
            for &s in slots {
                push_u16(b, s);
            }
            align(b);
            let at = b.len();
            b.extend_from_slice(&((at - vtable) as i32).to_le_bytes());
            at
        };
        let refs = |b: &mut Vec<u8>, n: usize| {
            push_u32(b, n);
            let first = b.len();
            b.resize(first + 4 * n, 0);
            first
        };

        push_u32(&mut b, 0);
        let root = table(&mut b, &[0, 0, 0, 0, 4, 0, 8], 2);
        patch(&mut b, 0, root);
        b.resize(root + 12, 0);
        let buffers_vec = b.len();
        patch(&mut b, root + 4, buffers_vec);
        let buffer_refs = refs(&mut b, buffers.len());
        let meta_vec = b.len();
        patch(&mut b, root + 8, meta_vec);
        let meta_refs = refs(&mut b, metadata.len());
        for (k, data) in buffers.iter().enumerate() {
            let slot = if data.is_empty() { 0 } else { 4 };
            let t = table(&mut b, &[slot], 1);
            patch(&mut b, buffer_refs + 4 * k, t);
            b.resize(t + 8, 0);
            if !data.is_empty() {
                let data_at = b.len();
                patch(&mut b, t + 4, data_at);
                push_u32(&mut b, data.len());
                b.extend_from_slice(data);
                align(&mut b);
            }
        }
        for (k, (name, buffer)) in metadata.iter().enumerate() {
            let t = table(&mut b, &[4, 8], 2);
            patch(&mut b, meta_refs + 4 * k, t);
            b.resize(t + 12, 0);
            b[t + 8..t + 12].copy_from_slice(&buffer.to_le_bytes());
            let data_at = b.len();
            patch(&mut b, t + 4, data_at);
            push_u32(&mut b, name.len());
            b.extend_from_slice(name.as_bytes());
            b.push(0);
            align(&mut b);
        }
        b
    }

    #[test]
    fn reads_the_look_ahead_of_the_model() {
        let m = model(
            &[("min_runtime_version", 1), ("right_context", 2)],
            &[b"", b"1.15.0\0", &[16]],
        );
        assert_eq!(right_context(&m), Ok(16));
    }

    #[test]
    fn wider_integers_are_little_endian() {
        let m = model(&[("right_context", 0)], &[&[12, 0, 0, 0]]);
        assert_eq!(right_context(&m), Ok(12));
        let m = model(&[("right_context", 0)], &[&[7, 0, 0, 0, 0, 0, 0, 0]]);
        assert_eq!(right_context(&m), Ok(7));
    }

    #[test]
    fn a_model_without_it_is_an_error() {
        let m = model(&[("min_runtime_version", 0)], &[b"1.15.0\0"]);
        assert!(right_context(&m).unwrap_err().contains("no right_context"));
        assert!(right_context(&model(&[], &[])).is_err());
    }

    #[test]
    fn a_model_without_a_metadata_field_is_an_error() {
        let mut m = model(&[("right_context", 0)], &[&[16]]);
        let root = usize::try_from(u32::from_le_bytes(m[0..4].try_into().unwrap())).unwrap();
        let back = i32::from_le_bytes(m[root..root + 4].try_into().unwrap());
        let vtable = usize::try_from(i64::try_from(root).unwrap() - i64::from(back)).unwrap();
        m[vtable..vtable + 2].copy_from_slice(&16u16.to_le_bytes());
        assert!(right_context(&m).unwrap_err().contains("no right_context"));
    }

    #[test]
    fn a_bad_value_or_index_is_an_error() {
        assert!(right_context(&model(&[("right_context", 0)], &[b""])).is_err());
        assert!(right_context(&model(&[("right_context", 0)], &[&[1; 9]])).is_err());
        assert!(right_context(&model(&[("right_context", 3)], &[&[16]])).is_err());
    }

    #[test]
    fn truncated_or_garbage_input_is_an_error_not_a_panic() {
        let m = model(&[("right_context", 0)], &[&[16]]);
        for len in 0..m.len() {
            let _ = right_context(&m[..len]);
        }
        assert!(right_context(&[]).is_err());
        assert!(right_context(&[0xff; 64]).is_err());
        let mut bad_vtable = m.clone();
        let root = usize::try_from(u32::from_le_bytes(m[0..4].try_into().unwrap())).unwrap();
        bad_vtable[root..root + 4].copy_from_slice(&i32::MIN.to_le_bytes());
        assert!(right_context(&bad_vtable).is_err());
    }
}
