//! Parsing of NTFS on-disk structures: the boot sector and MFT records.
//!
//! This module is pure parsing over byte slices. It opens no volume and issues
//! no syscalls, so every case below is exercised by unit tests over synthetic
//! records -- including the ones that are awkward to produce on a real volume
//! (a torn write, a name that runs past the record, a zero-length attribute).
//! Reading an actual `\\.\C:` needs administrator rights and lives in the
//! caller.
//!
//! Why MFT parsing is worth the complexity: an MFT record's `$FILE_NAME`
//! attribute carries the *parent* directory reference, so name and size join
//! without walking the tree. The cost is one sequential read of the MFT rather
//! than one random access per file.
//!
//! Offsets are named constants rather than magic numbers so a mismatch against
//! the documented layout is greppable.

// Nothing here is called from the scan yet -- `process_dir` still goes to `nt`
// or `win32`. The parser lands first, with its tests, so that the volume I/O
// that follows is written against something already known to be correct.
// Remove this once the backend is wired up.
#![allow(dead_code)]

/// Signature at the head of every MFT record.
const RECORD_SIGNATURE: &[u8; 4] = b"FILE";

/// Attribute type codes. Only the ones needed for sizes are listed.
pub(crate) mod attr_type {
    pub(crate) const ATTRIBUTE_LIST: u32 = 0x20;
    pub(crate) const FILE_NAME: u32 = 0x30;
    pub(crate) const DATA: u32 = 0x80;
    /// Terminates the attribute chain.
    pub(crate) const END: u32 = 0xFFFF_FFFF;
}

/// `$FILE_NAME` namespaces. A file often has two entries -- a Win32 name and a
/// DOS 8.3 name -- for the *same* link, so counting namespaces as links would
/// double-count.
pub(crate) mod namespace {
    pub(crate) const POSIX: u8 = 0;
    pub(crate) const WIN32: u8 = 1;
    pub(crate) const DOS: u8 = 2;
    pub(crate) const WIN32_AND_DOS: u8 = 3;
}

/// Record header flags.
const FLAG_IN_USE: u16 = 0x0001;
const FLAG_DIRECTORY: u16 = 0x0002;

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

// --- boot sector -------------------------------------------------------------

/// Geometry needed to locate and size MFT records, read from the boot sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Geometry {
    pub(crate) bytes_per_sector: u32,
    pub(crate) sectors_per_cluster: u32,
    /// Byte offset of the MFT from the start of the volume.
    pub(crate) mft_offset: u64,
    pub(crate) record_size: u32,
}

impl Geometry {
    pub(crate) fn cluster_size(&self) -> u32 {
        self.bytes_per_sector * self.sectors_per_cluster
    }
}

/// Parse the NTFS boot sector.
///
/// Returns `None` for anything that is not a plausible NTFS volume rather than
/// guessing: the caller falls back to directory enumeration, and a wrong guess
/// here would be read as a real answer.
pub(crate) fn parse_boot_sector(boot: &[u8]) -> Option<Geometry> {
    if boot.len() < 84 || boot.get(3..11)? != b"NTFS    " {
        return None;
    }
    let bytes_per_sector = u16_at(boot, 11)? as u32;
    // Every other offset here assumes a power-of-two sector size.
    if !(512..=4096).contains(&bytes_per_sector) || !bytes_per_sector.is_power_of_two() {
        return None;
    }

    // Since Windows 10 a negative value is a base-2 shift rather than a count.
    let sectors_per_cluster = decode_shift_or_count(*boot.get(13)? as i8, 1)?;
    let cluster_size = bytes_per_sector.checked_mul(sectors_per_cluster)?;

    let mft_lcn = u64_at(boot, 48)?;
    let mft_offset = mft_lcn.checked_mul(cluster_size as u64)?;

    let record_size = decode_shift_or_count(*boot.get(64)? as i8, cluster_size)?;
    // A record holds at least a header and one attribute, and is not megabytes.
    if !(48..=(1 << 20)).contains(&record_size) {
        return None;
    }

    Some(Geometry {
        bytes_per_sector,
        sectors_per_cluster,
        mft_offset,
        record_size,
    })
}

