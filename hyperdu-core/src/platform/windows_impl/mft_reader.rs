//! Reading MFT records off a volume, on top of the parsing in [`super::mft`].
//!
//! The volume sits behind a trait rather than a file handle, for one reason: a
//! real `\\.\C:` needs administrator rights, so a test that opened one could
//! only run on an elevated machine and would be skipped everywhere else. With
//! the source abstracted, the tests below build a synthetic NTFS volume in
//! memory and exercise the whole path -- boot sector, `$MFT` run list, record
//! iteration, name and size extraction -- with no privileges at all.
//!
//! What still needs a real volume is the last mile: that Windows hands back the
//! bytes we expect. Everything above that is settled here.

// Nothing here is called from the scan yet -- `process_dir` still goes to `nt`
// or `win32`. The reader lands with its tests first, so the volume handle that
// follows is written against something already known to work. Remove this once
// the backend is wired up.
#![allow(dead_code)]

use std::collections::HashMap;

use super::mft::{
    allocated_clusters, apply_fixups, attr_type, build_path, distinct_links, namespace,
    parse_boot_sector, parse_data_sizes, parse_file_name, parse_record_header, parse_run_list,
    vcn_to_offset, Attributes, DataSizes, FileName, Geometry, Run,
};

/// Somewhere MFT bytes can be read from: a volume handle in production, a
/// `Vec<u8>` in tests.
pub(crate) trait VolumeSource {
    /// Fill `buf` from `offset`. Returns false when the range is not readable,
    /// which the caller treats as the end of usable data rather than retrying.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool;
}

/// A record's identity and sizes, as the scan needs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) record: u64,
    pub(crate) parent: u64,
    pub(crate) name: String,
    pub(crate) is_directory: bool,
    pub(crate) sizes: DataSizes,
    pub(crate) hard_link_count: u16,
}

/// Reads records from the `$MFT` of a volume.
pub(crate) struct MftReader<S: VolumeSource> {
    source: S,
    geometry: Geometry,
    /// Where the MFT's own data lives. The MFT is a file and can be fragmented;
    /// without these runs only its first extent is reachable, and the scan
    /// would stop early while still reporting a plausible total.
    runs: Vec<Run>,
}

impl<S: VolumeSource> MftReader<S> {
    /// Read the boot sector and the MFT's own run list.
    ///
    /// Returns `None` for anything that is not a readable NTFS volume; the
    /// caller falls back to directory enumeration rather than guessing.
    pub(crate) fn open(mut source: S) -> Option<Self> {
        let mut boot = [0u8; 512];
        if !source.read_at(0, &mut boot) {
            return None;
        }
        let geometry = parse_boot_sector(&boot)?;

        // Chicken-and-egg: the MFT describes itself in record 0, which sits at
        // the start of the MFT, which the boot sector locates.
        let mut rec = vec![0u8; geometry.record_size as usize];
        if !source.read_at(geometry.mft_offset, &mut rec) {
            return None;
        }
        if !apply_fixups(&mut rec, geometry.bytes_per_sector) {
            return None;
        }
        let header = parse_record_header(&rec)?;

        // The MFT's $DATA is non-resident by construction; its run list is what
        // makes the rest of the records reachable.
        let mut runs = None;
        for attr in Attributes::new(&rec, &header) {
            if attr.type_code == attr_type::DATA && attr.non_resident {
                let off = run_list_offset(&rec, attr.pos)?;
                runs = parse_run_list(rec.get(off..)?);
                break;
            }
        }
        let runs = runs?;
        if runs.is_empty() {
            return None;
        }

        Some(Self {
            source,
            geometry,
            runs,
        })
    }

    pub(crate) fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Total clusters the MFT occupies, which bounds how many records exist.
    pub(crate) fn mft_clusters(&self) -> u64 {
        allocated_clusters(&self.runs)
    }

