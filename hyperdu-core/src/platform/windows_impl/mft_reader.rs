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

// Parser helpers are also compiled by portable fixtures without a Windows
// volume backend; some diagnostic helpers are unused in that test harness.
#![allow(dead_code)]

#[cfg(windows)]
#[path = "mft_reader/overlapped.rs"]
mod overlapped;
#[path = "mft_reader/progress.rs"]
mod progress;
#[path = "mft_reader/streams.rs"]
mod streams;
#[path = "mft_reader/window.rs"]
mod window;
#[cfg(test)]
use std::collections::HashMap;

pub(crate) use progress::ReadProgress;

#[cfg(test)]
use super::mft::build_path;
use super::mft::{
    allocated_clusters, apply_fixups, attr_type, distinct_links, namespace, parse_boot_sector,
    parse_file_name, parse_record_header, parse_run_list, Attributes, DataSizes, FileName,
    Geometry, Run,
};

/// Somewhere MFT bytes can be read from: a volume handle in production, a
/// `Vec<u8>` in tests.
pub(crate) trait VolumeSource {
    /// Fill `buf` from `offset`. Returns false when the range is not readable,
    /// which the caller treats as the end of usable data rather than retrying.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool;

    /// Start at most one owned read-ahead operation without borrowing a caller's
    /// buffer. False means unsupported or failed; normal read_at remains usable.
    fn begin_prefetch(&mut self, _offset: u64, _length: usize) -> bool {
        false
    }
    /// Drain the original operation and copy only a complete result into buf.
    fn finish_prefetch(&mut self, _buf: &mut [u8]) -> bool {
        false
    }
    /// Cancel and synchronously drain any pending operation before returning.
    fn cancel_prefetch(&mut self) {}
}

/// Lets a caller keep ownership of the volume while the reader borrows it, so
/// the sector size can be narrowed after the geometry is known.
impl<S: VolumeSource + ?Sized> VolumeSource for &mut S {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        (**self).read_at(offset, buf)
    }
    fn begin_prefetch(&mut self, offset: u64, length: usize) -> bool {
        (**self).begin_prefetch(offset, length)
    }
    fn finish_prefetch(&mut self, buf: &mut [u8]) -> bool {
        (**self).finish_prefetch(buf)
    }
    fn cancel_prefetch(&mut self) {
        (**self).cancel_prefetch();
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
    /// `$DATA` attribute flags: compressed, sparse, encrypted. Kept because the
    /// allocated size means something different for each, and the MFT and the
    /// enumeration API disagreed by 37% on a real volume until each was read
    /// its own way (#39).
    pub(crate) data_flags: u16,
    /// Why this record's sizes are what they are. The MFT still reports 6.1 GB
    /// less than enumeration (#41) and the candidate causes are distinguishable
    /// only by counting them: guessing which one dominates is how #39 went
    /// wrong twice.
    pub(crate) size_source: SizeSource,
}

/// Where a record's sizes came from, and what else it carries that could
/// account for a discrepancy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SizeSource {
    /// False when no unnamed `$DATA` was found and the stale copy in
    /// `$FILE_NAME` had to stand in.
    pub(crate) from_data_attribute: bool,
    /// The record has an `$ATTRIBUTE_LIST`, so some of its attributes --
    /// possibly the rest of `$DATA` -- live in extension records. Ordinary-file
    /// DATA extents are resolved before an entry is emitted.
    pub(crate) has_attribute_list: bool,
    /// Bytes in named `$DATA` streams, identified by name rather than order.
    /// These remain diagnostic only; the enumeration API may count them. WOF-compressed files keep their real
    /// contents here.
    pub(crate) named_stream_bytes: u64,
}

impl Default for SizeSource {
    fn default() -> Self {
        Self {
            from_data_attribute: true,
            has_attribute_list: false,
            named_stream_bytes: 0,
        }
    }
}

/// Reads records from the `$MFT` of a volume.
pub(crate) struct MftReader<S: VolumeSource> {
    source: S,
    geometry: Geometry,
    /// Where the MFT's own data lives. The MFT is a file and can be fragmented;
    /// without these runs only its first extent is reachable, and the scan
    /// would stop early while still reporting a plausible total.
    runs: Vec<Run>,
    window: window::ReadWindow,
    /// False after any required record or DATA extent could not be resolved.
    complete: bool,
}

impl<S: VolumeSource> Drop for MftReader<S> {
    fn drop(&mut self) {
        self.source.cancel_prefetch();
    }
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