/// NTFS stores two fields as "a count of `unit`, or a base-2 shift when
/// negative". Getting this wrong yields a plausible-looking but wrong geometry,
/// so it is factored out and tested directly.
fn decode_shift_or_count(raw: i8, unit: u32) -> Option<u32> {
    match raw {
        0 => None,
        n if n < 0 => {
            let shift = (-(n as i32)) as u32;
            if shift > 31 {
                return None;
            }
            Some(1u32 << shift)
        }
        n => (n as u32).checked_mul(unit),
    }
}

// --- fixups ------------------------------------------------------------------

/// Undo the update-sequence fixups an MFT record carries.
///
/// NTFS overwrites the last two bytes of every sector with a sequence number so
/// a torn write is detectable, and keeps the real bytes in an array at the head
/// of the record. Parsing without restoring them silently reads the sequence
/// number as data, which is why this runs before anything else looks at the
/// record.
///
/// Returns false when the record fails the torn-write check, in which case the
/// record must not be trusted.
pub(crate) fn apply_fixups(rec: &mut [u8], bytes_per_sector: u32) -> bool {
    let (Some(usa_off), Some(usa_count)) = (
        u16_at(rec, 4).map(usize::from),
        u16_at(rec, 6).map(usize::from),
    ) else {
        return false;
    };
    // usa_count counts the sequence number plus one entry per sector.
    if usa_count == 0 {
        return false;
    }
    let sectors = usa_count - 1;
    let sector_size = bytes_per_sector as usize;
    if sectors == 0 || sector_size < 2 || rec.len() < sectors * sector_size {
        return false;
    }
    // The array itself must fit inside the record.
    match usa_off.checked_add(usa_count * 2) {
        Some(end) if end <= rec.len() => {}
        _ => return false,
    }

    let Some(seq) = u16_at(rec, usa_off) else {
        return false;
    };
    for i in 0..sectors {
        let tail = (i + 1) * sector_size - 2;
        let (Some(found), Some(orig)) = (u16_at(rec, tail), u16_at(rec, usa_off + 2 + i * 2)) else {
            return false;
        };
        if found != seq {
            // Torn write, or not the record we think it is.
            return false;
        }
        rec[tail..tail + 2].copy_from_slice(&orig.to_le_bytes());
    }
    true
}

// --- record header -----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecordHeader {
    pub(crate) first_attr_offset: usize,
    pub(crate) used_size: u32,
    pub(crate) in_use: bool,
    pub(crate) is_directory: bool,
    pub(crate) hard_link_count: u16,
}

/// Parse an MFT record header. Call [`apply_fixups`] first.
pub(crate) fn parse_record_header(rec: &[u8]) -> Option<RecordHeader> {
    if rec.len() < 48 || rec.get(0..4)? != RECORD_SIGNATURE {
        return None;
    }
    let hard_link_count = u16_at(rec, 18)?;
    let first_attr_offset = u16_at(rec, 20)? as usize;
    let flags = u16_at(rec, 22)?;
    let used_size = u32_at(rec, 24)?;

    // An offset outside the record, or a used size larger than the record,
    // means this is not the record it claims to be.
    if first_attr_offset < 48 || first_attr_offset >= rec.len() {
        return None;
    }
    if (used_size as usize) > rec.len() {
        return None;
    }

    Some(RecordHeader {
        first_attr_offset,
        used_size,
        in_use: flags & FLAG_IN_USE != 0,
        is_directory: flags & FLAG_DIRECTORY != 0,
        hard_link_count,
    })
}

// --- attributes --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AttrHeader {
    pub(crate) type_code: u32,
    /// Offset of this attribute within the record.
    pub(crate) pos: usize,
    pub(crate) total_length: usize,
    pub(crate) non_resident: bool,
    /// Offset of the value within the record, for resident attributes.
    pub(crate) value_offset: usize,
    pub(crate) value_length: usize,
}

/// Walks the attribute chain of a record.
///
/// Stops at the end marker, at the used size, or at the first malformed header.
/// A zero or backwards length would otherwise loop forever on a corrupt record,
/// and the MFT of a failing disk is exactly where that shows up.
pub(crate) struct Attributes<'a> {
    rec: &'a [u8],
    pos: usize,
    limit: usize,
    done: bool,
}