    /// How many records the MFT can hold. Reading past this is not an error to
    /// report, just the end.
    pub(crate) fn record_count(&self) -> u64 {
        let bytes = self.mft_clusters() * self.geometry.cluster_size() as u64;
        bytes / self.geometry.record_size as u64
    }

    /// Read one record by number, with fixups already applied.
    ///
    /// Returns `None` for a record that is out of range, unreadable, or fails
    /// its torn-write check. A caller iterating records treats all three the
    /// same way: skip it.
    pub(crate) fn read_record(&mut self, number: u64) -> Option<Vec<u8>> {
        let record_size = self.geometry.record_size as u64;
        let cluster_size = self.geometry.cluster_size() as u64;

        // Records are packed into the MFT's clusters, so a record number maps
        // to a virtual cluster plus an offset inside it.
        let byte_offset = number.checked_mul(record_size)?;
        let vcn = byte_offset / cluster_size;
        let within = byte_offset % cluster_size;
        let base = vcn_to_offset(&self.runs, vcn, cluster_size)?;

        let mut buf = vec![0u8; record_size as usize];
        if !self.source.read_at(base + within, &mut buf) {
            return None;
        }
        if !apply_fixups(&mut buf, self.geometry.bytes_per_sector) {
            return None;
        }
        Some(buf)
    }

    /// Extract the entry for a record, or `None` when it holds nothing the scan
    /// cares about (deleted, unnamed, or unreadable).
    pub(crate) fn entry(&mut self, number: u64) -> Option<Entry> {
        let rec = self.read_record(number)?;
        let header = parse_record_header(&rec)?;
        if !header.in_use {
            return None;
        }

        let mut names: Vec<FileName> = Vec::new();
        let mut sizes: Option<DataSizes> = None;
        for attr in Attributes::new(&rec, &header) {
            match attr.type_code {
                attr_type::FILE_NAME if !attr.non_resident => {
                    let value =
                        rec.get(attr.value_offset..attr.value_offset + attr.value_length)?;
                    if let Some(f) = parse_file_name(value) {
                        names.push(f);
                    }
                }
                // The first $DATA is the file's contents. A later one is a
                // named alternate stream, which `du` does not count.
                attr_type::DATA if sizes.is_none() => {
                    sizes = parse_data_sizes(&rec, &attr);
                }
                _ => {}
            }
        }

        let links = distinct_links(&names);
        // Prefer a Win32 name for display; a POSIX-only record still has one.
        let chosen = links
            .iter()
            .find(|f| f.namespace == namespace::WIN32 || f.namespace == namespace::WIN32_AND_DOS)
            .or_else(|| links.first())?;

        Some(Entry {
            record: number,
            parent: chosen.parent,
            name: chosen.name.clone(),
            is_directory: header.is_directory,
            // A record whose $DATA lives in an extension record reports no size
            // here; $FILE_NAME's copy is the fallback, stale though it can be.
            sizes: sizes.unwrap_or(DataSizes {
                real_size: chosen.real_size,
                allocated_size: chosen.allocated_size,
            }),
            hard_link_count: header.hard_link_count,
        })
    }

    /// Walk every record, yielding the ones that hold a file or directory.
    ///
    /// Records 0..=15 are NTFS's own metadata files (`$MFT`, `$LogFile`,
    /// `$Bitmap` and friends). They occupy real space, but a user asking where
    /// their disk went is not asking about those, and `du` on a mounted volume
    /// cannot see them either -- so they are skipped to keep the two backends
    /// comparable.
    pub(crate) fn entries(&mut self) -> Vec<Entry> {
        const FIRST_USER_RECORD: u64 = 16;
        let count = self.record_count();
        let mut out = Vec::new();
        for n in FIRST_USER_RECORD..count {
            if let Some(e) = self.entry(n) {
                out.push(e);
            }
        }
        out
    }
}

