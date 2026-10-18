//! Just enough of the protobuf wire format to read the few fields Pitstop
//! needs from what Antigravity stores; nothing is ever written in it.

/// One field's value, as the wire carries it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wire<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

impl<'a> Wire<'a> {
    pub fn bytes(self) -> Option<&'a [u8]> {
        match self {
            Wire::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn text(self) -> Option<&'a str> {
        std::str::from_utf8(self.bytes()?).ok()
    }

    pub fn varint(self) -> Option<u64> {
        match self {
            Wire::Varint(v) => Some(v),
            _ => None,
        }
    }

    /// A `float` field.
    pub fn float(self) -> Option<f32> {
        match self {
            Wire::Fixed32(bits) => Some(f32::from_bits(bits)),
            _ => None,
        }
    }
}

fn varint(buf: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *buf.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// Every field of a message in order, or None when it isn't one: what
/// fails to parse is never half read.
pub fn fields(buf: &[u8]) -> Option<Vec<(u32, Wire<'_>)>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < buf.len() {
        let tag = varint(buf, &mut at)?;
        let number = u32::try_from(tag >> 3).ok().filter(|&n| n > 0)?;
        let wire = match tag & 7 {
            0 => Wire::Varint(varint(buf, &mut at)?),
            1 => {
                let bytes = buf.get(at..at + 8)?;
                at += 8;
                Wire::Fixed64(u64::from_le_bytes(bytes.try_into().ok()?))
            }
            2 => {
                let len = usize::try_from(varint(buf, &mut at)?).ok()?;
                let bytes = buf.get(at..at.checked_add(len)?)?;
                at += len;
                Wire::Bytes(bytes)
            }
            5 => {
                let bytes = buf.get(at..at + 4)?;
                at += 4;
                Wire::Fixed32(u32::from_le_bytes(bytes.try_into().ok()?))
            }
            _ => return None,
        };
        out.push((number, wire));
    }
    Some(out)
}

/// The first field `number` of a message.
pub fn field(buf: &[u8], number: u32) -> Option<Wire<'_>> {
    fields(buf)?
        .into_iter()
        .find_map(|(n, w)| (n == number).then_some(w))
}

/// Every field `number` of a message (a repeated field).
pub fn repeated(buf: &[u8], number: u32) -> Vec<Wire<'_>> {
    fields(buf)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(n, w)| (n == number).then_some(w))
        .collect()
}

/// Writing, for tests that need a message to read.
#[cfg(test)]
pub mod write {
    fn varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    pub fn bytes(number: u32, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        varint(u64::from(number) << 3 | 2, &mut out);
        varint(value.len() as u64, &mut out);
        out.extend_from_slice(value);
        out
    }

    pub fn uint(number: u32, value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        varint(u64::from(number) << 3, &mut out);
        varint(value, &mut out);
        out
    }

    pub fn float(number: u32, value: f32) -> Vec<u8> {
        let mut out = Vec::new();
        varint(u64::from(number) << 3 | 5, &mut out);
        out.extend_from_slice(&value.to_bits().to_le_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_kind_of_field_and_refuses_what_isnt_a_message() {
        let msg = [
            write::bytes(1, b"label"),
            write::uint(2, 300),
            write::float(15, 0.8),
            write::bytes(1, b"again"),
        ]
        .concat();
        assert_eq!(field(&msg, 1).and_then(Wire::text), Some("label"));
        assert_eq!(field(&msg, 2).and_then(Wire::varint), Some(300));
        assert_eq!(field(&msg, 15).and_then(Wire::float), Some(0.8));
        assert_eq!(repeated(&msg, 1).len(), 2);
        assert_eq!(field(&msg, 9), None);
        // cut short in the middle of a field
        assert_eq!(fields(&msg[..msg.len() - 2]), None);
        assert_eq!(fields(b"\x0a\xff"), None);
    }
}