impl<'a> Attributes<'a> {
    pub(crate) fn new(rec: &'a [u8], header: &RecordHeader) -> Self {
        Self {
            rec,
            pos: header.first_attr_offset,
            limit: (header.used_size as usize).min(rec.len()),
            done: false,
        }
    }
}

impl Iterator for Attributes<'_> {
    type Item = AttrHeader;

    fn next(&mut self) -> Option<AttrHeader> {
        if self.done || self.pos + 8 > self.limit {
            return None;
        }
        let type_code = u32_at(self.rec, self.pos)?;
        if type_code == attr_type::END {
            self.done = true;
            return None;
        }
        let total_length = u32_at(self.rec, self.pos + 4)? as usize;
        // Zero-length would spin; a length past the record is corrupt.
        if total_length < 16 || self.pos + total_length > self.limit {
            self.done = true;
            return None;
        }
        let non_resident = *self.rec.get(self.pos + 8)? != 0;

        let (value_offset, value_length) = if non_resident {
            (0, 0)
        } else {
            let off = u16_at(self.rec, self.pos + 20)? as usize;
            let len = u32_at(self.rec, self.pos + 16)? as usize;
            // The value must lie inside this attribute.
            if off < 16 || off + len > total_length {
                self.done = true;
                return None;
            }
            (self.pos + off, len)
        };

        let out = AttrHeader {
            type_code,
            pos: self.pos,
            total_length,
            non_resident,
            value_offset,
            value_length,
        };
        self.pos += total_length;
        Some(out)
    }
}

// --- $FILE_NAME --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileName {
    /// Record number of the parent directory. The sequence number is dropped;
    /// only the low 48 bits identify the record.
    pub(crate) parent: u64,
    pub(crate) allocated_size: u64,
    pub(crate) real_size: u64,
    pub(crate) namespace: u8,
    pub(crate) name: String,
}

impl FileName {
    /// True when this entry is a DOS 8.3 alias of another `$FILE_NAME` on the
    /// same record. Counting it as a link would double-count the file.
    pub(crate) fn is_dos_alias(&self) -> bool {
        self.namespace == namespace::DOS
    }
}

/// Parse a `$FILE_NAME` attribute value.
pub(crate) fn parse_file_name(value: &[u8]) -> Option<FileName> {
    if value.len() < 66 {
        return None;
    }
    // Low 48 bits are the record number; the high 16 are a reuse sequence.
    let parent = u64_at(value, 0)? & 0x0000_FFFF_FFFF_FFFF;
    let allocated_size = u64_at(value, 40)?;
    let real_size = u64_at(value, 48)?;
    let name_len = *value.get(64)? as usize;
    let ns = *value.get(65)?;

    let name_bytes = value.get(66..66 + name_len * 2)?;
    let units: Vec<u16> = name_bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    // Lossy on purpose: an unpaired surrogate on disk must not cost the whole
    // record, and the name is only used for display and grouping.
    let name = String::from_utf16_lossy(&units);

    Some(FileName {
        parent,
        allocated_size,
        real_size,
        namespace: ns,
        name,
    })
}

// --- $DATA -------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DataSizes {
    pub(crate) real_size: u64,
    pub(crate) allocated_size: u64,
}

