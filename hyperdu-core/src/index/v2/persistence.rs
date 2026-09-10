use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

use super::*;
use crate::index::journal::{JournalCursor, JournalKind};

const MAGIC: &[u8; 8] = b"HDUIDX02";
const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 2_000_000;
const MAX_NAME_BYTES: usize = 65536;

impl PersistentIndex {
    /// Save an atomic snapshot. The checksum detects accidental corruption;
    /// it does not authenticate data from another user.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if !self.valid {
            return Err(invalid("failed index update requires a rebuild"));
        }
        if self
            .dirs
            .len()
            .checked_add(self.files.len())
            .map_or(true, |count| count > MAX_ENTRIES)
        {
            return Err(invalid("too many snapshot objects"));
        }
        let mut writer = Writer(Vec::new());
        writer.bytes(MAGIC)?;
        writer.bytes(&[platform(), 1, 0, 0])?; // native names, metric policy, reserved
        writer.id(self.root)?;
        match self.cursor {
            Some(cursor) => {
                let kind = match cursor.kind {
                    JournalKind::Usn => 1,
                    JournalKind::Fsevents => 2,
                    JournalKind::Fanotify => 3,
                    JournalKind::Inotify => 4,
                };
                writer.bytes(&[kind])?;
                writer.u64(cursor.volume)?;
                writer.bytes(&cursor.epoch.to_le_bytes())?;
                writer.u64(cursor.position)?;
            }
            None => writer.bytes(&[0])?,
        }
        let total = self.dirs[&self.root].total;
        for number in [
            total.logical,
            total.physical,
            total.files,
            self.dirs.len() as u64 - 1,
            self.files.len() as u64,
        ] {
            writer.u64(number)?;
        }
        // Parent-before-child order makes decoding linear and rejects cycles
        // without repeatedly searching unresolved nodes.
        let mut pending = vec![self.root];
        while let Some(parent) = pending.pop() {
            for id in self.dirs[&parent].children.values() {
                if let Some(dir) = self.dirs.get(id) {
                    let link = dir.link.as_ref().ok_or_else(|| invalid("nested root"))?;
                    writer.id(*id)?;
                    writer.id(link.parent)?;
                    writer.name(&link.name)?;
                    pending.push(*id);
                }
            }
        }
        let mut links = 0_usize;
        for (id, file) in &self.files {
            links = links
                .checked_add(file.links.len())
                .ok_or_else(|| invalid("too many links"))?;
            if links > MAX_ENTRIES || file.links.is_empty() {
                return Err(invalid("invalid snapshot link count"));
            }
            writer.id(*id)?;
            writer.u64(file.logical)?;
            writer.u64(file.physical)?;
            writer.u64(file.links.len() as u64)?;
            for link in &file.links {
                writer.id(link.parent)?;
                writer.name(&link.name)?;
            }
        }
        let checksum = checksum(&writer.0);
        writer.u64(checksum)?;
        super::super::atomic_save::write(path, &writer.0)
    }

    /// Decode bounded native names and recompute every aggregate. Persisted
    /// freshness is never trusted, even when a replay cursor is available.
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let length = usize::try_from(file.metadata()?.len())
            .map_err(|_| invalid("snapshot is too large"))?;
        if !(20..=MAX_BYTES).contains(&length) {
            return Err(invalid("invalid snapshot size"));
        }
        let mut bytes = Vec::with_capacity(length);
        file.by_ref()
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() != length {
            return Err(invalid("snapshot changed while reading"));
        }
        let (payload, trailer) = bytes.split_at(length - 8);
        if checksum(payload) != u64::from_le_bytes(trailer.try_into().unwrap()) {
            return Err(invalid("snapshot checksum mismatch"));
        }
        let mut reader = Reader(payload);
        if reader.take(8)? != MAGIC || reader.take(4)? != [platform(), 1, 0, 0] {
            return Err(invalid("unsupported index format, platform or metric"));
        }
        let root = reader.id()?;
        let cursor = match reader.take(1)?[0] {
            0 => None,
            kind @ 1..=4 => {
                let kind = match kind {
                    1 => JournalKind::Usn,
                    2 => JournalKind::Fsevents,
                    3 => JournalKind::Fanotify,
                    _ => JournalKind::Inotify,
                };
                let volume = reader.u64()?;
                let epoch = u128::from_le_bytes(reader.take(16)?.try_into().unwrap());
                let position = reader.u64()?;
                if volume != root.volume {
                    return Err(invalid("cursor volume mismatch"));
                }
                Some(JournalCursor {
                    kind,
                    volume,
                    epoch,
                    position,
                })
            }
            _ => return Err(invalid("unsupported journal cursor")),
        };
        let expected = Totals {
            logical: reader.u64()?,
            physical: reader.u64()?,
            files: reader.u64()?,
        };
        let dirs = reader.count()?;
        let files = reader.count()?;
        if dirs
            .checked_add(files)
            .map_or(true, |count| count > MAX_ENTRIES)
        {
            return Err(invalid("too many snapshot objects"));
        }
        let mut index = Self::new(root);
        for _ in 0..dirs {
            let id = reader.id()?;
            let parent = reader.id()?;
            let name = reader.name()?;
            if index.dirs.contains_key(&id) || index.lookup(parent, &name).is_some() {
                return Err(invalid("duplicate directory identity or name"));
            }
            index.upsert(
                LinkKey { parent, name },
                ObservedEntry {
                    id,
                    kind: EntryKind::Directory,
                    logical: 0,
                    physical: 0,
                },
            )?;
        }
        let mut total_links = 0_usize;
        for _ in 0..files {
            let id = reader.id()?;
            let logical = reader.u64()?;
            let physical = reader.u64()?;
            let links = reader.count()?;
            total_links = total_links
                .checked_add(links)
                .ok_or_else(|| invalid("too many links"))?;
            if links == 0
                || total_links > MAX_ENTRIES
                || index.files.contains_key(&id)
                || index.dirs.contains_key(&id)
            {
                return Err(invalid("duplicate file identity or invalid link count"));
            }
            for _ in 0..links {
                let parent = reader.id()?;
                let name = reader.name()?;
                if index.lookup(parent, &name).is_some() {
                    return Err(invalid("duplicate entry name"));
                }
                index.upsert(
                    LinkKey { parent, name },
                    ObservedEntry {
                        id,
                        kind: EntryKind::File,
                        logical,
                        physical,
                    },
                )?;
            }
        }
        if !reader.0.is_empty() || index.dirs[&root].total != expected {
            return Err(invalid("snapshot totals or trailing data invalid"));
        }
        index.cursor = cursor;
        index.freshness = Freshness::Stale;
        Ok(index)
    }
}

