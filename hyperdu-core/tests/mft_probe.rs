//! Temporary read-only diagnosis. Does not return enumeration on MFT failure.
#![cfg(all(windows, target_env = "msvc"))]

pub use hyperdu_core::{Stat, StatMap};
#[path = "../src/platform/windows_impl/mft.rs"]
mod mft;
#[path = "../src/platform/windows_impl/mft_reader.rs"]
mod mft_reader;

fn u64_at(b: &[u8], n: usize) -> u64 {
    u64::from_le_bytes(b[n..n + 8].try_into().unwrap())
}

fn dump(rec: &[u8]) {
    let header = mft::parse_record_header(rec).expect("header");
    eprintln!(
        "probe: header={header:?} sequence={} base={:#x}",
        u16::from_le_bytes(rec[16..18].try_into().unwrap()),
        u64_at(rec, 32)
    );
    for a in mft::Attributes::new(rec, &header) {
        if a.type_code != mft::attr_type::DATA && a.type_code != mft::attr_type::ATTRIBUTE_LIST {
            continue;
        }
        eprintln!(
            "probe: attribute={a:?} name_units={} instance={}",
            rec[a.pos + 9],
            u16::from_le_bytes(rec[a.pos + 14..a.pos + 16].try_into().unwrap())
        );
        if a.non_resident {
            for offset in [16usize, 24, 40, 48, 56, 64] {
                if offset + 8 <= a.total_length {
                    let value = u64_at(rec, a.pos + offset);
                    eprintln!("probe: field[{offset:#x}]={value:#x} ({value})");
                }
            }
            let start =
                u16::from_le_bytes(rec[a.pos + 32..a.pos + 34].try_into().unwrap()) as usize;
            if start < a.total_length {
                eprintln!(
                    "probe: runs={:?}",
                    mft::parse_run_list(&rec[a.pos + start..a.pos + a.total_length])
                );
            }
        }
    }
}

#[test]
#[ignore = "read-only investigation of a live system volume"]
fn actual_mft_is_complete() {
    assert!(
        mft_reader::is_elevated(),
        "probe requires an elevated Windows runner"
    );
    let mut volume = mft_reader::WindowsVolume::open('C').expect("open volume");
    let mut reader = mft_reader::MftReader::open(&mut volume).expect("open MFT");
    let sector_size = reader.geometry().bytes_per_sector;
    reader.source_mut().set_sector_size(sector_size);
    eprintln!(
        "probe: records={} runs={}",
        reader.record_count(),
        reader.run_count()
    );
    for number in 16..reader.record_count() {
        let _ = reader.entry(number);
        if reader.is_complete() {
            continue;
        }
        let rec = reader.read_record(number).expect("read rejected record");
        eprintln!("probe: first rejected record={number}");
        dump(&rec);
        let header = mft::parse_record_header(&rec).unwrap();
        for a in mft::Attributes::new(&rec, &header) {
            if a.type_code != mft::attr_type::ATTRIBUTE_LIST || a.non_resident {
                continue;
            }
            let value = &rec[a.value_offset..a.value_offset + a.value_length];
            let mut p = 0;
            while p + 26 <= value.len() {
                let kind = u32::from_le_bytes(value[p..p + 4].try_into().unwrap());
                let len = u16::from_le_bytes(value[p + 4..p + 6].try_into().unwrap()) as usize;
                if len < 26 || p + len > value.len() {
                    break;
                }
                let reference = u64_at(value, p + 16);
                let instance = u16::from_le_bytes(value[p + 24..p + 26].try_into().unwrap());
                eprintln!("probe: reference kind={kind:#x} full_ref={reference:#x} low={} instance={instance}", u64_at(value, p+8));
                if kind == mft::attr_type::DATA && reference & ((1 << 48) - 1) != number {
                    if let Some(ext) = reader.read_record(reference & ((1 << 48) - 1)) {
                        dump(&ext);
                    } else {
                        eprintln!("probe: extension unreadable");
                    }
                }
                p += len;
            }
        }
        panic!("MFT rejected record {number}; a parity comparison must not silently enumerate");
    }
}
