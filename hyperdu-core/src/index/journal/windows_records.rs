//! Checked USN V2/V3 decoding. Never cast a variable-length kernel record.
//! Layout: Microsoft USN_RECORD_V2 / USN_RECORD_V3 (winioctl.h).

use std::io;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Record {
    pub id: [u8; 16],
    pub parent: [u8; 16],
    pub name: Vec<u16>,
    pub reason: u32,
    pub is_directory: bool,
}

/// The leading USN and every record belong to the same cursor transaction.
/// Unknown versions and truncated records invalidate the whole batch.
pub(super) fn parse_batch(bytes: &[u8], start: i64) -> io::Result<(i64, Vec<Record>)> {
    if start < 0 || bytes.len() < 8 || bytes.len() > 1024 * 1024 {
        return Err(invalid("invalid USN batch size or cursor"));
    }
    let next = i64::from_le_bytes(bytes[..8].try_into().unwrap());
    if next < start {
        return Err(invalid("USN cursor moved backwards"));
    }
    let mut rest = &bytes[8..];
    let mut records = Vec::new();
    let mut previous = start;
    while !rest.is_empty() {
        if rest.len() < 8 {
            return Err(invalid("truncated USN record header"));
        }
        let length = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        let version = u16::from_le_bytes(rest[4..6].try_into().unwrap());
        let (header, id_len, parent_offset, usn_offset, reason_offset) = match version {
            2 => (60, 8, 16, 24, 40),
            3 => (76, 16, 24, 40, 56),
            _ => return Err(invalid("unsupported USN record version")),
        };
        if length < header || length % 8 != 0 || length > rest.len() {
            return Err(invalid("invalid USN record length"));
        }
        let record = &rest[..length];
        let usn = i64::from_le_bytes(record[usn_offset..usn_offset + 8].try_into().unwrap());
        if usn < previous || usn >= next {
            return Err(invalid("USN record outside cursor interval"));
        }
        previous = usn;
        let name_len =
            u16::from_le_bytes(record[header - 4..header - 2].try_into().unwrap()) as usize;
        let name_offset =
            u16::from_le_bytes(record[header - 2..header].try_into().unwrap()) as usize;
        if name_len == 0
            || name_len % 2 != 0
            || name_offset < header
            || name_offset % 2 != 0
            || name_offset
                .checked_add(name_len)
                .map_or(true, |end| end > length)
        {
            return Err(invalid("invalid USN filename bounds"));
        }
        let name: Vec<u16> = record[name_offset..name_offset + name_len]
            .chunks_exact(2)
            .map(|word| u16::from_le_bytes([word[0], word[1]]))
            .collect();
        if name.iter().any(|ch| matches!(*ch, 0 | 47 | 58 | 92)) || name == [46] || name == [46, 46]
        {
            return Err(invalid("USN filename is not a single component"));
        }
        let mut id = [0; 16];
        let mut parent = [0; 16];
        id[..id_len].copy_from_slice(&record[8..8 + id_len]);
        parent[..id_len].copy_from_slice(&record[parent_offset..parent_offset + id_len]);
        let reason =
            u32::from_le_bytes(record[reason_offset..reason_offset + 4].try_into().unwrap());
        let attributes = u32::from_le_bytes(record[header - 8..header - 4].try_into().unwrap());
        records.push(Record {
            id,
            parent,
            name,
            reason,
            is_directory: attributes & 0x10 != 0,
        });
        rest = &rest[length..];
    }
    Ok((next, records))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(version: u16, name: &[u16]) -> Vec<u8> {
        let header = if version == 2 { 60 } else { 76 };
        let length = (header + name.len() * 2 + 7) & !7;
        let mut bytes = vec![0; length + 8];
        bytes[..8].copy_from_slice(&101_i64.to_le_bytes());
        let record = &mut bytes[8..];
        record[..4].copy_from_slice(&(length as u32).to_le_bytes());
        record[4..6].copy_from_slice(&version.to_le_bytes());
        let (id_len, parent_offset, usn_offset, reason_offset) = if version == 2 {
            (8, 16, 24, 40)
        } else {
            (16, 24, 40, 56)
        };
        record[8..8 + id_len].fill(0x12);
        record[parent_offset..parent_offset + id_len].fill(0x34);
        record[usn_offset..usn_offset + 8].copy_from_slice(&100_i64.to_le_bytes());
        record[reason_offset..reason_offset + 4].copy_from_slice(&0x2000_u32.to_le_bytes());
        record[header - 8..header - 4].copy_from_slice(&0x10_u32.to_le_bytes());
        record[header - 4..header - 2].copy_from_slice(&((name.len() * 2) as u16).to_le_bytes());
        record[header - 2..header].copy_from_slice(&(header as u16).to_le_bytes());
        for (slot, word) in record[header..].chunks_exact_mut(2).zip(name) {
            slot.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn versions_preserve_full_identity_and_unpaired_utf16() {
        for version in [2, 3] {
            let bytes = batch(version, &[0x61, 0xd800]);
            let (next, records) = parse_batch(&bytes, 99).unwrap();
            assert_eq!(next, 101);
            assert_eq!(records.len(), 1);
            let record = &records[0];
            let len = if version == 2 { 8 } else { 16 };
            assert_eq!(record.id[..len], vec![0x12; len]);
            assert_eq!(record.parent[..len], vec![0x34; len]);
            assert_eq!(record.id[len..], vec![0; 16 - len]);
            assert_eq!(record.name, [0x61, 0xd800]);
            assert_eq!(record.reason, 0x2000);
            assert!(record.is_directory);
        }
    }

    #[test]
    fn truncation_bad_offsets_unknown_versions_and_path_escape_fail_closed() {
        for version in [2, 3] {
            let bytes = batch(version, &[0x61]);
            for end in 0..bytes.len() {
                // A leading cursor with no records is a valid empty batch.
                if end != 8 {
                    assert!(parse_batch(&bytes[..end], 99).is_err(), "length {end}");
                }
            }
            let header = if version == 2 { 60 } else { 76 };
            for offset in [0_u16, 1, 8, 65534] {
                let mut malformed = bytes.clone();
                malformed[8 + header - 2..8 + header].copy_from_slice(&offset.to_le_bytes());
                assert!(parse_batch(&malformed, 99).is_err());
            }
            for name in [
                vec![0],
                vec![47],
                vec![92],
                vec![58],
                vec![46],
                vec![46, 46],
            ] {
                assert!(parse_batch(&batch(version, &name), 99).is_err());
            }
            let mut unknown = bytes.clone();
            unknown[12..14].copy_from_slice(&4_u16.to_le_bytes());
            assert!(parse_batch(&unknown, 99).is_err());
        }
    }

    #[test]
    fn cursor_validation_is_transactional() {
        let mut bytes = batch(2, &[0x61]);
        assert!(parse_batch(&bytes, 102).is_err());
        assert!(parse_batch(&bytes, -1).is_err());
        bytes[..8].copy_from_slice(&100_i64.to_le_bytes());
        assert!(parse_batch(&bytes, 99).is_err());
        let empty = 99_i64.to_le_bytes();
        assert_eq!(parse_batch(&empty, 99).unwrap(), (99, vec![]));
    }
}