struct Writer(Vec<u8>);
impl Writer {
    fn bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .map_or(true, |size| size > MAX_BYTES)
        {
            return Err(invalid("snapshot is too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn u64(&mut self, value: u64) -> io::Result<()> {
        self.bytes(&value.to_le_bytes())
    }
    fn id(&mut self, id: EntryId) -> io::Result<()> {
        self.u64(id.volume)?;
        self.bytes(&id.object.to_le_bytes())
    }
    fn name(&mut self, name: &OsStr) -> io::Result<()> {
        validate_name(name)?;
        let bytes = encode_name(name);
        if bytes.len() > MAX_NAME_BYTES {
            return Err(invalid("snapshot name is too long"));
        }
        self.u64(bytes.len() as u64)?;
        self.bytes(&bytes)
    }
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        if length > self.0.len() {
            return Err(invalid("truncated snapshot"));
        }
        let (head, rest) = self.0.split_at(length);
        self.0 = rest;
        Ok(head)
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn id(&mut self) -> io::Result<EntryId> {
        Ok(EntryId {
            volume: self.u64()?,
            object: u128::from_le_bytes(self.take(16)?.try_into().unwrap()),
        })
    }
    fn count(&mut self) -> io::Result<usize> {
        let count = usize::try_from(self.u64()?).map_err(|_| invalid("invalid count"))?;
        if count > MAX_ENTRIES {
            return Err(invalid("snapshot count exceeds limit"));
        }
        Ok(count)
    }
    fn name(&mut self) -> io::Result<OsString> {
        let length = self.count()?;
        if length > MAX_NAME_BYTES {
            return Err(invalid("snapshot name exceeds limit"));
        }
        let name = decode_name(self.take(length)?)?;
        validate_name(&name)?;
        Ok(name)
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325_u64, |value, byte| {
        (value ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(unix)]
fn platform() -> u8 {
    1
}
#[cfg(windows)]
fn platform() -> u8 {
    2
}
#[cfg(not(any(unix, windows)))]
fn platform() -> u8 {
    3
}

#[cfg(unix)]
fn encode_name(name: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes().to_vec()
}
#[cfg(windows)]
fn encode_name(name: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    name.encode_wide().flat_map(u16::to_le_bytes).collect()
}
#[cfg(not(any(unix, windows)))]
fn encode_name(name: &OsStr) -> Vec<u8> {
    name.to_string_lossy().as_bytes().to_vec()
}

#[cfg(unix)]
fn decode_name(bytes: &[u8]) -> io::Result<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes.to_vec()))
}
#[cfg(windows)]
fn decode_name(bytes: &[u8]) -> io::Result<OsString> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.len() % 2 != 0 {
        return Err(invalid("odd Windows filename length"));
    }
    let words: Vec<_> = bytes
        .chunks_exact(2)
        .map(|word| u16::from_le_bytes([word[0], word[1]]))
        .collect();
    Ok(OsString::from_wide(&words))
}
#[cfg(not(any(unix, windows)))]
fn decode_name(bytes: &[u8]) -> io::Result<OsString> {
    Ok(std::str::from_utf8(bytes)
        .map_err(|_| invalid("invalid filename encoding"))?
        .into())
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn root() -> EntryId {
        EntryId {
            volume: 1,
            object: 1,
        }
    }
    fn header(dirs: u64, files: u64, total: Totals) -> Writer {
        let mut writer = Writer(Vec::new());
        writer.bytes(MAGIC).unwrap();
        writer.bytes(&[platform(), 1, 0, 0]).unwrap();
        writer.id(root()).unwrap();
        writer.bytes(&[0]).unwrap();
        for value in [total.logical, total.physical, total.files, dirs, files] {
            writer.u64(value).unwrap();
        }
        writer
    }
    fn dir(writer: &mut Writer, id: u128, parent: u128, name: &str) {
        writer
            .id(EntryId {
                volume: 1,
                object: id,
            })
            .unwrap();
        writer
            .id(EntryId {
                volume: 1,
                object: parent,
            })
            .unwrap();
        writer.name(OsStr::new(name)).unwrap();
    }
    fn dir_with_ids(writer: &mut Writer, id: EntryId, parent: EntryId, name: &str) {
        writer.id(id).unwrap();
        writer.id(parent).unwrap();
        writer.name(OsStr::new(name)).unwrap();
    }
    fn rejected(mut writer: Writer) {
        writer.u64(checksum(&writer.0)).unwrap();
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), writer.0).unwrap();
        assert!(PersistentIndex::load(temp.path()).is_err());
    }
    #[test]
    fn valid_checksum_does_not_hide_bad_graph_counts_or_totals() {
        let mut cycle = header(1, 0, Totals::default());
        dir(&mut cycle, 2, 2, "self");
        rejected(cycle);
        let mut duplicate = header(2, 0, Totals::default());
        dir(&mut duplicate, 2, 1, "a");
        dir(&mut duplicate, 2, 1, "b");
        rejected(duplicate);
        let mut duplicate_name = header(2, 0, Totals::default());
        dir(&mut duplicate_name, 2, 1, "same");
        dir(&mut duplicate_name, 3, 1, "same");
        rejected(duplicate_name);
        rejected(header(MAX_ENTRIES as u64 + 1, 0, Totals::default()));
        rejected(header(0, 1, Totals::default()));
        rejected(header(
            0,
            0,
            Totals {
                files: 1,
                ..Totals::default()
            },
        ));
        let mut extra = header(0, 0, Totals::default());
        extra.bytes(b"trailing payload").unwrap();
        rejected(extra);
        let mut unlinked_file = header(0, 1, Totals::default());
        unlinked_file
            .id(EntryId {
                volume: 1,
                object: 2,
            })
            .unwrap();
        for value in [1, 1, 0] {
            unlinked_file.u64(value).unwrap();
        }
        rejected(unlinked_file);
    }

    #[test]
    fn valid_checksum_rejects_foreign_volume_objects() {
        let mut foreign_directory = header(1, 0, Totals::default());
        dir_with_ids(
            &mut foreign_directory,
            EntryId {
                volume: 2,
                object: 2,
            },
            root(),
            "foreign-directory",
        );
        rejected(foreign_directory);

        let mut foreign_file = header(
            0,
            1,
            Totals {
                logical: 1,
                physical: 2,
                files: 1,
            },
        );
        foreign_file
            .id(EntryId {
                volume: 2,
                object: 3,
            })
            .unwrap();
        foreign_file.u64(1).unwrap();
        foreign_file.u64(2).unwrap();
        foreign_file.u64(1).unwrap();
        foreign_file.id(root()).unwrap();
        foreign_file.name(OsStr::new("foreign-file")).unwrap();
        rejected(foreign_file);
    }
    #[test]
    fn oversized_sparse_snapshot_is_rejected_before_payload_allocation() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        temp.as_file().set_len(MAX_BYTES as u64 + 1).unwrap();
        assert!(PersistentIndex::load(temp.path()).is_err());
    }
}
