use std::ffi::CStr;

// Apple xnu bsd/sys/attr.h. Kept here so the exact bounds-checked wire parser
// can be tested on hosts without a Darwin runtime; native tests check libc too.
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
// Darwin <sys/attr.h>: libc exposes the other request bits but not CMN_ERROR.
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
pub(crate) const COMMON: u32 = ATTR_CMN_RETURNED_ATTRS
    | ATTR_CMN_ERROR
    | ATTR_CMN_NAME
    | ATTR_CMN_DEVID
    | ATTR_CMN_OBJTYPE
    | ATTR_CMN_FILEID;
pub(crate) const FILE: u32 = ATTR_FILE_LINKCOUNT | ATTR_FILE_DATALENGTH | ATTR_FILE_ALLOCSIZE;
pub(crate) const VREG: u32 = 1;
pub(crate) const VDIR: u32 = 2;
pub(crate) const VBLK: u32 = 3;
pub(crate) const VCHR: u32 = 4;
pub(crate) const VLNK: u32 = 5;
pub(crate) const VSOCK: u32 = 6;
pub(crate) const VFIFO: u32 = 7;

#[derive(Debug, Default)]
pub(crate) struct Entry<'a> {
    pub(crate) name: Option<&'a CStr>,
    pub(crate) error: u32,
    pub(crate) kind: Option<u32>,
    pub(crate) device: Option<u64>,
    pub(crate) inode: Option<u64>,
    pub(crate) links: Option<u32>,
    pub(crate) logical: Option<u64>,
    pub(crate) physical: Option<u64>,
}

fn word<const N: usize>(record: &[u8], offset: usize) -> Result<[u8; N], &'static str> {
    record
        .get(offset..offset.checked_add(N).ok_or("attribute offset overflow")?)
        .and_then(|s| s.try_into().ok())
        .ok_or("truncated attribute")
}
pub(crate) fn u32_at(record: &[u8], offset: usize) -> Result<u32, &'static str> {
    Ok(u32::from_ne_bytes(word(record, offset)?))
}
fn size_at(record: &[u8], offset: usize) -> Result<u64, &'static str> {
    u64::try_from(i64::from_ne_bytes(word(record, offset)?)).map_err(|_| "negative file size")
}

