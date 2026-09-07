//! Ordinary-file DATA streams. Whole-stream size fields are valid only in the
//! lowest-VCN-zero extent; sparse allocation is the sum of all mapped extents.
//! Attribute lists are untrusted input: bound I/O and validate record ownership
//! and sequence before allowing the caller to present a complete volume total.
use std::collections::{HashMap, HashSet};

use super::{
    super::mft::{
        attr_flags, attr_type, parse_data_sizes, parse_record_header, parse_run_list, AttrHeader,
        Attributes, DataSizes, RecordHeader, Run,
    },
    MftReader, VolumeSource,
};

const RECORD_MASK: u64 = (1 << 48) - 1;
const MAX_LIST_BYTES: u64 = 1024 * 1024;
const MAX_EXTENSIONS: usize = 4096;

#[derive(Default)]
pub(super) struct StreamSizes {
    pub(super) unnamed: Option<DataSizes>,
    pub(super) flags: u16,
    pub(super) named_bytes: u64,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct Reference {
    name: Vec<u8>,
    low: u64,
    record: u64,
    instance: u16,
}

struct Extent {
    name: Vec<u8>,
    low: u64,
    high: u64,
    non_resident: bool,
    flags: u16,
    sizes: Option<DataSizes>,
    sparse_bytes: u64,
    sparse_uncompressed: bool,
}

fn u16_at(b: &[u8], pos: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(pos..pos.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn u64_at(b: &[u8], pos: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(pos..pos.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn name_in(bytes: &[u8], count: usize, offset: usize, minimum: usize) -> Option<Vec<u8>> {
    if count == 0 {
        return Some(Vec::new());
    }
    if offset < minimum || offset % 2 != 0 {
        return None;
    }
    Some(
        bytes
            .get(offset..offset.checked_add(count.checked_mul(2)?)?)?
            .to_vec(),
    )
}

fn attribute_name(rec: &[u8], attr: &AttrHeader) -> Option<Vec<u8>> {
    let bytes = rec.get(attr.pos..attr.pos.checked_add(attr.total_length)?)?;
    name_in(
        bytes,
        *bytes.get(9)? as usize,
        u16_at(bytes, 10)? as usize,
        if attr.non_resident { 64 } else { 24 },
    )
}

fn lowest(rec: &[u8], attr: &AttrHeader) -> Option<u64> {
    if attr.non_resident {
        u64_at(rec, attr.pos + 16)
    } else {
        Some(0)
    }
}

/// Unlike the bootstrap parser, required DATA runs must end inside their own
/// attribute. A terminator in the next attribute must not validate this one.
fn strict_runs(rec: &[u8], attr: &AttrHeader) -> Option<Vec<Run>> {
    let relative = u16_at(rec, attr.pos + 32)? as usize;
    if relative < 64 || relative >= attr.total_length {
        return None;
    }
    let bytes =
        rec.get(attr.pos.checked_add(relative)?..attr.pos.checked_add(attr.total_length)?)?;
    let mut pos = 0;
    loop {
        let header = *bytes.get(pos)?;
        if header == 0 {
            break;
        }
        let len = (header & 15) as usize;
        let off = (header >> 4) as usize;
        if len == 0 || len > 8 || off > 8 {
            return None;
        }
        pos = pos.checked_add(1 + len + off)?;
    }
    let runs = parse_run_list(&bytes[..=pos])?;
    if runs.iter().any(|run| run.length == 0) {
        return None;
    }
    Some(runs)
}

fn extent(rec: &[u8], attr: &AttrHeader, cluster: u32) -> Option<Extent> {
    // Slice to the attribute end so even fixed-size header reads stay bounded.
    let rec = rec.get(..attr.pos.checked_add(attr.total_length)?)?;
    let low = lowest(rec, attr)?;
    let high = if attr.non_resident {
        u64_at(rec, attr.pos + 24)?
    } else {
        0
    };
    let sizes = if low == 0 {
        Some(parse_data_sizes(rec, attr, cluster)?)
    } else {
        None
    };
    // CompressionUnit, not the COMPRESSED flag alone, decides whether the
    // header carries physical compressed size (the #39 accounting contract).
    let sparse_uncompressed = attr.non_resident
        && attr.flags & attr_flags::SPARSE != 0
        && u16_at(rec, attr.pos + 34)? == 0;
    let sparse_bytes = if sparse_uncompressed {
        let runs = strict_runs(rec, attr)?;
        let covered = runs
            .iter()
            .try_fold(0u64, |sum, run| sum.checked_add(run.length))?;
        if covered != high.checked_sub(low)?.checked_add(1)? {
            return None;
        }
        runs.iter()
            .filter(|run| run.lcn.is_some())
            .try_fold(0u64, |sum, run| sum.checked_add(run.length))?
            .checked_mul(cluster as u64)?
    } else {
        0
    };
    Some(Extent {
        name: attribute_name(rec, attr)?,
        low,
        high,
        non_resident: attr.non_resident,
        flags: attr.flags,
        sizes,
        sparse_bytes,
        sparse_uncompressed,
    })
}

fn references(bytes: &[u8]) -> Option<Vec<Reference>> {
    let mut result = Vec::new();
    let mut pos: usize = 0;
    while pos < bytes.len() {
        let remaining = bytes.get(pos..)?;
        if remaining.first() == Some(&0) && remaining.iter().all(|b| *b == 0) {
            break;
        }
        let length = u16_at(remaining, 4)? as usize;
        if length < 26 {
            return None;
        }
        let entry = remaining.get(..length)?;
        let kind = u32::from_le_bytes(entry.get(..4)?.try_into().ok()?);
        let name = name_in(entry, *entry.get(6)? as usize, *entry.get(7)? as usize, 26)?;
        if kind == attr_type::DATA {
            result.push(Reference {
                name,
                low: u64_at(entry, 8)?,
                record: u64_at(entry, 16)?,
                instance: u16_at(entry, 24)?,
            });
            if result.len() > MAX_EXTENSIONS {
                return None;
            }
        }
        pos = pos.checked_add(length)?;
    }
    Some(result)
}

impl<S: VolumeSource> MftReader<S> {
    /// Read one resident or bounded, non-resident attribute list. A fragmented
    /// list requiring its own extension is unsupported and triggers fallback.
    fn list_value(&mut self, rec: &[u8], attr: &AttrHeader) -> Option<Vec<u8>> {
        if !attr.non_resident {
            return Some(
                rec.get(attr.value_offset..attr.value_offset.checked_add(attr.value_length)?)?
                    .to_vec(),
            );
        }
        if lowest(rec, attr)? != 0 || attr.flags != 0 {
            return None;
        }
        let length = u64_at(rec, attr.pos + 48)?;
        if length == 0 || length > MAX_LIST_BYTES {
            return None;
        }
        let runs = strict_runs(rec, attr)?;
        let mut result = vec![0; length as usize];
        let mut filled = 0usize;
        for run in runs {
            if filled == result.len() {
                break;
            }
            let start = run.lcn?.checked_mul(self.geometry.cluster_size() as u64)?;
            let bytes = run
                .length
                .checked_mul(self.geometry.cluster_size() as u64)?;
            let take = bytes.min((result.len() - filled) as u64) as usize;
            if !self
                .source
                .read_at(start, &mut result[filled..filled + take])
            {
                return None;
            }
            filled += take;
        }
        if filled != result.len() {
            return None;
        }
        Some(result)
    }

    pub(super) fn data_streams(
        &mut self,
        number: u64,
        rec: &[u8],
        header: &RecordHeader,
    ) -> Option<StreamSizes> {
        // Directory index allocation is not part of the scanner's file totals.
        if header.is_directory {
            return Some(StreamSizes::default());
        }
        let cluster = self.geometry.cluster_size();
        let mut extents = Vec::new();
        let mut refs = Vec::new();
        let mut has_list = false;
        for attr in Attributes::new(rec, header) {
            match attr.type_code {
                attr_type::DATA => extents.push(extent(rec, &attr, cluster)?),
                attr_type::ATTRIBUTE_LIST => {
                    if has_list {
                        return None;
                    }
                    has_list = true;
                    refs = references(&self.list_value(rec, &attr)?)?;
                }
                _ => {}
            }
        }
        // Almost every file has one base-record stream. Avoid building a hash
        // table or allocating an extension cache on that hot path.
        if !has_list && extents.len() <= 1 {
            let Some(ext) = extents.pop() else {
                return Some(StreamSizes::default());
            };
            let mut sizes = ext.sizes?;
            if ext.low != 0 {
                return None;
            }
            if ext.sparse_uncompressed {
                sizes.allocated_size = ext.sparse_bytes;
            }
            return Some(if ext.name.is_empty() {
                StreamSizes {
                    unnamed: Some(sizes),
                    flags: ext.flags,
                    named_bytes: 0,
                }
            } else {
                StreamSizes {
                    named_bytes: sizes.allocated_size,
                    ..Default::default()
                }
            });
        }
        let base_reference = number | ((u16_at(rec, 16)? as u64) << 48);
        let mut records = HashMap::new();
        let mut seen = HashSet::new();
        for reference in refs {
            if !seen.insert(reference.clone()) {
                continue;
            }
            let record = reference.record & RECORD_MASK;
            let extension = record != number;
            let (bytes, record_header) = if extension {
                if let std::collections::hash_map::Entry::Vacant(slot) = records.entry(record) {
                    slot.insert(self.read_record(record)?);
                }
                let bytes = records.get(&record)?;
                let h = parse_record_header(bytes)?;
                if !h.in_use || u64_at(bytes, 32)? != base_reference {
                    return None;
                }
                (bytes.as_slice(), h)
            } else {
                (rec, *header)
            };
            if u16_at(bytes, 16)? != (reference.record >> 48) as u16 {
                return None;
            }
            let mut matched = None;
            for attr in
                Attributes::new(bytes, &record_header).filter(|a| a.type_code == attr_type::DATA)
            {
                if u16_at(bytes, attr.pos + 14)? == reference.instance
                    && lowest(bytes, &attr)? == reference.low
                    && attribute_name(bytes, &attr)? == reference.name
                {
                    if matched.is_some() {
                        return None;
                    }
                    matched = Some(attr);
                }
            }
            let attr = matched?;
            if extension {
                extents.push(extent(bytes, &attr, cluster)?);
            }
        }
        let mut groups: HashMap<Vec<u8>, Vec<Extent>> = HashMap::new();
        for ext in extents {
            groups.entry(ext.name.clone()).or_default().push(ext);
        }
        let mut result = StreamSizes::default();
        for (name, mut extents) in groups {
            extents.sort_unstable_by_key(|e| e.low);
            let first = extents.first()?;
            let mut sizes = first.sizes?;
            let flags = first.flags;
            let non_resident = first.non_resident;
            let sparse = first.sparse_uncompressed;
            let mut end = 0u64;
            let mut sparse_bytes = 0u64;
            for (n, ext) in extents.iter().enumerate() {
                if ext.low != end
                    || ext.flags != flags
                    || ext.non_resident != non_resident
                    || ext.sparse_uncompressed != sparse
                {
                    return None;
                }
                if n > 0 && !non_resident {
                    return None;
                }
                // Plain single-extent headers can describe an empty stream.
                if has_list || extents.len() > 1 || sparse {
                    end = ext.high.checked_add(1)?;
                    if end <= ext.low {
                        return None;
                    }
                } else {
                    end = 1;
                }
                sparse_bytes = sparse_bytes.checked_add(ext.sparse_bytes)?;
            }
            if sparse && non_resident {
                sizes.allocated_size = sparse_bytes;
            }
            if has_list && non_resident {
                let required = sizes.real_size / cluster as u64
                    + u64::from(sizes.real_size % cluster as u64 != 0);
                if end < required {
                    return None;
                }
            }
            if name.is_empty() {
                result.unnamed = Some(sizes);
                result.flags = flags;
            } else {
                result.named_bytes = result.named_bytes.checked_add(sizes.allocated_size)?;
            }
        }
        Some(result)
    }
}