        if !header.in_use || header.is_directory || bootstrap_u64(&rec, 32)? != 0 {
            return None;
        }
        let base_reference = (bootstrap_u16(&rec, 16)? as u64) << 48;
        let mut extents = bootstrap_extents(&rec, &header)?;
        extents.sort_unstable_by_key(|extent| extent.low);
        let first = extents.first()?;
        if first.low != 0 {
            return None;
        }
        let allocated = bootstrap_u64(&rec, first.pos + 40)?;
        let logical = bootstrap_u64(&rec, first.pos + 48)?;
        let initialized = bootstrap_u64(&rec, first.pos + 56)?;
        let cluster = geometry.cluster_size() as u64;
        if allocated == 0
            || allocated % cluster != 0
            || logical > allocated
            || initialized > logical
        {
            return None;
        }
        let expected_clusters = allocated / cluster;
        let references = bootstrap_references(&rec, &header)?;
        let mut reader = Self {
            source,
            geometry,
            runs: Vec::new(),
            complete: true,
            window: window::ReadWindow::default(),
        };
        let mut end = 0;
        for extent in &extents {
            if extent.low != end || extent.end > expected_clusters {
                return None;
            }
            end = extent.end;
            reader.runs.extend_from_slice(&extent.runs);
        }
        // A partial bootstrap must never be reported as a complete MFT. Keep
        // the valid prefix only so callers retain the existing fallback signal.
        reader.complete = reader
            .extend_runs_from(
                &references,
                &extents,
                base_reference,
                end,
                expected_clusters,
            )
            .is_some_and(|end| end == expected_clusters);
        Some(reader)
    }

    /// Extend only the contiguous VCN prefix. A record needed for the next
    /// extent must be reachable through that prefix; later extents cannot be
    /// appended early just to make an unresolved reference appear reachable.
    fn extend_runs_from(
        &mut self,
        references: &[BootstrapReference],
        base_extents: &[BootstrapExtent],
        base_reference: u64,
        mut end: u64,
        expected_clusters: u64,
    ) -> Option<u64> {
        for reference in references {
            if reference.record & BOOTSTRAP_RECORD_MASK == 0 {
                if reference.record != base_reference
                    || !base_extents.iter().any(|extent| {
                        extent.low == reference.low && extent.instance == reference.instance
                    })
                {
                    return None;
                }
                continue;
            }
            if reference.low != end {
                return None;
            }
            let rec = self.read_record(reference.record & BOOTSTRAP_RECORD_MASK)?;
            let header = parse_record_header(&rec)?;
            if !header.in_use
                || header.is_directory
                || bootstrap_u16(&rec, 16)? != (reference.record >> 48) as u16
                || bootstrap_u64(&rec, 32)? != base_reference
            {
                return None;
            }
            let extents = bootstrap_extents(&rec, &header)?;
            let mut matching = extents.iter().filter(|extent| {
                extent.low == reference.low && extent.instance == reference.instance
            });
            let extent = matching.next()?;
            if matching.next().is_some() || extent.end > expected_clusters {
                return None;
            }
            end = extent.end;
            self.runs.extend_from_slice(&extent.runs);
        }
        Some(end)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    pub(crate) fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// The volume, so the caller can narrow the read alignment once the
    /// geometry is known. Doing that after the records are read achieves
    /// nothing, which is what used to happen.
    pub(crate) fn source_mut(&mut self) -> &mut S {
        self.drain_prefetch();
        self.window.clear();
        &mut self.source
    }

    /// Total clusters the MFT occupies, which bounds how many records exist.
    pub(crate) fn mft_clusters(&self) -> u64 {
        allocated_clusters(&self.runs)
    }

    /// How many extents the MFT's run list describes. A truncated list is the
    /// difference between reading the whole MFT and reading part of it while
    /// reporting a plausible total.
    pub(crate) fn run_count(&self) -> usize {
        self.runs.len()
    }

    /// How many records the MFT can hold. Reading past this is not an error to
    /// report, just the end.
    pub(crate) fn record_count(&self) -> u64 {
        let bytes = self.mft_clusters() * self.geometry.cluster_size() as u64;
        bytes / self.geometry.record_size as u64
    }

    /// Read one record by number, with fixups already applied.
    ///
    /// Returns `None` for an out-of-range, unreadable or torn record. Required
    /// extension callers must invalidate completeness on failure.
    pub(crate) fn read_record(&mut self, number: u64) -> Option<Vec<u8>> {
        let mut buf = self.raw_record(number, false)?;
        if !apply_fixups(&mut buf, self.geometry.bytes_per_sector) {
            return None;
        }
        Some(buf)
    }

    /// Extract the entry for a record, or `None` when it holds nothing the scan
    /// cares about (deleted or unnamed). Unreadable records invalidate completeness.
    pub(crate) fn entry(&mut self, number: u64) -> Option<Entry> {
        let Some(mut rec) = self.raw_record(number, true) else {
            self.complete = false;
            return None;
        };
        // Allocated MFT capacity can contain uninitialized slots. Only an
        // entirely zero slot is accepted without a valid FILE header/fixups.
        if rec.iter().all(|byte| *byte == 0) {
            return None;
        }
        if !apply_fixups(&mut rec, self.geometry.bytes_per_sector) {
            self.complete = false;
            return None;
        }
        let Some(header) = parse_record_header(&rec) else {
            self.complete = false;
            return None;
        };
        // An extension belongs to its base record, never a separate file.
        if !header.in_use || u64::from_le_bytes(rec.get(32..40)?.try_into().ok()?) != 0 {
            return None;
        }

        let mut names: Vec<FileName> = Vec::new();
        let mut source = SizeSource::default();
        let mut reparse = false;
        let mut reparse_hint = false;
        let mut attr_end = header.first_attr_offset;
        for attr in Attributes::new(&rec, &header) {
            attr_end = attr.pos + attr.total_length;
            match attr.type_code {
                attr_type::FILE_NAME | attr_type::STANDARD_INFORMATION => {
                    let value =
                        rec.get(attr.value_offset..attr.value_offset + attr.value_length)?;
                    let flags_offset = if attr.type_code == attr_type::FILE_NAME {
                        56
                    } else {
                        32
                    };
                    let Some(flags) = value.get(flags_offset..flags_offset + 4) else {
                        self.complete = false;
                        return None;
                    };
                    reparse_hint |= u32::from_le_bytes(flags.try_into().ok()?) & 0x400 != 0;
                    if attr.type_code == attr_type::FILE_NAME {
                        if let Some(f) = parse_file_name(value) {
                            names.push(f);
                        }
                    }
                }
                attr_type::REPARSE_POINT => {
                    let value =
                        rec.get(attr.value_offset..attr.value_offset + attr.value_length)?;
                    if reparse
                        || attr.non_resident
                        || rec.get(attr.pos + 9) != Some(&0)
                        || attr.value_offset < attr.pos + 24
                        || super::mft::parse_reparse_link(value).is_none()
                    {
                        self.complete = false;
                        return None;
                    }
                    reparse = true;
                }
                attr_type::ATTRIBUTE_LIST => source.has_attribute_list = true,
                _ => {}
            }
        }
        // Attributes stops on malformed headers. Do not mistake a truncated
        // reparse attribute for an ordinary file, even when flags are missing.
        if attr_end < header.used_size as usize
            && rec.get(attr_end..attr_end + 4) != Some(&attr_type::END.to_le_bytes())
        {
            self.complete = false;
            return None;
        }
        if reparse_hint && !reparse {
            // The tag may be in an extension; without it no-follow semantics
            // cannot be established from duplicated FILE_NAME size metadata.
            self.complete = false;
            return None;
        }
        if reparse {
            return None;
        }
        let resolved = match self.data_streams(number, &rec, &header) {
            Some(streams) => streams,
            None => {
                self.complete = false;
                return None;
            }
        };
        let sizes = resolved.unnamed;
        let data_flags = resolved.flags;
        source.named_stream_bytes = resolved.named_bytes;
        source.from_data_attribute = sizes.is_some();

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
            // Extensions are resolved above. Retain the legacy fallback only
            // for records without a DATA attribute, never for a failed extent.
            sizes: sizes.unwrap_or(DataSizes {
                real_size: chosen.real_size,
                allocated_size: chosen.allocated_size,
            }),
            hard_link_count: header.hard_link_count,
            data_flags,
            size_source: source,
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
        self.entries_with_control(|_| true).unwrap_or_default()
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
#[cfg(test)]
pub(crate) fn to_stat_map(
    entries: &[Entry],
    paths: &HashMap<u64, String>,
    root_prefix: &str,
    count_hardlinks: bool,
    compute_physical: bool,
) -> crate::StatMap {
    let mut map: crate::StatMap = crate::StatMap::default();
    let mut counted_links: std::collections::HashSet<u64> = std::collections::HashSet::new();

    // The volume root, which no record supplies: `entries` starts at the first
    // user record and the root is record 5, below it. The walking backends
    // insert each directory before reading it, so their root is always present;
    // without this the rolled-up totals have nowhere to accumulate and the
    // whole-volume figure reads as zero.
    map.entry(join_path(root_prefix, "")).or_default();

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
        let parent_path = if e.parent == super::mft::ROOT_RECORD {
            // Record 5 is deliberately excluded from `entries`; its children
            // still belong to the volume root, not to an unknown parent.
            ""
        } else if let Some(path) = paths.get(&e.parent) {
            path.as_str()
        } else {
            // Genuine orphans remain excluded rather than assigned a parent.
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

#[cfg(test)]
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
    prefetch: Option<overlapped::Engine>,
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
            prefetch: overlapped::Engine::open(&path, handle, overlapped::Mode::configured()),
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
        self.cancel_prefetch();
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
impl VolumeSource for WindowsVolume {
    fn begin_prefetch(&mut self, offset: u64, length: usize) -> bool {
        self.prefetch
            .as_mut()
            .is_some_and(|io| io.begin(offset, length))
    }
    fn finish_prefetch(&mut self, buf: &mut [u8]) -> bool {
        self.prefetch.as_mut().is_some_and(|io| io.finish(buf))
    }
    fn cancel_prefetch(&mut self) {
        if let Some(io) = &mut self.prefetch {
            io.cancel();
        }
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> bool {
        use windows::Win32::{
            Storage::FileSystem::{ReadFile, SetFilePointerEx, FILE_BEGIN},
            System::IO::OVERLAPPED,
        };

        // Widen to sector boundaries: a volume handle rejects anything else.
        let start = offset - (offset % self.sector);
        let Some(end) = offset
            .checked_add(buf.len() as u64)
            .and_then(|end| end.checked_add(self.sector - 1))
            .map(|end| end / self.sector * self.sector)
        else {
            return false;
        };
        if end > i64::MAX as u64 || end - start > u32::MAX as u64 {
            return false;
        }
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

const BOOTSTRAP_RECORD_MASK: u64 = (1 << 48) - 1;

struct BootstrapReference {
    record: u64,
    low: u64,
    instance: u16,
}

struct BootstrapExtent {
    pos: usize,
    low: u64,
    end: u64,
    instance: u16,
    runs: Vec<Run>,
}

fn bootstrap_u16(bytes: &[u8], pos: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(pos..pos.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn bootstrap_u64(bytes: &[u8], pos: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(pos..pos.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// The shared iterator deliberately stops on a malformed attribute. Bootstrap
/// must distinguish that stop from a real END marker or the exact used size.
fn validate_bootstrap_attributes(rec: &[u8], header: &super::mft::RecordHeader) -> Option<()> {
    let mut end = header.first_attr_offset;
    for attr in Attributes::new(rec, header) {
        end = attr.pos.checked_add(attr.total_length)?;
    }
    let used = header.used_size as usize;
    if end == used
        || (end.checked_add(4)? <= used && rec.get(end..end + 4)? == attr_type::END.to_le_bytes())
    {
        Some(())
    } else {
        None
    }
}

/// Read only unnamed DATA mappings, bounded by each attribute's own end.
/// Bootstrap cannot use sparse/compressed/encrypted mappings as raw MFT bytes.
fn bootstrap_extents(
    rec: &[u8],
    header: &super::mft::RecordHeader,
) -> Option<Vec<BootstrapExtent>> {
    validate_bootstrap_attributes(rec, header)?;
    let mut extents = Vec::new();
    for attr in Attributes::new(rec, header).filter(|attr| attr.type_code == attr_type::DATA) {
        let bytes = rec.get(attr.pos..attr.pos.checked_add(attr.total_length)?)?;
        if *bytes.get(9)? != 0 {
            continue;
        }
        if !attr.non_resident || attr.flags != 0 || bytes.len() < 64 {
            return None;
        }
        let low = bootstrap_u64(bytes, 16)?;
        let end = bootstrap_u64(bytes, 24)?.checked_add(1)?;
        if end <= low {
            return None;
        }
        let offset = bootstrap_u16(bytes, 32)? as usize;
        if offset < 64 {
            return None;
        }
        let mapping = bytes.get(offset..)?;
        // parse_run_list also accepts an unterminated final run, so locate an
        // actual terminator without permitting it to come from the next attr.
        let mut pos = 0usize;
        while *mapping.get(pos)? != 0 {
            let header = mapping[pos];
            let length_bytes = (header & 15) as usize;
            let offset_bytes = (header >> 4) as usize;
            if length_bytes == 0 || length_bytes > 8 || offset_bytes == 0 || offset_bytes > 8 {
                return None;
            }
            pos = pos.checked_add(1 + length_bytes + offset_bytes)?;
        }
        let runs = parse_run_list(&mapping[..=pos])?;
        let covered = runs.iter().try_fold(0u64, |sum, run| {
            if run.length == 0 {
                None
            } else {
                sum.checked_add(run.length)
            }
        })?;
        if covered != end - low {
            return None;
        }
        extents.push(BootstrapExtent {
            pos: attr.pos,
            low,
            end,
            instance: bootstrap_u16(bytes, 14)?,
            runs,
        });
    }
    Some(extents)
}

/// Keep full segment identity, instance and VCN; named DATA is a different
/// stream and cannot extend the unnamed MFT mapping. Non-resident bootstrap
/// lists remain unsupported and trigger enumeration fallback.
fn bootstrap_references(
    rec: &[u8],
    header: &super::mft::RecordHeader,
) -> Option<Vec<BootstrapReference>> {
    let mut out = Vec::new();
    let mut has_list = false;
    for attr in
        Attributes::new(rec, header).filter(|attr| attr.type_code == attr_type::ATTRIBUTE_LIST)
    {
        if has_list || attr.non_resident {
            return None;
        }
        has_list = true;
        let value =
            rec.get(attr.value_offset..attr.value_offset.checked_add(attr.value_length)?)?;
        let mut pos = 0usize;
        while pos < value.len() {
            let length = bootstrap_u16(value, pos.checked_add(4)?)? as usize;
            if length < 26 {
                return None;
            }
            let entry = value.get(pos..pos.checked_add(length)?)?;
            let name_len = *entry.get(6)? as usize;
            if name_len != 0 {
                let name_offset = *entry.get(7)? as usize;
                if name_offset < 26 || name_offset % 2 != 0 {
                    return None;
                }
                entry.get(name_offset..name_offset.checked_add(name_len.checked_mul(2)?)?)?;
            }
            let kind = u32::from_le_bytes(entry.get(..4)?.try_into().ok()?);
            if kind == attr_type::DATA && name_len == 0 {
                out.push(BootstrapReference {
                    record: bootstrap_u64(entry, 16)?,
                    low: bootstrap_u64(entry, 8)?,
                    instance: bootstrap_u16(entry, 24)?,
                });
            }
            pos = pos.checked_add(length)?;
        }
    }
    out.sort_unstable_by_key(|reference| reference.low);
    // A duplicate VCN is ambiguous even if both references name the same
    // record. Never discard a conflicting sequence or instance by deduping IDs.
    if out.windows(2).any(|pair| pair[0].low == pair[1].low) {
        return None;
    }
    Some(out)
}

/// Build `record -> path` for a set of entries.
///
/// Entries whose parent chain does not reach the root are dropped rather than
/// attached somewhere plausible: putting an orphan under a guessed parent moves
/// its bytes into a directory that does not contain it.
#[cfg(test)]
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
    include!("mft_reader/batching_tests.rs");
    include!("mft_reader/async_tests.rs");
    include!("mft_reader/bootstrap_tests.rs");
    include!("mft_reader/progress_tests.rs");
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
        set_mft_extent_sizes(&mut r, pos, 0, clusters, CLUSTER as u64);
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

    fn set_mft_extent_sizes(rec: &mut [u8], pos: usize, low: u64, clusters: u64, cluster: u64) {
        rec[pos + 16..pos + 24].copy_from_slice(&low.to_le_bytes());
        rec[pos + 24..pos + 32].copy_from_slice(&(low + clusters - 1).to_le_bytes());
        if low == 0 {
            for offset in [40, 48, 56] {
                rec[pos + offset..pos + offset + 8]
                    .copy_from_slice(&(clusters * cluster).to_le_bytes());
            }
        }
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

    fn append_resident(rec: &mut [u8], kind: u32, value: &[u8]) -> usize {
        let pos = u32::from_le_bytes(rec[24..28].try_into().unwrap()) as usize;
        let total = (24 + value.len()).next_multiple_of(8);
        rec[pos..pos + total].fill(0);
        rec[pos..pos + 4].copy_from_slice(&kind.to_le_bytes());
        rec[pos + 4..pos + 8].copy_from_slice(&(total as u32).to_le_bytes());
        rec[pos + 16..pos + 20].copy_from_slice(&(value.len() as u32).to_le_bytes());
        rec[pos + 20..pos + 22].copy_from_slice(&24u16.to_le_bytes());
        rec[pos + 24..pos + 24 + value.len()].copy_from_slice(value);
        set_used(rec, (pos + total) as u32);
        pos
    }

    fn link_record(tag: u32, directory: bool) -> Vec<u8> {
        let mut rec = if directory {
            dir_record(5, "link")
        } else {
            file_record(5, "link", 4096, 7, 1)
        };
        let fixed = if tag == 0xa000_000c { 12 } else { 8 };
        let mut value = vec![0; 8 + fixed + 2];
        value[..4].copy_from_slice(&tag.to_le_bytes());
        value[4..6].copy_from_slice(&((fixed + 2) as u16).to_le_bytes());
        value[10..12].copy_from_slice(&2u16.to_le_bytes());
        value[8 + fixed..].copy_from_slice(&[b'x', 0]);
        // The duplicated FILE_NAME attributes flag reparse metadata as well.
        rec[64 + 24 + 56..64 + 24 + 60].copy_from_slice(&0x400u32.to_le_bytes());
        append_resident(&mut rec, 0xc0, &value);
        rec
    }

    #[test]
    fn known_reparse_links_are_skipped_without_losing_ordinary_records() {
        let records = vec![
            link_record(0xa000_000c, false),
            link_record(0xa000_000c, true),
            link_record(0xa000_0003, true),
            file_record(5, "ordinary", 4096, 17, 1),
        ];
        let mut reader = MftReader::open(volume(with_metadata_records(records, 5))).unwrap();
        let entries = reader.entries();
        assert!(reader.is_complete());
        assert_eq!(
            entries.len(),
            1,
            "no-follow links must not become file or directory entries"
        );
        assert_eq!(entries[0].name, "ordinary");
        assert_eq!(entries[0].sizes.real_size, 17);
    }

    #[test]
    fn unresolved_reparse_metadata_invalidates_the_whole_scan() {
        for case in 0..8 {
            let mut rec = link_record(0xa000_000c, false);
            let header = parse_record_header(&rec).unwrap();
            let attr = Attributes::new(&rec, &header)
                .find(|a| a.type_code == 0xc0)
                .unwrap();
            match case {
                0 => rec[attr.value_offset..attr.value_offset + 4]
                    .copy_from_slice(&0x8000_001bu32.to_le_bytes()),
                1 => rec[attr.pos + 8] = 1, // unsupported non-resident value
                2 => rec[attr.pos + 16..attr.pos + 20].copy_from_slice(&3u32.to_le_bytes()),
                3 => rec[attr.value_offset + 4..attr.value_offset + 6]
                    .copy_from_slice(&100u16.to_le_bytes()),
                4 => set_used(&mut rec, attr.pos as u32), // flags but missing base attribute
                5 => {
                    // STANDARD_INFORMATION alone must also detect a missing tag.
                    rec[64 + 24 + 56..64 + 24 + 60].fill(0);
                    set_used(&mut rec, attr.pos as u32);
                    let mut info = [0u8; 36];
                    info[32..36].copy_from_slice(&0x400u32.to_le_bytes());
                    append_resident(&mut rec, 0x10, &info);
                }
                6 => rec[attr.pos + 4..attr.pos + 8].copy_from_slice(&8u32.to_le_bytes()),
                7 => rec[attr.value_offset + 8..attr.value_offset + 10]
                    .copy_from_slice(&200u16.to_le_bytes()),
                _ => unreachable!(),
            }
            let mut reader = MftReader::open(volume(with_metadata_records(vec![rec], 5))).unwrap();
            assert!(
                reader.entries().is_empty(),
                "unsupported reparse case {case}"
            );
            assert!(
                !reader.is_complete(),
                "unsupported reparse case {case} must decline"
            );
        }
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
            data_flags: 0,
            size_source: SizeSource::default(),
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
        let root = map.get(std::path::Path::new(r"C:\")).expect("root");
        assert_eq!(root.files, 0, "the orphan is dropped");
        assert_eq!(map.len(), 1, "only the root remains");
    }

    // No record supplies the volume root: `entries` starts at the first user
    // record and the root is record 5. Without an explicit entry the rolled-up
    // totals have nowhere to accumulate and the whole-volume figure reads as
    // zero -- which is exactly what a real volume did. See #37.
    #[test]
    fn the_volume_root_is_always_in_the_map() {
        let empty = to_stat_map(&[], &paths(&[]), r"C:\", false, true);
        assert!(
            empty.contains_key(std::path::Path::new(r"C:\")),
            "even an empty volume must have a root entry to roll up into"
        );

        // And with real entries whose paths never mention the root.
        let entries = vec![
            entry(16, ROOT_RECORD, "Windows", true, 0, 0),
            entry(17, 16, "notepad.exe", false, 1000, 4096),
        ];
        let p = paths(&[(16, "Windows"), (17, r"Windows\notepad.exe")]);
        let map = to_stat_map(&entries, &p, r"C:\", false, true);
        assert!(map.contains_key(std::path::Path::new(r"C:\")));
        assert!(map.contains_key(std::path::Path::new(r"C:\Windows")));
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
        for (index, reference) in extensions.iter().enumerate() {
            if reference & BOOTSTRAP_RECORD_MASK != 0 {
                let entry = 64 + 24 + index * 32;
                r[entry + 8..entry + 16].copy_from_slice(&clusters.to_le_bytes());
            }
        }

        let total = 72usize;
        r[p..p + 4].copy_from_slice(&attr_type::DATA.to_le_bytes());
        r[p + 4..p + 8].copy_from_slice(&(total as u32).to_le_bytes());
        r[p + 8] = 1; // non-resident
        set_mft_extent_sizes(&mut r, p, 0, clusters, CLUSTER as u64);
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
        set_mft_extent_sizes(&mut r, pos, 1, clusters, CLUSTER as u64);
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
        let data = 64 + 24 + 32;
        for offset in [40, 48, 56] {
            records[0][data + offset..data + offset + 8]
                .copy_from_slice(&(5 * CLUSTER as u64).to_le_bytes());
        }
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
        set_mft_extent_sizes(&mut mft, pos, 0, 2, CLUSTER as u64);
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

    // Regression fixtures for #41 use actual stream names and extension links.
    fn named_data(rec: &mut [u8], pos: usize, name: &str, alloc: u64, real: u64) -> usize {
        let units: Vec<u16> = name.encode_utf16().collect();
        let end = push_nonresident_data(rec, pos, alloc, real);
        let total = (72 + units.len() * 2 + 7) & !7;
        rec[pos + 4..pos + 8].copy_from_slice(&(total as u32).to_le_bytes());
        rec[pos + 9] = units.len() as u8;
        rec[pos + 10..pos + 12].copy_from_slice(&72u16.to_le_bytes());
        for (n, unit) in units.iter().enumerate() {
            rec[end + n * 2..end + n * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
        pos + total
    }

    #[test]
    fn issue41_named_stream_before_unnamed_is_not_file_contents() {
        let mut rec = blank_record(1, 1);
        let p = push_file_name(&mut rec, 64, ROOT_RECORD, "streams");
        let p = named_data(&mut rec, p, "ads", 8192, 8000);
        let p = push_nonresident_data(&mut rec, p, 4096, 3000);
        rec[p..p + 4].copy_from_slice(&attr_type::END.to_le_bytes());
        set_used(&mut rec, (p + 4) as u32);
        let vol = volume(with_metadata_records(vec![rec], 5));
        let entry = MftReader::open(vol).unwrap().entry(16).unwrap();
        assert_eq!(entry.sizes.real_size, 3000);
        assert_eq!(entry.sizes.allocated_size, 4096);
        assert_eq!(entry.size_source.named_stream_bytes, 8192);
    }

    #[test]
    fn issue41_data_in_extension_replaces_stale_filename_sizes() {
        let mut base = blank_record(1, 1);
        let p = push_file_name(&mut base, 64, ROOT_RECORD, "growing");
        let p = push_attribute_list(&mut base, p, &[17]);
        base[p..p + 4].copy_from_slice(&attr_type::END.to_le_bytes());
        set_used(&mut base, (p + 4) as u32);
        let mut extension = blank_record(1, 0);
        extension[32..40].copy_from_slice(&16u64.to_le_bytes());
        let p = push_nonresident_data(&mut extension, 64, 16384, 12345);
        extension[88..96].copy_from_slice(&3u64.to_le_bytes());
        extension[p..p + 4].copy_from_slice(&attr_type::END.to_le_bytes());
        set_used(&mut extension, (p + 4) as u32);
        let vol = volume(with_metadata_records(vec![base, extension], 5));
        let entry = MftReader::open(vol).unwrap().entry(16).unwrap();
        assert_eq!(entry.sizes.real_size, 12345);
        assert_eq!(entry.sizes.allocated_size, 16384);
        assert!(entry.size_source.from_data_attribute);
    }

    #[test]
    fn issue41_extension_records_are_not_independent_files() {
        let mut ext = file_record(ROOT_RECORD, "not-another-file", 4096, 100, 1);
        ext[32..40].copy_from_slice(&16u64.to_le_bytes());
        let vol = volume(with_metadata_records(vec![ext], 5));
        assert!(MftReader::open(vol).unwrap().entry(16).is_none());
    }

    fn data_extension_fixture(sparse: bool) -> (Vec<u8>, Vec<u8>) {
        let mut base = blank_record(1, 1);
        let data = push_file_name(&mut base, 64, ROOT_RECORD, "extents");
        let list = push_nonresident_data(&mut base, data, 16384, 16384);
        base[data + 24..data + 32].copy_from_slice(&1u64.to_le_bytes());
        let end = push_attribute_list(&mut base, list, &[16, 17]);
        // The second reference describes VCN 2, not another initial extent.
        base[list + 24 + 32 + 8..list + 24 + 32 + 16].copy_from_slice(&2u64.to_le_bytes());
        set_used(&mut base, end as u32);
        let mut extension = blank_record(1, 0);
        extension[32..40].copy_from_slice(&16u64.to_le_bytes());
        let end = push_nonresident_data(&mut extension, 64, 16384, 16384);
        extension[80..88].copy_from_slice(&2u64.to_le_bytes());
        extension[88..96].copy_from_slice(&3u64.to_le_bytes());
        set_used(&mut extension, end as u32);
        if sparse {
            base[data + 12..data + 14].copy_from_slice(&0x8000u16.to_le_bytes());
            base[data + 32..data + 34].copy_from_slice(&64u16.to_le_bytes());
            base[data + 64..data + 70].copy_from_slice(&[0x11, 1, 40, 0x01, 1, 0]);
            extension[76..78].copy_from_slice(&0x8000u16.to_le_bytes());
            extension[96..98].copy_from_slice(&64u16.to_le_bytes());
            extension[128..132].copy_from_slice(&[0x11, 2, 50, 0]);
        }
        (base, extension)
    }

    #[test]
    fn issue41_sparse_continuations_include_all_non_hole_clusters() {
        let (base, ext) = data_extension_fixture(true);
        let mut reader =
            MftReader::open(volume(with_metadata_records(vec![base, ext], 5))).unwrap();
        let entry = reader.entry(16).unwrap();
        assert_eq!(entry.sizes.allocated_size, 3 * CLUSTER as u64);
        assert_eq!(entry.sizes.real_size, 16384);
        assert_eq!(entry.size_source.named_stream_bytes, 0);
        assert!(reader.is_complete());
    }

    #[test]
    fn issue41_plain_continuations_do_not_sum_whole_stream_size_twice() {
        let (base, ext) = data_extension_fixture(false);
        let mut reader =
            MftReader::open(volume(with_metadata_records(vec![base, ext], 5))).unwrap();
        assert_eq!(reader.entry(16).unwrap().sizes.allocated_size, 16384);
        assert!(reader.is_complete());
    }

    #[test]
    fn issue41_missing_reused_or_wrong_owner_extension_requires_fallback() {
        for failure in 0..3 {
            let (base, mut ext) = data_extension_fixture(false);
            match failure {
                0 => ext[0..4].copy_from_slice(b"BAAD"),
                1 => ext[16..18].copy_from_slice(&1u16.to_le_bytes()),
                _ => ext[32..40].copy_from_slice(&99u64.to_le_bytes()),
            }
            let mut reader =
                MftReader::open(volume(with_metadata_records(vec![base, ext], 5))).unwrap();
            assert!(reader.entry(16).is_none(), "failure {failure}");
            assert!(!reader.is_complete(), "failure {failure}");
        }
    }

    #[test]
    fn issue41_malformed_sparse_runs_do_not_fall_back_to_inflated_header() {
        let (mut base, ext) = data_extension_fixture(true);
        let header = parse_record_header(&base).unwrap();
        let attr = Attributes::new(&base, &header)
            .find(|a| a.type_code == attr_type::DATA)
            .unwrap();
        // Eight nonzero bytes with no terminator inside this attribute.
        base[attr.pos + 64..attr.pos + 72].fill(0x11);
        let mut reader =
            MftReader::open(volume(with_metadata_records(vec![base, ext], 5))).unwrap();
        assert!(reader.entry(16).is_none());
        assert!(!reader.is_complete());
    }

    #[test]
    fn issue41_named_streams_remain_diagnostic_not_total_bytes() {
        let mut rec = blank_record(1, 1);
        let p = push_file_name(&mut rec, 64, ROOT_RECORD, "named");
        let p = push_nonresident_data(&mut rec, p, 4096, 100);
        let p = named_data(&mut rec, p, "WofCompressedData", 8192, 8000);
        set_used(&mut rec, p as u32);
        let mut reader = MftReader::open(volume(with_metadata_records(vec![rec], 5))).unwrap();
        let entries = reader.entries();
        assert!(reader.is_complete());
        assert_eq!(entries[0].size_source.named_stream_bytes, 8192);
        let map = to_stat_map(&entries, &paths_for(&entries), "C:\\", false, true);
        assert_eq!(map.values().map(|s| s.physical).sum::<u64>(), 4096);
    }

    #[test]
    fn issue41_nonresident_attribute_list_reads_referenced_data() {
        let mut base = blank_record(1, 1);
        let p = push_file_name(&mut base, 64, ROOT_RECORD, "external-list");
        let end = push_nonresident_data(&mut base, p, CLUSTER as u64, 32);
        base[p..p + 4].copy_from_slice(&attr_type::ATTRIBUTE_LIST.to_le_bytes());
        base[p + 32..p + 34].copy_from_slice(&64u16.to_le_bytes());
        base[p + 64..p + 68].copy_from_slice(&[0x11, 1, 80, 0]);
        set_used(&mut base, end as u32);
        let mut ext = blank_record(1, 0);
        ext[32..40].copy_from_slice(&16u64.to_le_bytes());
        let end = push_nonresident_data(&mut ext, 64, 4096, 123);
        set_used(&mut ext, end as u32);
        let mut vol = volume(with_metadata_records(vec![base, ext], 5));
        vol.0.resize(81 * CLUSTER, 0);
        let start = 80 * CLUSTER;
        vol.0[start..start + 4].copy_from_slice(&attr_type::DATA.to_le_bytes());
        vol.0[start + 4..start + 6].copy_from_slice(&32u16.to_le_bytes());
        vol.0[start + 16..start + 24].copy_from_slice(&17u64.to_le_bytes());
        let mut reader = MftReader::open(vol).unwrap();
        assert_eq!(reader.entry(16).unwrap().sizes.real_size, 123);
        assert!(reader.is_complete());
    }

    #[test]
    fn issue41_compression_unit_preserves_header_size_even_with_sparse_flag() {
        for with_named in [false, true] {
            let mut rec = blank_record(1, 1);
            let data = push_file_name(&mut rec, 64, ROOT_RECORD, "compressed");
            let mut end = push_nonresident_data(&mut rec, data, 65536, 60000);
            rec[data + 12..data + 14].copy_from_slice(&0x8000u16.to_le_bytes());
            rec[data + 34..data + 36].copy_from_slice(&4u16.to_le_bytes());
            rec[data + 64..data + 72].copy_from_slice(&4096u64.to_le_bytes());
            if with_named {
                end = named_data(&mut rec, end, "ads", 8192, 100);
            }
            set_used(&mut rec, end as u32);
            let mut reader = MftReader::open(volume(with_metadata_records(vec![rec], 5))).unwrap();
            assert_eq!(reader.entry(16).unwrap().sizes.allocated_size, 4096);
            assert!(reader.is_complete());
        }
    }

    #[test]
    fn issue41_overlapping_or_gapped_data_extents_require_fallback() {
        for low in [0u64, 3] {
            let (mut base, mut ext) = data_extension_fixture(false);
            let h = parse_record_header(&base).unwrap();
            let list = Attributes::new(&base, &h)
                .find(|a| a.type_code == attr_type::ATTRIBUTE_LIST)
                .unwrap();
            let pos = list.value_offset + 32 + 8;
            base[pos..pos + 8].copy_from_slice(&low.to_le_bytes());
            ext[80..88].copy_from_slice(&low.to_le_bytes());
            let mut reader =
                MftReader::open(volume(with_metadata_records(vec![base, ext], 5))).unwrap();
            assert!(reader.entry(16).is_none());
            assert!(!reader.is_complete());
        }
    }

    #[test]
    fn continuation_uses_the_initial_extents_allocation_mode() {
        // The initial extent has the whole-stream allocation field at 0x40.
        // A continuation omits that field and need not repeat CompressionUnit.
        let (mut base, ext) = data_extension_fixture(true);
        let h = parse_record_header(&base).unwrap();
        let data = Attributes::new(&base, &h)
            .find(|a| a.type_code == attr_type::DATA)
            .unwrap();
        let list = data.pos + data.total_length;
        let used = h.used_size as usize;
        base.copy_within(list..used, list + 8);
        base[data.pos + 4..data.pos + 8].copy_from_slice(&80u32.to_le_bytes());
        base[data.pos + 32..data.pos + 34].copy_from_slice(&72u16.to_le_bytes());
        base[data.pos + 34..data.pos + 36].copy_from_slice(&4u16.to_le_bytes());
        base[data.pos + 64..data.pos + 72].copy_from_slice(&(3 * CLUSTER as u64).to_le_bytes());
        base[data.pos + 72..data.pos + 78].copy_from_slice(&[0x11, 1, 40, 0x01, 1, 0]);
        set_used(&mut base, (used + 8) as u32);
        let mut reader =
            MftReader::open(volume(with_metadata_records(vec![base, ext], 5))).unwrap();
        let entry = reader
            .entry(16)
            .expect("continuation must not invalidate the stream");
        assert_eq!(entry.sizes.allocated_size, 3 * CLUSTER as u64);
        assert_eq!(entry.sizes.real_size, 16384);
        assert!(reader.is_complete());
    }

    #[test]
    fn zero_length_nonresident_sparse_stream_is_valid() {
        for with_named in [false, true] {
            let mut rec = blank_record(1, 1);
            let data = push_file_name(&mut rec, 64, ROOT_RECORD, "empty-sparse");
            let mut end = push_nonresident_data(&mut rec, data, 0, 0);
            rec[data + 12..data + 14].copy_from_slice(&0x8000u16.to_le_bytes());
            rec[data + 24..data + 32].copy_from_slice(&u64::MAX.to_le_bytes());
            rec[data + 32..data + 34].copy_from_slice(&64u16.to_le_bytes());
            rec[data + 64..data + 72].fill(0);
            if with_named {
                end = named_data(&mut rec, end, "ads", 4096, 123);
            }
            set_used(&mut rec, end as u32);
            let mut reader = MftReader::open(volume(with_metadata_records(vec![rec], 5))).unwrap();
            let entry = reader
                .entry(16)
                .expect("empty runlist and VCN -1 describe an empty stream");
            assert_eq!(entry.sizes.real_size, 0);
            assert_eq!(entry.sizes.allocated_size, 0);
            assert!(reader.is_complete());
        }
    }

    #[test]
    fn empty_vcn_marker_does_not_hide_allocations_or_continuations() {
        for bad in 0..4 {
            let mut rec = blank_record(1, 1);
            let data = push_file_name(&mut rec, 64, ROOT_RECORD, "bad-empty");
            let end = push_nonresident_data(&mut rec, data, 0, 0);
            rec[data + 12..data + 14].copy_from_slice(&0x8000u16.to_le_bytes());
            rec[data + 24..data + 32].copy_from_slice(&u64::MAX.to_le_bytes());
            rec[data + 32..data + 34].copy_from_slice(&64u16.to_le_bytes());
            match bad {
                0 => rec[data + 40..data + 48].copy_from_slice(&4096u64.to_le_bytes()),
                1 => rec[data + 48..data + 56].copy_from_slice(&1u64.to_le_bytes()),
                2 => rec[data + 16..data + 24].copy_from_slice(&1u64.to_le_bytes()),
                _ => rec[data + 64..data + 68].copy_from_slice(&[0x11, 1, 40, 0]),
            }
            set_used(&mut rec, end as u32);
            let mut reader = MftReader::open(volume(with_metadata_records(vec![rec], 5))).unwrap();
            assert!(reader.entry(16).is_none(), "invalid empty case {bad}");
            assert!(!reader.is_complete());
        }
    }
}