/// Sizes from a `$DATA` attribute.
///
/// `$FILE_NAME` also carries sizes, but they are only refreshed when the name is
/// written, so a growing file reports a stale size there. `$DATA` is the
/// authority; `$FILE_NAME` is the fallback for records where `$DATA` lives in an
/// extension record.
pub(crate) fn parse_data_sizes(rec: &[u8], attr: &AttrHeader) -> Option<DataSizes> {
    if !attr.non_resident {
        // A small file lives inside the record and occupies no clusters of its
        // own, so reporting the cluster size here would overcount every tiny
        // file on the volume.
        return Some(DataSizes {
            real_size: attr.value_length as u64,
            allocated_size: attr.value_length as u64,
        });
    }
    // Non-resident header: allocated at 0x28, real at 0x30. For compressed or
    // sparse data the allocated size is what the volume actually spends, which
    // is the number `du`-style tools want.
    Some(DataSizes {
        allocated_size: u64_at(rec, attr.pos + 0x28)?,
        real_size: u64_at(rec, attr.pos + 0x30)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- boot sector ---------------------------------------------------------

    fn boot_sector(bps: u16, spc: i8, mft_lcn: u64, cpr: i8) -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[3..11].copy_from_slice(b"NTFS    ");
        b[11..13].copy_from_slice(&bps.to_le_bytes());
        b[13] = spc as u8;
        b[48..56].copy_from_slice(&mft_lcn.to_le_bytes());
        b[64] = cpr as u8;
        b
    }

    #[test]
    fn parses_a_typical_ntfs_boot_sector() {
        // 512 B/sector, 8 sectors/cluster = 4 KiB clusters, 1 KiB records.
        let g = parse_boot_sector(&boot_sector(512, 8, 786432, -10)).expect("should parse");
        assert_eq!(g.bytes_per_sector, 512);
        assert_eq!(g.sectors_per_cluster, 8);
        assert_eq!(g.cluster_size(), 4096);
        assert_eq!(g.record_size, 1024);
        assert_eq!(g.mft_offset, 786432 * 4096);
    }

    #[test]
    fn rejects_a_non_ntfs_volume() {
        let mut b = boot_sector(512, 8, 100, -10);
        b[3..11].copy_from_slice(b"FAT32   ");
        assert!(parse_boot_sector(&b).is_none());
    }

    #[test]
    fn rejects_an_implausible_sector_size() {
        assert!(parse_boot_sector(&boot_sector(999, 8, 100, -10)).is_none());
        assert!(parse_boot_sector(&boot_sector(0, 8, 100, -10)).is_none());
    }

    #[test]
    fn rejects_a_zero_sectors_per_cluster() {
        assert!(parse_boot_sector(&boot_sector(512, 0, 100, -10)).is_none());
    }

    #[test]
    fn handles_the_shift_encoding_for_large_clusters() {
        // -12 means 2^12 sectors per cluster, not "12 of them".
        let g = parse_boot_sector(&boot_sector(512, -12, 0, -10)).expect("should parse");
        assert_eq!(g.sectors_per_cluster, 4096);
    }

    #[test]
    fn decodes_a_count_and_a_shift_differently() {
        assert_eq!(decode_shift_or_count(8, 1), Some(8));
        assert_eq!(decode_shift_or_count(-10, 1), Some(1024));
        assert_eq!(decode_shift_or_count(0, 1), None);
        assert_eq!(decode_shift_or_count(-99, 1), None, "shift out of range");
    }

    #[test]
    fn rejects_a_truncated_boot_sector() {
        assert!(parse_boot_sector(&[0u8; 32]).is_none());
    }

    // --- fixups --------------------------------------------------------------

    /// A two-sector record whose sector tails hold the sequence number, with the
    /// real bytes stashed in the update-sequence array.
    fn record_with_fixups(sector_size: usize, seq: u16, originals: [u16; 2]) -> Vec<u8> {
        let mut r = vec![0u8; sector_size * 2];
        r[0..4].copy_from_slice(RECORD_SIGNATURE);
        let usa_off = 48usize;
        r[4..6].copy_from_slice(&(usa_off as u16).to_le_bytes());
        r[6..8].copy_from_slice(&3u16.to_le_bytes()); // seq + 2 sectors
        r[usa_off..usa_off + 2].copy_from_slice(&seq.to_le_bytes());
        r[usa_off + 2..usa_off + 4].copy_from_slice(&originals[0].to_le_bytes());
        r[usa_off + 4..usa_off + 6].copy_from_slice(&originals[1].to_le_bytes());
        for i in 0..2 {
            let tail = (i + 1) * sector_size - 2;
            r[tail..tail + 2].copy_from_slice(&seq.to_le_bytes());
        }
        r
    }

    #[test]
    fn fixups_restore_the_bytes_the_sequence_number_replaced() {
        let mut r = record_with_fixups(512, 0xAABB, [0x1234, 0x5678]);
        assert!(apply_fixups(&mut r, 512));
        assert_eq!(u16_at(&r, 510), Some(0x1234));
        assert_eq!(u16_at(&r, 1022), Some(0x5678));
    }

    #[test]
    fn a_torn_write_is_rejected() {
        let mut r = record_with_fixups(512, 0xAABB, [0x1234, 0x5678]);
        // Second sector never reached the disk: its tail holds an older sequence.
        r[1022..1024].copy_from_slice(&0x0001u16.to_le_bytes());
        assert!(!apply_fixups(&mut r, 512), "must not trust a torn record");
    }

    #[test]
    fn a_fixup_array_pointing_outside_the_record_is_rejected() {
        let mut r = record_with_fixups(512, 0xAABB, [0x1234, 0x5678]);
        r[4..6].copy_from_slice(&60000u16.to_le_bytes());
        assert!(!apply_fixups(&mut r, 512));
    }

    #[test]
    fn a_zero_usa_count_is_rejected() {
        let mut r = record_with_fixups(512, 0xAABB, [0x1234, 0x5678]);
        r[6..8].copy_from_slice(&0u16.to_le_bytes());
        assert!(!apply_fixups(&mut r, 512));
    }

    // --- record header -------------------------------------------------------

    fn record_header_bytes(flags: u16, first_attr: u16, used: u32, links: u16) -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(RECORD_SIGNATURE);
        r[18..20].copy_from_slice(&links.to_le_bytes());
        r[20..22].copy_from_slice(&first_attr.to_le_bytes());
        r[22..24].copy_from_slice(&flags.to_le_bytes());
        r[24..28].copy_from_slice(&used.to_le_bytes());
        r
    }

    #[test]
    fn parses_a_file_record_header() {
        let r = record_header_bytes(FLAG_IN_USE, 56, 400, 1);
        let h = parse_record_header(&r).expect("should parse");
        assert!(h.in_use);
        assert!(!h.is_directory);
        assert_eq!(h.first_attr_offset, 56);
        assert_eq!(h.used_size, 400);
        assert_eq!(h.hard_link_count, 1);
    }

    #[test]
    fn recognises_a_directory_record() {
        let r = record_header_bytes(FLAG_IN_USE | FLAG_DIRECTORY, 56, 400, 1);
        assert!(parse_record_header(&r).expect("should parse").is_directory);
    }

    #[test]
    fn recognises_a_deleted_record() {
        let r = record_header_bytes(0, 56, 400, 1);
        assert!(!parse_record_header(&r).expect("should parse").in_use);
    }

    #[test]
    fn rejects_a_record_without_the_signature() {
        let mut r = record_header_bytes(FLAG_IN_USE, 56, 400, 1);
        r[0..4].copy_from_slice(b"BAAD");
        assert!(parse_record_header(&r).is_none());
    }

    #[test]
    fn rejects_a_first_attribute_offset_outside_the_record() {
        let r = record_header_bytes(FLAG_IN_USE, 60000, 400, 1);
        assert!(parse_record_header(&r).is_none());
    }

    #[test]
    fn rejects_a_used_size_larger_than_the_record() {
        let r = record_header_bytes(FLAG_IN_USE, 56, 99999, 1);
        assert!(parse_record_header(&r).is_none());
    }

    // --- attributes ----------------------------------------------------------

    /// Append a resident attribute; returns the next write position.
    fn push_resident(r: &mut [u8], pos: usize, type_code: u32, value: &[u8]) -> usize {
        let value_off = 24usize;
        let total = (value_off + value.len()).next_multiple_of(8);
        r[pos..pos + 4].copy_from_slice(&type_code.to_le_bytes());
        r[pos + 4..pos + 8].copy_from_slice(&(total as u32).to_le_bytes());
        r[pos + 8] = 0; // resident
        r[pos + 16..pos + 20].copy_from_slice(&(value.len() as u32).to_le_bytes());
        r[pos + 20..pos + 22].copy_from_slice(&(value_off as u16).to_le_bytes());
        r[pos + value_off..pos + value_off + value.len()].copy_from_slice(value);
        pos + total
    }

    fn end_marker(r: &mut [u8], pos: usize) {
        r[pos..pos + 4].copy_from_slice(&attr_type::END.to_le_bytes());
    }

    #[test]
    fn walks_the_attribute_chain() {
        let mut r = record_header_bytes(FLAG_IN_USE, 56, 1024, 1);
        let mut p = 56;
        p = push_resident(&mut r, p, attr_type::FILE_NAME, &[0u8; 80]);
        p = push_resident(&mut r, p, attr_type::DATA, &[1u8; 16]);
        end_marker(&mut r, p);
        let h = parse_record_header(&r).expect("header");
        let kinds: Vec<u32> = Attributes::new(&r, &h).map(|a| a.type_code).collect();
        assert_eq!(kinds, vec![attr_type::FILE_NAME, attr_type::DATA]);
    }

    #[test]
    fn a_zero_length_attribute_does_not_loop_forever() {
        let mut r = record_header_bytes(FLAG_IN_USE, 56, 1024, 1);
        r[56..60].copy_from_slice(&attr_type::DATA.to_le_bytes());
        r[60..64].copy_from_slice(&0u32.to_le_bytes());
        let h = parse_record_header(&r).expect("header");
        assert_eq!(Attributes::new(&r, &h).count(), 0, "must stop, not spin");
    }

    #[test]
    fn an_attribute_running_past_the_record_is_dropped() {
        let mut r = record_header_bytes(FLAG_IN_USE, 56, 1024, 1);
        r[56..60].copy_from_slice(&attr_type::DATA.to_le_bytes());
        r[60..64].copy_from_slice(&99999u32.to_le_bytes());
        let h = parse_record_header(&r).expect("header");
        assert_eq!(Attributes::new(&r, &h).count(), 0);
    }

    #[test]
    fn attributes_stop_at_the_used_size_not_the_record_size() {
        // The first attribute occupies 56..160 (24-byte header + 80-byte value,
        // padded to 8). used_size is set to end exactly there, so the second
        // attribute lies beyond it.
        let mut r = record_header_bytes(FLAG_IN_USE, 56, 160, 1);
        let mut p = 56;
        p = push_resident(&mut r, p, attr_type::FILE_NAME, &[0u8; 80]);
        assert_eq!(p, 160, "fixture assumption: first attribute ends at 160");
        // Starts past used_size: stale bytes from a previous, larger record.
        push_resident(&mut r, p, attr_type::DATA, &[1u8; 16]);
        let h = parse_record_header(&r).expect("header");
        let kinds: Vec<u32> = Attributes::new(&r, &h).map(|a| a.type_code).collect();
        assert_eq!(kinds, vec![attr_type::FILE_NAME]);
    }

    #[test]
    fn an_attribute_list_is_reported_so_the_caller_can_follow_it() {
        // A file with many fragments keeps some attributes in extension
        // records. Silently ignoring $ATTRIBUTE_LIST would undercount it.
        let mut r = record_header_bytes(FLAG_IN_USE, 56, 1024, 1);
        let mut p = 56;
        p = push_resident(&mut r, p, attr_type::ATTRIBUTE_LIST, &[0u8; 32]);
        end_marker(&mut r, p);
        let h = parse_record_header(&r).expect("header");
        let kinds: Vec<u32> = Attributes::new(&r, &h).map(|a| a.type_code).collect();
        assert_eq!(kinds, vec![attr_type::ATTRIBUTE_LIST]);
    }

    // --- $FILE_NAME ----------------------------------------------------------

    fn file_name_value(parent: u64, alloc: u64, real: u64, ns: u8, name: &str) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut v = vec![0u8; 66 + units.len() * 2];
        v[0..8].copy_from_slice(&parent.to_le_bytes());
        v[40..48].copy_from_slice(&alloc.to_le_bytes());
        v[48..56].copy_from_slice(&real.to_le_bytes());
        v[64] = units.len() as u8;
        v[65] = ns;
        for (i, u) in units.iter().enumerate() {
            v[66 + i * 2..68 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        v
    }

    #[test]
    fn parses_a_file_name_attribute() {
        let v = file_name_value(5, 4096, 1234, namespace::WIN32, "example.txt");
        let f = parse_file_name(&v).expect("should parse");
        assert_eq!(f.parent, 5);
        assert_eq!(f.allocated_size, 4096);
        assert_eq!(f.real_size, 1234);
        assert_eq!(f.name, "example.txt");
        assert!(!f.is_dos_alias());
    }

    #[test]
    fn the_parent_reference_drops_the_sequence_number() {
        // The high 16 bits are a reuse counter, not part of the record number.
        let v = file_name_value(0x0007_0000_0000_0005, 0, 0, namespace::WIN32, "a");
        assert_eq!(parse_file_name(&v).expect("parse").parent, 5);
    }

    #[test]
    fn a_dos_alias_is_recognised() {
        let v = file_name_value(5, 0, 0, namespace::DOS, "EXAMPL~1.TXT");
        assert!(parse_file_name(&v).expect("parse").is_dos_alias());
    }

    #[test]
    fn a_win32_and_dos_entry_is_not_an_alias() {
        // Namespace 3 means one name serves both, so it is a real link.
        let v = file_name_value(5, 0, 0, namespace::WIN32_AND_DOS, "readme");
        assert!(!parse_file_name(&v).expect("parse").is_dos_alias());
        let p = file_name_value(5, 0, 0, namespace::POSIX, "readme");
        assert!(!parse_file_name(&p).expect("parse").is_dos_alias());
    }

    #[test]
    fn a_name_running_past_the_attribute_is_rejected() {
        let mut v = file_name_value(5, 0, 0, namespace::WIN32, "abc");
        v[64] = 200; // claims 200 characters
        assert!(parse_file_name(&v).is_none());
    }

    #[test]
    fn a_truncated_file_name_attribute_is_rejected() {
        assert!(parse_file_name(&[0u8; 20]).is_none());
    }

    #[test]
    fn a_unicode_name_survives_the_round_trip() {
        let v = file_name_value(5, 0, 0, namespace::WIN32, "日本語のファイル.txt");
        assert_eq!(
            parse_file_name(&v).expect("parse").name,
            "日本語のファイル.txt"
        );
    }

    // --- $DATA ---------------------------------------------------------------

    #[test]
    fn a_resident_data_attribute_occupies_no_clusters() {
        let mut r = record_header_bytes(FLAG_IN_USE, 56, 1024, 1);
        let p = push_resident(&mut r, 56, attr_type::DATA, &[7u8; 100]);
        end_marker(&mut r, p);
        let h = parse_record_header(&r).expect("header");
        let attr = Attributes::new(&r, &h).next().expect("one attribute");
        let sizes = parse_data_sizes(&r, &attr).expect("sizes");
        assert_eq!(sizes.real_size, 100);
        assert_eq!(
            sizes.allocated_size, 100,
            "a small file lives in the record; charging it a cluster would \
             overcount every tiny file on the volume"
        );
    }

    #[test]
    fn a_non_resident_data_attribute_reports_allocated_and_real_sizes() {
        let mut r = record_header_bytes(FLAG_IN_USE, 56, 1024, 1);
        let pos = 56usize;
        r[pos..pos + 4].copy_from_slice(&attr_type::DATA.to_le_bytes());
        r[pos + 4..pos + 8].copy_from_slice(&72u32.to_le_bytes());
        r[pos + 8] = 1; // non-resident
        r[pos + 0x28..pos + 0x30].copy_from_slice(&8192u64.to_le_bytes());
        r[pos + 0x30..pos + 0x38].copy_from_slice(&5000u64.to_le_bytes());
        end_marker(&mut r, pos + 72);
        let h = parse_record_header(&r).expect("header");
        let attr = Attributes::new(&r, &h).next().expect("one attribute");
        let sizes = parse_data_sizes(&r, &attr).expect("sizes");
        assert_eq!(sizes.real_size, 5000);
        assert_eq!(
            sizes.allocated_size, 8192,
            "allocated is what the volume spends, and is where a sparse or \
             compressed file differs from its apparent size"
        );
    }

    #[test]
    fn a_sparse_file_allocates_less_than_it_claims() {
        let mut r = record_header_bytes(FLAG_IN_USE, 56, 1024, 1);
        let pos = 56usize;
        r[pos..pos + 4].copy_from_slice(&attr_type::DATA.to_le_bytes());
        r[pos + 4..pos + 8].copy_from_slice(&72u32.to_le_bytes());
        r[pos + 8] = 1;
        r[pos + 0x28..pos + 0x30].copy_from_slice(&4096u64.to_le_bytes());
        r[pos + 0x30..pos + 0x38].copy_from_slice(&(1u64 << 30).to_le_bytes());
        end_marker(&mut r, pos + 72);
        let h = parse_record_header(&r).expect("header");
        let attr = Attributes::new(&r, &h).next().expect("one attribute");
        let sizes = parse_data_sizes(&r, &attr).expect("sizes");
        assert!(
            sizes.allocated_size < sizes.real_size,
            "a 1 GiB sparse file holding one cluster must not be counted as 1 GiB"
        );
    }
}
