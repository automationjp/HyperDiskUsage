//! Temporary read-only diagnosis. Does not return enumeration on MFT failure.
#![cfg(all(windows, target_env = "msvc"))]

pub use hyperdu_core::{Stat, StatMap};
#[path = "../src/platform/windows_impl/mft.rs"]
mod mft;
#[path = "../src/platform/windows_impl/mft_reader.rs"]
mod mft_reader;

#[test]
fn actual_mft_is_complete() {
    assert!(mft_reader::is_elevated(), "probe requires an elevated Windows runner");
    let mut volume = mft_reader::WindowsVolume::open('C').expect("open volume");
    let mut reader = mft_reader::MftReader::open(&mut volume).expect("open MFT");
    let sector_size = reader.geometry().bytes_per_sector;
    reader.source_mut().set_sector_size(sector_size);
    eprintln!("probe: records={} runs={}", reader.record_count(), reader.run_count());
    for number in 16..reader.record_count() {
        let _ = reader.entry(number);
        if reader.is_complete() { continue; }
        let rec = reader.read_record(number).expect("read rejected record");
        let header = mft::parse_record_header(&rec).expect("header");
        eprintln!("probe: first rejected record={number} header={header:?}");
        for a in mft::Attributes::new(&rec, &header) {
            if a.type_code != mft::attr_type::DATA && a.type_code != mft::attr_type::ATTRIBUTE_LIST { continue; }
            eprintln!("probe: attribute={a:?} name_units={}", rec[a.pos + 9]);
            if a.non_resident {
                for offset in [16usize, 24, 40, 48, 56, 64] {
                    if offset + 8 <= a.total_length {
                        let value = u64::from_le_bytes(rec[a.pos+offset..a.pos+offset+8].try_into().unwrap());
                        eprintln!("probe: field[{offset:#x}]={value:#x} ({value})");
                    }
                }
                let start = u16::from_le_bytes(rec[a.pos+32..a.pos+34].try_into().unwrap()) as usize;
                if start < a.total_length {
                    eprintln!("probe: runs={:?}", mft::parse_run_list(&rec[a.pos+start..a.pos+a.total_length]));
                }
            }
        }
        panic!("MFT rejected record {number}; a parity comparison must not silently enumerate");
    }
}
