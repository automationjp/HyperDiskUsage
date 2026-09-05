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

/// Lets a caller keep ownership of the volume while the reader borrows it, so
/// the sector size can be narrowed after the geometry is known.
impl<S: VolumeSource + ?Sized> VolumeSource for &mut S {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        (**self).read_at(offset, buf)
    }
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
        let mut runs = data_runs_in(&rec, &header).unwrap_or_default();
        if runs.is_empty() {
            return None;
        }

        // On a volume with millions of files the MFT is itself fragmented
        // enough that its $DATA does not fit in one record, and the rest lives
        // in extension records named by $ATTRIBUTE_LIST. Stopping at the base
        // record was measured missing 88.9% of a real volume's files -- while
        // still reporting a plausible total, which is the worst way to be
        // wrong. See #15.
        let extensions = extension_records_for(&rec, &header, attr_type::DATA);
        if !extensions.is_empty() {
            let mut reader = Self {
                source,
                geometry,
                runs,
            };
            reader.extend_runs_from(&extensions);
            return Some(reader);
        }

        // Reborrow: `source` was only moved in the branch above.
        runs.shrink_to_fit();
        Some(Self {
            source,
            geometry,
            runs,
        })
    }

    /// Follow `$ATTRIBUTE_LIST` entries and append the `$DATA` runs they name.
    ///
    /// The extension records are themselves in the MFT, so this can only reach
    /// the ones covered by the runs found so far. That is enough in practice:
    /// NTFS keeps the first extent large, and each round of appending brings
    /// more of the MFT into reach. The loop stops when a pass adds nothing,
    /// rather than assuming one pass suffices.
    fn extend_runs_from(&mut self, extensions: &[u64]) {
        // A pathological volume could otherwise bounce between records; the
        // records come from a filesystem that may be damaged.
        const MAX_PASSES: usize = 8;
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();

        for _ in 0..MAX_PASSES {
            let before = self.runs.len();
            for &number in extensions {
                if !seen.insert(number) {
                    continue;
                }
                let Some(rec) = self.read_record(number) else {
                    // Not reachable yet with the runs we have; a later pass may
                    // reach it once the run list grows.
                    seen.remove(&number);
                    continue;
                };
                let Some(header) = parse_record_header(&rec) else {
                    continue;
                };
                if let Some(more) = data_runs_in(&rec, &header) {
                    self.runs.extend(more);
                }
            }
            if self.runs.len() == before {
                return;
            }
        }
    }

    pub(crate) fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// The volume, so the caller can narrow the read alignment once the
    /// geometry is known. Doing that after the records are read achieves
    /// nothing, which is what used to happen.
    pub(crate) fn source_mut(&mut self) -> &mut S {
        &mut self.source
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

// --- turning records into a StatMap ------------------------------------------

/// Fold MFT entries into the per-directory map the rest of the scan expects.
///
/// `root_prefix` is prepended to every path (`C:\` for a whole-volume scan), so
/// the result is addressed the same way the enumeration backend addresses it.
/// Without that the two backends' maps cannot be compared, and comparing them
/// is how this backend gets shown to be correct.
///
/// Hardlink handling matches GNU `du`: a file with several links is charged
/// once, to whichever link is met first. `count_hardlinks` turns that off, as
/// it does elsewhere.
pub(crate) fn to_stat_map(
    entries: &[Entry],
    paths: &HashMap<u64, String>,
    root_prefix: &str,
    count_hardlinks: bool,
    compute_physical: bool,
) -> crate::StatMap {
    let mut map: crate::StatMap = crate::StatMap::default();
    let mut counted_links: std::collections::HashSet<u64> = std::collections::HashSet::new();

    // Directories come first so an empty one still appears in the map. The
    // enumeration backend lists it, and a directory that exists in one map but
    // not the other shows up as a spurious difference.
    for e in entries.iter().filter(|e| e.is_directory) {
        if let Some(p) = paths.get(&e.record) {
            map.entry(join_path(root_prefix, p)).or_default();
        }
    }

    for e in entries.iter().filter(|e| !e.is_directory) {
        // A file's bytes belong to the directory holding it, not to itself.
        let Some(parent_path) = paths.get(&e.parent) else {
            // Parent outside the scanned set: dropping the file is the same
            // choice `paths_for` makes for orphans, and for the same reason.
            continue;
        };
        if !count_hardlinks && e.hard_link_count > 1 && !counted_links.insert(e.record) {
            continue;
        }

        let stat = map.entry(join_path(root_prefix, parent_path)).or_default();
        stat.files += 1;
        stat.logical += e.sizes.real_size;
        stat.physical += if compute_physical {
            e.sizes.allocated_size
        } else {
            e.sizes.real_size
        };
    }

    map
}

fn join_path(prefix: &str, rest: &str) -> std::path::PathBuf {
    if rest.is_empty() {
        return std::path::PathBuf::from(prefix);
    }
    let mut s = String::with_capacity(prefix.len() + 1 + rest.len());
    s.push_str(prefix);
    if !prefix.ends_with('\\') && !prefix.ends_with('/') {
        s.push('\\');
    }
    s.push_str(rest);
    std::path::PathBuf::from(s)
}

// --- the real volume ---------------------------------------------------------

/// A volume opened for reading, as `\\.\C:`.
///
/// Opening one needs administrator rights, so [`WindowsVolume::open`] returns
/// `None` for an unelevated process and the caller falls back to directory
/// enumeration. That failure is expected, not exceptional: most runs will not
/// be elevated.
#[cfg(windows)]
pub(crate) struct WindowsVolume {
    handle: windows::Win32::Foundation::HANDLE,
    /// Reads on a volume handle must be aligned to the sector size and a whole
    /// number of sectors long. MFT records are not, so reads are widened to
    /// the enclosing sectors and the wanted bytes copied out.
    sector: u64,
}

#[cfg(windows)]
impl WindowsVolume {
    /// Open `drive` (a single letter, as in `C`) for reading.
    ///
    /// Returns `None` when the process is not elevated, the drive does not
    /// exist, or the handle cannot be opened for any other reason. All three
    /// mean the same thing to the caller: use the enumeration backend.
    pub(crate) fn open(drive: char) -> Option<Self> {
        use windows::{
            core::PCWSTR,
            Win32::{
                Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE},
                Storage::FileSystem::{
                    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
                    OPEN_EXISTING,
                },
            },
        };

        if !drive.is_ascii_alphabetic() {
            return None;
        }
        // \\.\C: -- the volume itself, not a file on it.
        let path: Vec<u16> = format!(r"\\.\{}:", drive.to_ascii_uppercase())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_READ.0,
                // The volume is mounted and in use; without sharing, the open
                // fails on every system volume.
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .ok()?;

        if handle == INVALID_HANDLE_VALUE {
            return None;
        }

        // 512 covers every NTFS volume in practice; a 4K-native disk reports
        // 4096 and the boot sector will say so. Reads are widened to whichever
        // is larger, so starting conservative is safe.
        Some(Self {
            handle,
            sector: 4096,
        })
    }

    /// Narrow the alignment once the boot sector has been parsed, so reads stop
    /// fetching more than they need.
    pub(crate) fn set_sector_size(&mut self, bytes: u32) {
        if bytes >= 512 && bytes.is_power_of_two() {
            self.sector = bytes as u64;
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsVolume {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
impl VolumeSource for WindowsVolume {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        use windows::Win32::{
            Storage::FileSystem::{ReadFile, SetFilePointerEx, FILE_BEGIN},
            System::IO::OVERLAPPED,
        };

        // Widen to sector boundaries: a volume handle rejects anything else.
        let start = offset - (offset % self.sector);
        let end = (offset + buf.len() as u64).next_multiple_of(self.sector);
        let span = (end - start) as usize;

        let mut scratch = vec![0u8; span];
        unsafe {
            if SetFilePointerEx(self.handle, start as i64, None, FILE_BEGIN).is_err() {
                return false;
            }
            let mut read = 0u32;
            if ReadFile(
                self.handle,
                Some(scratch.as_mut_slice()),
                Some(&mut read),
                None::<*mut OVERLAPPED>,
            )
            .is_err()
            {
                return false;
            }
            if (read as usize) < span {
                return false;
            }
        }

        let within = (offset - start) as usize;
        let Some(slice) = scratch.get(within..within + buf.len()) else {
            return false;
        };
        buf.copy_from_slice(slice);
        true
    }
}

/// Whether this process can open a volume handle at all.
///
/// Checked before attempting, so the caller can say "needs administrator
/// rights" rather than reporting an access-denied error from deep inside the
/// scan.
#[cfg(windows)]
pub(crate) fn is_elevated() -> bool {
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE},
        Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// Run list of the first non-resident `$DATA` in a record, if it has one.
fn data_runs_in(rec: &[u8], header: &super::mft::RecordHeader) -> Option<Vec<Run>> {
    for attr in Attributes::new(rec, header) {
        if attr.type_code == attr_type::DATA && attr.non_resident {
            let off = run_list_offset(rec, attr.pos)?;
            return parse_run_list(rec.get(off..)?);
        }
    }
    None
}

/// Records that hold `type_code` attributes for this file, from its
/// `$ATTRIBUTE_LIST`.
///
/// Entries naming the base record itself are dropped: re-reading it would add
/// its runs a second time, doubling the MFT's apparent size.
fn extension_records_for(
    rec: &[u8],
    header: &super::mft::RecordHeader,
    type_code: u32,
) -> Vec<u64> {
    let mut out = Vec::new();
    for attr in Attributes::new(rec, header) {
        if attr.type_code != attr_type::ATTRIBUTE_LIST || attr.non_resident {
            continue;
        }
        let Some(value) = rec.get(attr.value_offset..attr.value_offset + attr.value_length) else {
            continue;
        };
        for entry in super::mft::parse_attribute_list(value) {
            // Record 0 is the base for $MFT; anything else is an extension.
            if entry.type_code == type_code && entry.record != 0 {
                out.push(entry.record);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
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

    // --- StatMap conversion --------------------------------------------------

    fn entry(record: u64, parent: u64, name: &str, is_dir: bool, real: u64, alloc: u64) -> Entry {
        Entry {
            record,
            parent,
            name: name.to_string(),
            is_directory: is_dir,
            sizes: DataSizes {
                real_size: real,
                allocated_size: alloc,
            },
            hard_link_count: 1,
        }
    }

    fn paths(pairs: &[(u64, &str)]) -> HashMap<u64, String> {
        pairs.iter().map(|(r, p)| (*r, p.to_string())).collect()
    }

    #[test]
    fn a_files_bytes_land_in_its_parent_directory() {
        let entries = vec![
            entry(16, ROOT_RECORD, "Users", true, 0, 0),
            entry(17, 16, "a.txt", false, 1000, 4096),
        ];
        let p = paths(&[(16, "Users")]);
        let map = to_stat_map(&entries, &p, r"C:\", false, true);
        let users = map
            .get(std::path::Path::new(r"C:\Users"))
            .expect("Users is in the map");
        assert_eq!(users.files, 1);
        assert_eq!(users.logical, 1000);
        assert_eq!(users.physical, 4096, "allocated, not apparent");
    }

    #[test]
    fn an_empty_directory_still_appears() {
        let entries = vec![entry(16, ROOT_RECORD, "Empty", true, 0, 0)];
        let p = paths(&[(16, "Empty")]);
        let map = to_stat_map(&entries, &p, r"C:\", false, true);
        assert!(
            map.contains_key(std::path::Path::new(r"C:\Empty")),
            "the enumeration backend lists it, so this one must too"
        );
    }

    #[test]
    fn logical_only_charges_the_apparent_size() {
        let entries = vec![
            entry(16, ROOT_RECORD, "d", true, 0, 0),
            entry(17, 16, "sparse.img", false, 1_000_000, 4096),
        ];
        let p = paths(&[(16, "d")]);
        let map = to_stat_map(&entries, &p, r"C:\", false, false);
        let d = map.get(std::path::Path::new(r"C:\d")).expect("d");
        assert_eq!(
            d.physical, 1_000_000,
            "compute_physical=false means logical"
        );
    }

    #[test]
    fn a_hardlinked_file_is_charged_once() {
        let mut a = entry(17, 16, "link-a.txt", false, 1000, 4096);
        a.hard_link_count = 2;
        let entries = vec![entry(16, ROOT_RECORD, "d", true, 0, 0), a.clone(), a];
        let p = paths(&[(16, "d")]);
        let map = to_stat_map(&entries, &p, r"C:\", false, true);
        assert_eq!(
            map.get(std::path::Path::new(r"C:\d")).expect("d").files,
            1,
            "GNU du charges a hardlinked file to the first link only"
        );
    }

    #[test]
    fn count_hardlinks_charges_every_link() {
        let mut a = entry(17, 16, "link-a.txt", false, 1000, 4096);
        a.hard_link_count = 2;
        let entries = vec![entry(16, ROOT_RECORD, "d", true, 0, 0), a.clone(), a];
        let p = paths(&[(16, "d")]);
        let map = to_stat_map(&entries, &p, r"C:\", true, true);
        assert_eq!(map.get(std::path::Path::new(r"C:\d")).expect("d").files, 2);
    }

    #[test]
    fn a_file_whose_parent_is_unknown_is_dropped() {
        // Same choice `paths_for` makes for orphans, for the same reason.
        let entries = vec![entry(17, 999, "orphan.txt", false, 1000, 4096)];
        let map = to_stat_map(&entries, &paths(&[]), r"C:\", false, true);
        assert!(map.is_empty());
    }

    #[test]
    fn a_file_directly_under_the_root_lands_at_the_root() {
        let entries = vec![entry(16, ROOT_RECORD, "top.txt", false, 500, 4096)];
        // The root's own path is the empty string.
        let p = paths(&[(ROOT_RECORD, "")]);
        let map = to_stat_map(&entries, &p, r"C:\", false, true);
        assert_eq!(
            map.get(std::path::Path::new(r"C:\")).expect("root").files,
            1
        );
    }

    #[test]
    fn the_prefix_is_not_doubled_when_it_already_ends_in_a_separator() {
        assert_eq!(
            join_path(r"C:\", "Users"),
            std::path::PathBuf::from(r"C:\Users")
        );
        assert_eq!(
            join_path(r"C:", "Users"),
            std::path::PathBuf::from(r"C:\Users")
        );
    }

    // --- the real volume -----------------------------------------------------
    //
    // These do not assert on the volume's contents -- that needs elevation, and
    // a test that only runs when elevated is a test that mostly does not run.
    // What they do pin down is the branch every unelevated run takes.

    #[cfg(windows)]
    #[test]
    fn opening_a_volume_agrees_with_the_elevation_check() {
        // The two must not disagree: reporting "needs administrator rights"
        // while the open would have worked, or vice versa, sends the caller
        // down the wrong path.
        let elevated = is_elevated();
        let opened = WindowsVolume::open('C').is_some();
        assert!(
            !opened || elevated,
            "a volume opened without elevation means the check is wrong"
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_invalid_drive_letter_is_refused_without_touching_the_disk() {
        assert!(WindowsVolume::open('1').is_none());
        assert!(WindowsVolume::open('/').is_none());
    }

    /// Append an `$ATTRIBUTE_LIST` naming `records` as holding more `$DATA`.
    fn push_attribute_list(rec: &mut [u8], pos: usize, records: &[u64]) -> usize {
        let entry_len = 32usize; // 26 rounded up to 8
        let value_len = records.len() * entry_len;
        let value_off = 24usize;
        let total = (value_off + value_len).next_multiple_of(8);

        rec[pos..pos + 4].copy_from_slice(&attr_type::ATTRIBUTE_LIST.to_le_bytes());
        rec[pos + 4..pos + 8].copy_from_slice(&(total as u32).to_le_bytes());
        rec[pos + 8] = 0; // resident
        rec[pos + 16..pos + 20].copy_from_slice(&(value_len as u32).to_le_bytes());
        rec[pos + 20..pos + 22].copy_from_slice(&(value_off as u16).to_le_bytes());

        for (i, r) in records.iter().enumerate() {
            let e = pos + value_off + i * entry_len;
            rec[e..e + 4].copy_from_slice(&attr_type::DATA.to_le_bytes());
            rec[e + 4..e + 6].copy_from_slice(&(entry_len as u16).to_le_bytes());
            rec[e + 16..e + 24].copy_from_slice(&r.to_le_bytes());
        }
        pos + total
    }

    /// A `$MFT` record whose `$DATA` covers `clusters` from `MFT_LCN`, plus an
    /// `$ATTRIBUTE_LIST` pointing at `extensions` for the rest.
    fn mft_record_with_extensions(clusters: u64, extensions: &[u64]) -> Vec<u8> {
        let mut r = blank_record(0x0001, 1);
        let mut p = 64usize;
        p = push_attribute_list(&mut r, p, extensions);

        let total = 72usize;
        r[p..p + 4].copy_from_slice(&attr_type::DATA.to_le_bytes());
        r[p + 4..p + 8].copy_from_slice(&(total as u32).to_le_bytes());
        r[p + 8] = 1; // non-resident
        let run_off = 0x40u16;
        r[p + 0x20..p + 0x22].copy_from_slice(&run_off.to_le_bytes());
        let ro = p + run_off as usize;
        r[ro] = 0x11;
        r[ro + 1] = clusters as u8;
        r[ro + 2] = MFT_LCN as u8;
        r[ro + 3] = 0x00;
        set_used(&mut r, (p + total) as u32);
        r
    }

    /// An extension record holding a further `$DATA` run for `$MFT`.
    ///
    /// Its run list is parsed on its own, so the offset byte is an absolute LCN
    /// rather than a delta from a run in the base record.
    fn extension_record(clusters: u64, lcn: i8) -> Vec<u8> {
        let mut r = blank_record(0x0001, 1);
        let pos = 64usize;
        let total = 72usize;
        r[pos..pos + 4].copy_from_slice(&attr_type::DATA.to_le_bytes());
        r[pos + 4..pos + 8].copy_from_slice(&(total as u32).to_le_bytes());
        r[pos + 8] = 1;
        r[pos + 0x20..pos + 0x22].copy_from_slice(&0x40u16.to_le_bytes());
        let ro = pos + 0x40;
        r[ro] = 0x11;
        r[ro + 1] = clusters as u8;
        r[ro + 2] = lcn as u8;
        r[ro + 3] = 0x00;
        set_used(&mut r, (pos + total) as u32);
        r
    }

    // The case a real volume failed on: `$MFT` is itself fragmented enough that
    // its own $DATA spills into an extension record. Reading only the base
    // record found 11% of a 10-million-file volume -- and reported a
    // plausible-looking total for it. See #15.
    #[test]
    fn an_mft_whose_runs_spill_into_an_extension_record_is_read_in_full() {
        // Four records fit in a cluster, so record 16 sits in the fifth. The
        // base record's own run covers only the first cluster; the extension
        // record supplies the next four, which is what makes 16 reachable.
        let mut records = vec![mft_record_with_extensions(1, &[1]), extension_record(4, 5)];
        while records.len() < 16 {
            records.push(blank_record(0x0000, 0));
        }
        records.push(file_record(
            ROOT_RECORD,
            "in-second-extent.txt",
            4096,
            77,
            1,
        ));

        let mut v = boot_sector();
        v.resize(MFT_LCN as usize * CLUSTER, 0);
        for r in &records {
            v.extend_from_slice(r);
        }
        v.resize(MFT_LCN as usize * CLUSTER + 8 * CLUSTER, 0);

        let mut reader = MftReader::open(MemVolume(v)).expect("open");
        assert_eq!(
            reader.mft_clusters(),
            5,
            "the extension record's run must be appended; without it the reader \
             sees one cluster of MFT and stops there"
        );
        assert_eq!(reader.record_count(), 20);
        assert_eq!(
            reader.entry(16).expect("record 16").name,
            "in-second-extent.txt",
            "record 16 lives past the base record's own run, so reaching it \
             proves the extension was followed"
        );
    }

    #[test]
    fn an_attribute_list_naming_the_base_record_does_not_double_count() {
        // Record 0 is the base; following it again would add its runs twice.
        let mut records = vec![mft_record_with_extensions(2, &[0])];
        while records.len() < 16 {
            records.push(blank_record(0x0000, 0));
        }
        let mut v = boot_sector();
        v.resize(MFT_LCN as usize * CLUSTER, 0);
        for r in &records {
            v.extend_from_slice(r);
        }
        v.resize(MFT_LCN as usize * CLUSTER + 8 * CLUSTER, 0);

        let reader = MftReader::open(MemVolume(v)).expect("open");
        assert_eq!(
            reader.mft_clusters(),
            2,
            "the base record's own runs must not be added a second time"
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