/// Byte offset of a non-resident attribute's run list within the record.
///
/// The offset lives at 0x20 of the attribute header and is relative to the
/// attribute, not the record.
fn run_list_offset(rec: &[u8], attr_pos: usize) -> Option<usize> {
    let rel = u16::from_le_bytes(rec.get(attr_pos + 0x20..attr_pos + 0x22)?.try_into().ok()?);
    let off = attr_pos.checked_add(rel as usize)?;
    if off >= rec.len() {
        return None;
    }
    Some(off)
}

/// Build `record -> path` for a set of entries.
///
/// Entries whose parent chain does not reach the root are dropped rather than
/// attached somewhere plausible: putting an orphan under a guessed parent moves
/// its bytes into a directory that does not contain it.
pub(crate) fn paths_for(entries: &[Entry]) -> HashMap<u64, String> {
    let by_record: HashMap<u64, (String, u64)> = entries
        .iter()
        .map(|e| (e.record, (e.name.clone(), e.parent)))
        .collect();

    entries
        .iter()
        .filter_map(|e| {
            let path = build_path(e.record, |n| by_record.get(&n).cloned())?;
            Some((e.record, path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{super::mft::ROOT_RECORD, *};

    // --- a synthetic NTFS volume ---------------------------------------------

    const SECTOR: usize = 512;
    const CLUSTER: usize = 4096;
    const RECORD: usize = 1024;
    /// Cluster where the MFT starts in the fixtures below.
    const MFT_LCN: u64 = 4;

    struct MemVolume(Vec<u8>);

    impl VolumeSource for MemVolume {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
            let start = offset as usize;
            let end = start + buf.len();
            match self.0.get(start..end) {
                Some(s) => {
                    buf.copy_from_slice(s);
                    true
                }
                None => false,
            }
        }
    }

    fn boot_sector() -> Vec<u8> {
        let mut b = vec![0u8; SECTOR];
        b[3..11].copy_from_slice(b"NTFS    ");
        b[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
        b[13] = (CLUSTER / SECTOR) as u8;
        b[48..56].copy_from_slice(&MFT_LCN.to_le_bytes());
        b[64] = -10i8 as u8; // 2^10 = 1024-byte records
        b
    }

    /// A record with a valid signature and a fixup array that checks out.
    fn blank_record(flags: u16, links: u16) -> Vec<u8> {
        let mut r = vec![0u8; RECORD];
        r[0..4].copy_from_slice(b"FILE");
        let usa_off = 48usize;
        let sectors = RECORD / SECTOR;
        r[4..6].copy_from_slice(&(usa_off as u16).to_le_bytes());
        r[6..8].copy_from_slice(&((sectors + 1) as u16).to_le_bytes());
        let seq = 0x0101u16;
        r[usa_off..usa_off + 2].copy_from_slice(&seq.to_le_bytes());
        for i in 0..sectors {
            // The real bytes are zero here, so the array holds zeroes and each
            // sector tail holds the sequence number.
            r[usa_off + 2 + i * 2..usa_off + 4 + i * 2].copy_from_slice(&0u16.to_le_bytes());
            let tail = (i + 1) * SECTOR - 2;
            r[tail..tail + 2].copy_from_slice(&seq.to_le_bytes());
        }
        r[18..20].copy_from_slice(&links.to_le_bytes());
        r[20..22].copy_from_slice(&64u16.to_le_bytes()); // first attribute
        r[22..24].copy_from_slice(&flags.to_le_bytes());
        r
    }

    fn set_used(rec: &mut [u8], used: u32) {
        rec[24..28].copy_from_slice(&used.to_le_bytes());
    }

    fn push_file_name(rec: &mut [u8], pos: usize, parent: u64, name: &str) -> usize {
        let units: Vec<u16> = name.encode_utf16().collect();
        let value_len = 66 + units.len() * 2;
        let value_off = 24usize;
        let total = (value_off + value_len).next_multiple_of(8);

        rec[pos..pos + 4].copy_from_slice(&attr_type::FILE_NAME.to_le_bytes());
        rec[pos + 4..pos + 8].copy_from_slice(&(total as u32).to_le_bytes());
        rec[pos + 8] = 0; // resident
        rec[pos + 16..pos + 20].copy_from_slice(&(value_len as u32).to_le_bytes());
        rec[pos + 20..pos + 22].copy_from_slice(&(value_off as u16).to_le_bytes());

        let v = pos + value_off;
        rec[v..v + 8].copy_from_slice(&parent.to_le_bytes());
        rec[v + 64] = units.len() as u8;
        rec[v + 65] = namespace::WIN32;
        for (i, u) in units.iter().enumerate() {
            rec[v + 66 + i * 2..v + 68 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        pos + total
    }

    fn push_nonresident_data(rec: &mut [u8], pos: usize, alloc: u64, real: u64) -> usize {
        let total = 72usize;
        rec[pos..pos + 4].copy_from_slice(&attr_type::DATA.to_le_bytes());
        rec[pos + 4..pos + 8].copy_from_slice(&(total as u32).to_le_bytes());
        rec[pos + 8] = 1; // non-resident
        rec[pos + 0x28..pos + 0x30].copy_from_slice(&alloc.to_le_bytes());
        rec[pos + 0x30..pos + 0x38].copy_from_slice(&real.to_le_bytes());
        pos + total
    }

    /// `$MFT`'s own record: a non-resident `$DATA` whose run list covers
    /// `clusters` clusters starting at `MFT_LCN`.
    fn mft_record(clusters: u64) -> Vec<u8> {
        let mut r = blank_record(0x0001, 1);
        let pos = 64usize;
        let total = 72usize;
        r[pos..pos + 4].copy_from_slice(&attr_type::DATA.to_le_bytes());
        r[pos + 4..pos + 8].copy_from_slice(&(total as u32).to_le_bytes());
        r[pos + 8] = 1; // non-resident
        let run_off = 0x40u16; // within the attribute
        r[pos + 0x20..pos + 0x22].copy_from_slice(&run_off.to_le_bytes());
        // 0x11: one length byte, one offset byte.
        let ro = pos + run_off as usize;
        r[ro] = 0x11;
        r[ro + 1] = clusters as u8;
        r[ro + 2] = MFT_LCN as u8;
        r[ro + 3] = 0x00; // end of list
        set_used(&mut r, (pos + total) as u32);
        r
    }

    /// Volume holding `records` starting at record 0, laid out as the MFT.
    fn volume(records: Vec<Vec<u8>>) -> MemVolume {
        let mft_byte_offset = MFT_LCN as usize * CLUSTER;
        let mut v = boot_sector();
        v.resize(mft_byte_offset, 0);
        for r in &records {
            v.extend_from_slice(r);
        }
        // Round out to whole clusters so the run list's length is honest.
        let records_per_cluster = CLUSTER / RECORD;
        let clusters = records.len().div_ceil(records_per_cluster).max(1);
        v.resize(mft_byte_offset + clusters * CLUSTER, 0);
        MemVolume(v)
    }

    /// Records 0..16 are NTFS metadata; the fixtures fill them with blanks so
    /// user records land at realistic numbers.
    fn with_metadata_records(mut user: Vec<Vec<u8>>, mft_clusters: u64) -> Vec<Vec<u8>> {
        let mut records = vec![mft_record(mft_clusters)];
        while records.len() < 16 {
            records.push(blank_record(0x0000, 0)); // not in use
        }
        records.append(&mut user);
        records
    }

    fn file_record(parent: u64, name: &str, alloc: u64, real: u64, links: u16) -> Vec<u8> {
        let mut r = blank_record(0x0001, links);
        let mut p = 64usize;
        p = push_file_name(&mut r, p, parent, name);
        p = push_nonresident_data(&mut r, p, alloc, real);
        set_used(&mut r, p as u32);
        r
    }

    fn dir_record(parent: u64, name: &str) -> Vec<u8> {
        let mut r = blank_record(0x0003, 1); // in use + directory
        let mut p = 64usize;
        p = push_file_name(&mut r, p, parent, name);
        set_used(&mut r, p as u32);
        r
    }

    // --- tests ---------------------------------------------------------------

    #[test]
    fn opens_a_volume_and_finds_the_mft_run_list() {
        let vol = volume(with_metadata_records(vec![], 8));
        let reader = MftReader::open(vol).expect("should open");
        assert_eq!(reader.geometry().record_size, RECORD as u32);
        assert_eq!(reader.geometry().cluster_size(), CLUSTER as u32);
        assert_eq!(reader.mft_clusters(), 8, "from $MFT's own run list");
        assert_eq!(reader.record_count(), 8 * CLUSTER as u64 / RECORD as u64);
    }

    #[test]
    fn refuses_a_volume_that_is_not_ntfs() {
        let mut v = volume(with_metadata_records(vec![], 8));
        v.0[3..11].copy_from_slice(b"FAT32   ");
        assert!(MftReader::open(v).is_none());
    }

    #[test]
    fn refuses_a_volume_too_short_to_hold_a_boot_sector() {
        assert!(MftReader::open(MemVolume(vec![0u8; 16])).is_none());
    }

    #[test]
    fn refuses_a_volume_whose_mft_record_is_unreadable() {
        // The boot sector points past the end of the volume.
        let mut v = volume(with_metadata_records(vec![], 8));
        v.0.truncate(SECTOR);
        assert!(MftReader::open(v).is_none());
    }

    #[test]
    fn reads_a_file_record_end_to_end() {
        let records = with_metadata_records(
            vec![file_record(ROOT_RECORD, "notes.txt", 8192, 5000, 1)],
            8,
        );
        let mut reader = MftReader::open(volume(records)).expect("open");
        let e = reader.entry(16).expect("record 16 is the file");
        assert_eq!(e.name, "notes.txt");
        assert_eq!(e.parent, ROOT_RECORD);
        assert!(!e.is_directory);
        assert_eq!(e.sizes.real_size, 5000);
        assert_eq!(e.sizes.allocated_size, 8192);
    }

    #[test]
    fn a_directory_record_is_marked_as_one() {
        let records = with_metadata_records(vec![dir_record(ROOT_RECORD, "Users")], 8);
        let mut reader = MftReader::open(volume(records)).expect("open");
        assert!(reader.entry(16).expect("entry").is_directory);
    }

    #[test]
    fn a_deleted_record_yields_nothing() {
        let mut rec = file_record(ROOT_RECORD, "gone.txt", 4096, 100, 1);
        rec[22..24].copy_from_slice(&0u16.to_le_bytes()); // not in use
        let records = with_metadata_records(vec![rec], 8);
        let mut reader = MftReader::open(volume(records)).expect("open");
        assert!(reader.entry(16).is_none());
    }

    #[test]
    fn iterating_skips_the_ntfs_metadata_records() {
        let records = with_metadata_records(
            vec![
                dir_record(ROOT_RECORD, "Users"),
                file_record(16, "a.txt", 4096, 10, 1),
            ],
            8,
        );
        let mut reader = MftReader::open(volume(records)).expect("open");
        let entries = reader.entries();
        assert_eq!(
            entries.len(),
            2,
            "$MFT and friends must not appear as user files"
        );
        assert_eq!(entries[0].name, "Users");
        assert_eq!(entries[1].name, "a.txt");
    }

    #[test]
    fn a_record_past_the_end_of_the_mft_is_not_readable() {
        let records = with_metadata_records(vec![], 8);
        let mut reader = MftReader::open(volume(records)).expect("open");
        let past = reader.record_count() + 10;
        assert!(reader.read_record(past).is_none());
    }

    #[test]
    fn builds_paths_from_parent_references() {
        // 16 = Users (under root), 17 = alice (under Users), 18 = notes.txt
        let records = with_metadata_records(
            vec![
                dir_record(ROOT_RECORD, "Users"),
                dir_record(16, "alice"),
                file_record(17, "notes.txt", 4096, 10, 1),
            ],
            8,
        );
        let mut reader = MftReader::open(volume(records)).expect("open");
        let entries = reader.entries();
        let paths = paths_for(&entries);
        assert_eq!(
            paths.get(&18).map(String::as_str),
            Some("Users\\alice\\notes.txt")
        );
        assert_eq!(paths.get(&16).map(String::as_str), Some("Users"));
    }

    #[test]
    fn an_orphaned_record_gets_no_path() {
        // Parent 999 does not exist, so the chain never reaches the root.
        let records = with_metadata_records(vec![file_record(999, "orphan.txt", 4096, 10, 1)], 8);
        let mut reader = MftReader::open(volume(records)).expect("open");
        let paths = paths_for(&reader.entries());
        assert!(
            paths.is_empty(),
            "attaching it to a guessed parent would move its bytes"
        );
    }

    #[test]
    fn a_torn_record_is_skipped_rather_than_misread() {
        let mut rec = file_record(ROOT_RECORD, "torn.txt", 4096, 10, 1);
        // Break the second sector's fixup so the torn-write check fails.
        rec[2 * SECTOR - 2..2 * SECTOR].copy_from_slice(&0xDEADu16.to_le_bytes());
        let records = with_metadata_records(vec![rec], 8);
        let mut reader = MftReader::open(volume(records)).expect("open");
        assert!(reader.entry(16).is_none());
    }

    #[test]
    fn the_hard_link_count_is_carried_through() {
        let records =
            with_metadata_records(vec![file_record(ROOT_RECORD, "linked.txt", 4096, 10, 3)], 8);
        let mut reader = MftReader::open(volume(records)).expect("open");
        assert_eq!(
            reader.entry(16).expect("entry").hard_link_count,
            3,
            "dedupe needs this, and it is free here"
        );
    }

    #[test]
    fn a_fragmented_mft_reaches_records_in_its_second_extent() {
        // Two runs: cluster 4 (1 cluster) then cluster 9 (1 cluster). Records
        // 0..3 live in the first, 4..7 in the second.
        let mut mft = blank_record(0x0001, 1);
        let pos = 64usize;
        mft[pos..pos + 4].copy_from_slice(&attr_type::DATA.to_le_bytes());
        mft[pos + 4..pos + 8].copy_from_slice(&72u32.to_le_bytes());
        mft[pos + 8] = 1;
        mft[pos + 0x20..pos + 0x22].copy_from_slice(&0x40u16.to_le_bytes());
        let ro = pos + 0x40;
        mft[ro] = 0x11; // run 1: len 1, lcn +4
        mft[ro + 1] = 1;
        mft[ro + 2] = 4;
        mft[ro + 3] = 0x11; // run 2: len 1, lcn +5 (absolute 9)
        mft[ro + 4] = 1;
        mft[ro + 5] = 5;
        mft[ro + 6] = 0x00;
        set_used(&mut mft, (pos + 72) as u32);

        // Lay the volume out by hand: first extent at cluster 4, second at 9.
        let mut v = boot_sector();
        v.resize(4 * CLUSTER, 0);
        v.extend_from_slice(&mft); // record 0
        for _ in 1..4 {
            v.extend_from_slice(&blank_record(0x0000, 0));
        }
        v.resize(9 * CLUSTER, 0);
        // Records 4..8 land in the second extent; put a file at record 4.
        v.extend_from_slice(&file_record(ROOT_RECORD, "far.txt", 8192, 1234, 1));
        for _ in 5..8 {
            v.extend_from_slice(&blank_record(0x0000, 0));
        }

        let mut reader = MftReader::open(MemVolume(v)).expect("open");
        assert_eq!(reader.record_count(), 8, "two clusters of records");
        let e = reader.entry(4).expect("record 4 is in the second extent");
        assert_eq!(
            e.name, "far.txt",
            "without the run list this record is unreachable, and the scan \
             would stop early while still looking complete"
        );
        assert_eq!(e.sizes.real_size, 1234);
    }
}