/// FSOPT_PACK_INVAL_ATTRS fixes offsets within each attribute group; masks say
/// whether values are valid. Directory records omit the file group even with
/// that option (xnu bsd/vfs/vfs_attrlist.c, getattrlist_setupvattr/attr_pack_file).
/// All fields are packed on four-byte boundaries, including 64-bit integers.
pub(crate) fn parse_record(record: &[u8]) -> Result<Entry<'_>, &'static str> {
    let common = u32_at(record, 4)?;
    let file = u32_at(record, 16)?;
    if common & (ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME) != ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME
    {
        return Err("missing returned attributes or entry name");
    }
    let error = if common & ATTR_CMN_ERROR != 0 {
        u32_at(record, 24)?
    } else {
        0
    };
    let kind = if common & ATTR_CMN_OBJTYPE != 0 {
        Some(u32_at(record, 40)?)
    } else {
        None
    };
    // attr_dataoffset is relative to the attrreference at byte 28, NOT the record.
    let relative = i32::from_ne_bytes(word(record, 28)?);
    let start = 28usize
        .checked_add(usize::try_from(relative).map_err(|_| "negative name offset")?)
        .ok_or("name offset overflow")?;
    let length = u32_at(record, 32)? as usize;
    let end = start.checked_add(length).ok_or("name length overflow")?;
    let fixed_size = if error == 0 && kind.is_some() && kind != Some(VDIR) {
        72
    } else {
        52
    };
    if start < fixed_size {
        return Err("name overlaps fixed attributes");
    }
    let name = CStr::from_bytes_with_nul(record.get(start..end).ok_or("name outside record")?)
        .map_err(|_| "entry name is not NUL terminated or contains NUL")?;
    if name.to_bytes().is_empty() || name.to_bytes().contains(&b'/') {
        return Err("entry name is not a basename");
    }
    let mut entry = Entry {
        name: Some(name),
        error,
        kind,
        ..Entry::default()
    };
    if error != 0 {
        return Ok(entry);
    }
    if common & ATTR_CMN_DEVID != 0 {
        entry.device = Some(i32::from_ne_bytes(word(record, 36)?) as u64);
    }
    if common & ATTR_CMN_FILEID != 0 {
        entry.inode = Some(u64::from_ne_bytes(word(record, 44)?));
    }
    if kind.is_some() && kind != Some(VDIR) {
        if file & ATTR_FILE_LINKCOUNT != 0 {
            entry.links = Some(u32_at(record, 52)?);
        }
        if file & ATTR_FILE_DATALENGTH != 0 {
            entry.logical = Some(size_at(record, 64)?);
        }
        if file & ATTR_FILE_ALLOCSIZE != 0 {
            entry.physical = Some(size_at(record, 56)?);
        }
    }
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
    }
    fn fixture(kind: u32, name: &[u8]) -> Vec<u8> {
        let fixed = if kind == VDIR { 52 } else { 72 };
        let mut bytes = vec![0; (fixed + name.len() + 1 + 7) & !7];
        let length = bytes.len() as u32;
        put32(&mut bytes, 0, length);
        put32(&mut bytes, 4, COMMON);
        put32(&mut bytes, 16, if kind == VDIR { 0 } else { FILE });
        put32(&mut bytes, 28, (fixed - 28) as u32);
        put32(&mut bytes, 32, name.len() as u32 + 1);
        put32(&mut bytes, 36, 7);
        put32(&mut bytes, 40, kind);
        bytes[44..52].copy_from_slice(&0x1_0000_0002u64.to_ne_bytes());
        if kind != VDIR {
            put32(&mut bytes, 52, 2);
            bytes[64..72].copy_from_slice(&8192i64.to_ne_bytes());
        }
        bytes[fixed..fixed + name.len()].copy_from_slice(name);
        bytes
    }

    #[test]
    fn special_file_types_remain_distinct_from_regular_files() {
        for kind in [VBLK, VCHR, VSOCK, VFIFO] {
            let bytes = fixture(kind, b"special");
            let entry = parse_record(&bytes).unwrap();
            assert_eq!(entry.kind, Some(kind));
            assert_ne!(entry.kind, Some(VREG));
        }
    }

    #[test]
    fn packed_file_uses_reference_address_and_retains_zero_allocation() {
        let bytes = fixture(VREG, b"sparse");
        let entry = parse_record(&bytes).unwrap();
        assert_eq!(entry.name.unwrap().to_bytes(), b"sparse");
        assert_eq!(entry.kind, Some(VREG));
        assert_eq!(entry.device, Some(7));
        assert_eq!(entry.inode, Some(0x1_0000_0002));
        assert_eq!(entry.links, Some(2));
        assert_eq!(entry.logical, Some(8192));
        assert_eq!(entry.physical, Some(0));
    }

    #[test]
    fn data_fork_length_and_all_fork_allocation_have_distinct_offsets() {
        let mut bytes = fixture(VREG, b"resource-fork");
        bytes[56..64].copy_from_slice(&16384i64.to_ne_bytes());
        bytes[64..72].copy_from_slice(&7i64.to_ne_bytes());
        let entry = parse_record(&bytes).unwrap();
        assert_eq!(entry.logical, Some(7));
        assert_eq!(entry.physical, Some(16384));
        assert_eq!(
            FILE & 0x0000_0002,
            0,
            "TOTALSIZE must not stand in for st_size"
        );
    }
    #[test]
    fn directories_have_no_file_attribute_slots() {
        let bytes = fixture(VDIR, b"child");
        let entry = parse_record(&bytes).unwrap();
        assert_eq!(entry.kind, Some(VDIR));
        assert_eq!(entry.name.unwrap().to_bytes(), b"child");
        assert_eq!(entry.inode, Some(0x1_0000_0002));
        assert!(entry.links.is_none() && entry.logical.is_none() && entry.physical.is_none());
    }

    #[test]
    fn missing_attributes_are_not_zero_values() {
        let mut bytes = fixture(VREG, b"missing");
        put32(&mut bytes, 4, COMMON & !(ATTR_CMN_DEVID | ATTR_CMN_FILEID));
        put32(
            &mut bytes,
            16,
            FILE & !(ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE),
        );
        let entry = parse_record(&bytes).unwrap();
        assert!(entry.device.is_none() && entry.inode.is_none() && entry.links.is_none());
        assert_eq!(entry.logical, Some(8192));
        assert_eq!(entry.physical, None);
    }

    #[test]
    fn error_record_reads_errno_before_name_and_ignores_invalid_slots() {
        let mut bytes = fixture(VDIR, b"denied");
        put32(
            &mut bytes,
            4,
            ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_ERROR,
        );
        put32(&mut bytes, 24, 13);
        let entry = parse_record(&bytes).unwrap();
        assert_eq!(entry.error, 13);
        assert_eq!(entry.name.unwrap().to_bytes(), b"denied");
        assert!(entry.kind.is_none() && entry.logical.is_none());
    }

    #[test]
    fn truncated_records_and_invalid_references_are_rejected() {
        let bytes = fixture(VREG, b"file");
        for end in 0..77 {
            assert!(
                parse_record(&bytes[..end]).is_err(),
                "accepted truncated end={end}"
            );
        }
        for (offset, value) in [
            (28, u32::MAX),
            (28, 0),
            (28, i32::MAX as u32),
            (32, 0),
            (32, u32::MAX),
            (4, COMMON & !ATTR_CMN_NAME),
        ] {
            let mut malformed = bytes.clone();
            put32(&mut malformed, offset, value);
            assert!(
                parse_record(&malformed).is_err(),
                "accepted offset={offset} value={value}"
            );
        }
    }

    #[test]
    fn invalid_names_and_negative_sizes_are_rejected() {
        for name in [b"".as_slice(), b"a/b", b"a\0b"] {
            assert!(parse_record(&fixture(VREG, name)).is_err());
        }
        let mut bytes = fixture(VREG, b"file");
        bytes[76] = b'x';
        assert!(parse_record(&bytes).is_err());
        for offset in [56, 64] {
            let mut bytes = fixture(VREG, b"file");
            bytes[offset..offset + 8].copy_from_slice(&(-1i64).to_ne_bytes());
            assert!(parse_record(&bytes).is_err());
        }
    }

    #[test]
    fn symlink_type_and_zero_logical_size_are_not_missing() {
        let mut bytes = fixture(VLNK, b"link");
        bytes[64..72].copy_from_slice(&0i64.to_ne_bytes());
        let entry = parse_record(&bytes).unwrap();
        assert_eq!(entry.kind, Some(VLNK));
        assert_eq!(entry.logical, Some(0));
    }
}
