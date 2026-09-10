//! Runtime-selected name operations. The default build keeps Rust 1.75 support;
//! `simd-avx512` is optional and requires Rust 1.89 for stable AVX-512 intrinsics.
//! HYPERDU_SIMD=scalar|auto is read once, before the first name operation.
use std::{ffi::OsStr, sync::OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Scalar,
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    Avx2,
    #[cfg(all(
        feature = "simd-avx512",
        any(target_arch = "x86", target_arch = "x86_64")
    ))]
    Avx512,
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    Neon,
}

impl Backend {
    fn available(self) -> bool {
        match self {
            Self::Scalar => true,
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Self::Avx2 => std::is_x86_feature_detected!("avx2"),
            #[cfg(all(
                feature = "simd-avx512",
                any(target_arch = "x86", target_arch = "x86_64")
            ))]
            Self::Avx512 => {
                std::is_x86_feature_detected!("avx2")
                    && std::is_x86_feature_detected!("avx512f")
                    && std::is_x86_feature_detected!("avx512bw")
            }
            #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
            Self::Neon => std::arch::is_aarch64_feature_detected!("neon"),
        }
    }

    fn detect() -> Self {
        #[cfg(all(
            feature = "simd-avx512",
            any(target_arch = "x86", target_arch = "x86_64")
        ))]
        if Self::Avx512.available() {
            return Self::Avx512;
        }
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if Self::Avx2.available() {
            return Self::Avx2;
        }
        #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
        if Self::Neon.available() {
            return Self::Neon;
        }
        Self::Scalar
    }
}

fn configured(value: Option<&OsStr>) -> Result<Backend, ()> {
    match value {
        None => Ok(Backend::detect()),
        Some(value) if value == "auto" => Ok(Backend::detect()),
        Some(value) if value == "scalar" => Ok(Backend::Scalar),
        Some(_) => Err(()),
    }
}

fn selected() -> Backend {
    static BACKEND: OnceLock<Backend> = OnceLock::new();
    *BACKEND.get_or_init(|| {
        let backend =
            configured(std::env::var_os("HYPERDU_SIMD").as_deref()).unwrap_or_else(|()| {
                eprintln!("hyperdu: invalid HYPERDU_SIMD (expected scalar or auto); using scalar");
                Backend::Scalar
            });
        debug_assert!(backend.available());
        backend
    })
}

/// Empty exclusion patterns never match. Keep memchr's existing runtime SIMD
/// search in auto mode; do not introduce a second byte-search implementation.
#[cfg(any(not(windows), test))]
pub(crate) fn contains_bytes(name: &[u8], pattern: &[u8]) -> bool {
    if pattern.is_empty() || pattern.len() > name.len() {
        return false;
    }
    if selected() == Backend::Scalar {
        name.windows(pattern.len()).any(|window| window == pattern)
    } else {
        memchr::memmem::find(name, pattern).is_some()
    }
}

#[cfg(any(windows, test))]
fn wide_scalar(name: &[u16], pattern: &[u16]) -> bool {
    !pattern.is_empty()
        && pattern.len() <= name.len()
        && name.windows(pattern.len()).any(|window| window == pattern)
}

/// Case-sensitive code-unit comparison, including unpaired surrogates.
#[cfg(any(windows, test))]
pub(crate) fn contains_wide(name: &[u16], pattern: &[u16]) -> bool {
    wide_with(selected(), name, pattern)
}

#[cfg(any(windows, test))]
fn wide_with(backend: Backend, name: &[u16], pattern: &[u16]) -> bool {
    if pattern.is_empty() || pattern.len() > name.len() {
        return false;
    }
    // Backend values reach this private dispatch only through runtime detection
    // (tests likewise enumerate available ISAs). Length guards apply to every ISA.
    match backend {
        Backend::Scalar => wide_scalar(name, pattern),
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        Backend::Avx2 => unsafe { x86::wide_avx2(name, pattern) },
        #[cfg(all(
            feature = "simd-avx512",
            any(target_arch = "x86", target_arch = "x86_64")
        ))]
        Backend::Avx512 => unsafe { x86::avx512::wide(name, pattern) },
        #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
        Backend::Neon => unsafe { neon::wide(name, pattern) },
    }
}

/// Decode checked UTF-16LE bytes, keeping the existing lossy surrogate behavior.
/// ASCII names allocate only the final byte buffer, avoiding a temporary Vec<u16>.
#[cfg(any(windows, test))]
pub(crate) fn decode_utf16le_lossy(bytes: &[u8]) -> Option<String> {
    decode_with(selected(), bytes)
}

#[cfg(any(windows, test))]
fn decode_with(backend: Backend, bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 != 0 {
        return None;
    }
    let mut ascii = vec![0; bytes.len() / 2];
    // Runtime selection guarantees the target features. Kernels write only
    // complete ASCII blocks within ascii and return their initialized prefix.
    let mut done = match backend {
        Backend::Scalar => 0,
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        Backend::Avx2 => unsafe { x86::ascii_avx2(bytes, &mut ascii) },
        #[cfg(all(
            feature = "simd-avx512",
            any(target_arch = "x86", target_arch = "x86_64")
        ))]
        Backend::Avx512 => unsafe { x86::avx512::ascii(bytes, &mut ascii) },
        #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
        Backend::Neon => unsafe { neon::ascii(bytes, &mut ascii) },
    };
    while done < ascii.len() {
        let at = done * 2;
        if bytes[at] > 0x7f || bytes[at + 1] != 0 {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            return Some(String::from_utf16_lossy(&units));
        }
        ascii[done] = bytes[at];
        done += 1;
    }
    // Every emitted byte has been checked to be ASCII by its kernel or above.
    Some(unsafe { String::from_utf8_unchecked(ascii) })
}

#[cfg(all(any(windows, test), any(target_arch = "x86", target_arch = "x86_64")))]
mod x86 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn ascii_avx2(bytes: &[u8], out: &mut [u8]) -> usize {
        let mut done = 0;
        let high_bits = _mm256_set1_epi16(!0x7f);
        // 16 UTF-16 units require 32 readable input bytes and 16 output bytes.
        while done + 16 <= out.len() {
            let value = _mm256_loadu_si256(bytes.as_ptr().add(done * 2).cast());
            if _mm256_testz_si256(value, high_bits) == 0 {
                break;
            }
            let low = _mm256_castsi256_si128(value);
            let high = _mm256_extracti128_si256::<1>(value);
            _mm_storeu_si128(
                out.as_mut_ptr().add(done).cast(),
                _mm_packus_epi16(low, high),
            );
            done += 16;
        }
        done
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn wide_avx2(name: &[u16], pattern: &[u16]) -> bool {
        let candidates = name.len() - pattern.len() + 1;
        let first = _mm256_set1_epi16(pattern[0] as i16);
        let last = _mm256_set1_epi16(pattern[pattern.len() - 1] as i16);
        let mut at = 0;
        // Both loads fit: even the last lane's candidate has a whole pattern.
        while at + 16 <= candidates {
            let starts = _mm256_loadu_si256(name.as_ptr().add(at).cast());
            let ends = _mm256_loadu_si256(name.as_ptr().add(at + pattern.len() - 1).cast());
            let equal = _mm256_and_si256(
                _mm256_cmpeq_epi16(starts, first),
                _mm256_cmpeq_epi16(ends, last),
            );
            let mut mask = _mm256_movemask_epi8(equal) as u32;
            while mask != 0 {
                let lane = mask.trailing_zeros() as usize / 2;
                let start = at + lane;
                if &name[start..start + pattern.len()] == pattern {
                    return true;
                }
                mask &= !(3 << (lane * 2));
            }
            at += 16;
        }
        super::wide_scalar(&name[at..], pattern)
    }

    // Stable AVX-512 is deliberately isolated from the default Rust 1.75 build.
    #[cfg(feature = "simd-avx512")]
    #[clippy::msrv = "1.89.0"]
    pub(super) mod avx512 {
        use super::*;

        #[target_feature(enable = "avx2,avx512f,avx512bw")]
        pub(in super::super) unsafe fn ascii(bytes: &[u8], out: &mut [u8]) -> usize {
            let mut done = 0;
            let high_bits = _mm512_set1_epi16(!0x7f);
            let zero = _mm512_setzero_si512();
            while done + 32 <= out.len() {
                let value = _mm512_loadu_si512(bytes.as_ptr().add(done * 2).cast());
                if _mm512_cmpeq_epi16_mask(_mm512_and_si512(value, high_bits), zero) != u32::MAX {
                    break;
                }
                let low = _mm512_castsi512_si256(value);
                let high = _mm512_extracti64x4_epi64::<1>(value);
                let packed = _mm256_packus_epi16(low, high);
                // packus works within 128-bit lanes: restore the input order.
                let ordered = _mm256_permute4x64_epi64::<0xd8>(packed);
                _mm256_storeu_si256(out.as_mut_ptr().add(done).cast(), ordered);
                done += 32;
            }
            done
        }

        #[target_feature(enable = "avx2,avx512f,avx512bw")]
        pub(in super::super) unsafe fn wide(name: &[u16], pattern: &[u16]) -> bool {
            let candidates = name.len() - pattern.len() + 1;
            let first = _mm512_set1_epi16(pattern[0] as i16);
            let last = _mm512_set1_epi16(pattern[pattern.len() - 1] as i16);
            let mut at = 0;
            while at + 32 <= candidates {
                let starts = _mm512_loadu_si512(name.as_ptr().add(at).cast());
                let ends = _mm512_loadu_si512(name.as_ptr().add(at + pattern.len() - 1).cast());
                let mut mask =
                    _mm512_cmpeq_epi16_mask(starts, first) & _mm512_cmpeq_epi16_mask(ends, last);
                while mask != 0 {
                    let start = at + mask.trailing_zeros() as usize;
                    if &name[start..start + pattern.len()] == pattern {
                        return true;
                    }
                    mask &= mask - 1;
                }
                at += 32;
            }
            super::super::wide_scalar(&name[at..], pattern)
        }
    }
}

#[cfg(all(any(windows, test), target_arch = "aarch64", target_endian = "little"))]
mod neon {
    use std::arch::aarch64::*;

    #[target_feature(enable = "neon")]
    pub(super) unsafe fn ascii(bytes: &[u8], out: &mut [u8]) -> usize {
        let mut done = 0;
        while done + 8 <= out.len() {
            // Load bytes, not an aligned u16 slice; MFT attributes can be unaligned.
            let value = vreinterpretq_u16_u8(vld1q_u8(bytes.as_ptr().add(done * 2)));
            if vmaxvq_u16(value) > 0x7f {
                break;
            }
            vst1_u8(out.as_mut_ptr().add(done), vmovn_u16(value));
            done += 8;
        }
        done
    }

    #[target_feature(enable = "neon")]
    pub(super) unsafe fn wide(name: &[u16], pattern: &[u16]) -> bool {
        let candidates = name.len() - pattern.len() + 1;
        let first = vdupq_n_u16(pattern[0]);
        let last = vdupq_n_u16(pattern[pattern.len() - 1]);
        let mut at = 0;
        while at + 8 <= candidates {
            let starts = vld1q_u16(name.as_ptr().add(at));
            let ends = vld1q_u16(name.as_ptr().add(at + pattern.len() - 1));
            let equal = vandq_u16(vceqq_u16(starts, first), vceqq_u16(ends, last));
            let mut lanes = [0u16; 8];
            vst1q_u16(lanes.as_mut_ptr(), equal);
            for (lane, equal) in lanes.into_iter().enumerate() {
                let start = at + lane;
                if equal != 0 && &name[start..start + pattern.len()] == pattern {
                    return true;
                }
            }
            at += 8;
        }
        super::wide_scalar(&name[at..], pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available_backends() -> Vec<Backend> {
        let mut all = vec![Backend::Scalar];
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        all.push(Backend::Avx2);
        #[cfg(all(
            feature = "simd-avx512",
            any(target_arch = "x86", target_arch = "x86_64")
        ))]
        all.push(Backend::Avx512);
        #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
        all.push(Backend::Neon);
        all.into_iter()
            .filter(|backend| backend.available())
            .collect()
    }

    fn reference_decode(bytes: &[u8]) -> Option<String> {
        if bytes.len() % 2 != 0 {
            return None;
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        Some(String::from_utf16_lossy(&units))
    }

    #[test]
    fn decoder_matches_scalar_at_every_block_tail_and_byte_alignment() {
        for backend in available_backends() {
            for length in 0..=130 {
                let units: Vec<u16> = (0..length).map(|i| (i % 128) as u16).collect();
                let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
                for offset in 0..=33 {
                    let mut storage = vec![0xA5; offset];
                    storage.extend_from_slice(&bytes);
                    let input = &storage[offset..];
                    assert_eq!(
                        decode_with(backend, input),
                        reference_decode(input),
                        "{backend:?} length={length} offset={offset}"
                    );
                }
            }
        }
    }

    #[test]
    fn decoder_preserves_non_ascii_and_malformed_surrogates_in_any_lane() {
        let units = [
            0x0080, 0x0100, 0x3042, 0xD7FF, 0xD800, 0xDBFF, 0xDC00, 0xDFFF, 0xE000, 0xFFFF,
        ];
        for backend in available_backends() {
            for at in 0..65 {
                for unit in units {
                    let mut input = vec![b'a' as u16; 65];
                    input[at] = unit;
                    let bytes: Vec<u8> = input.iter().flat_map(|unit| unit.to_le_bytes()).collect();
                    assert_eq!(
                        decode_with(backend, &bytes),
                        Some(String::from_utf16_lossy(&input)),
                        "{backend:?} unit={unit:04x} lane={at}"
                    );
                    if at + 1 < input.len() {
                        input[at] = 0xD83D;
                        input[at + 1] = 0xDE00;
                        let bytes: Vec<u8> =
                            input.iter().flat_map(|unit| unit.to_le_bytes()).collect();
                        assert_eq!(
                            decode_with(backend, &bytes),
                            Some(String::from_utf16_lossy(&input))
                        );
                    }
                }
            }
            for length in [1, 15, 17, 31, 33, 63, 65, 129] {
                assert_eq!(decode_with(backend, &vec![0; length]), None);
            }
        }
    }

    #[test]
    fn wide_search_checks_candidates_and_all_block_tails() {
        for backend in available_backends() {
            for length in 0..=96 {
                for pattern in [
                    vec![],
                    vec![0xD800],
                    vec![0xD800, 0x3042, 0xDFFF],
                    vec![7; 33],
                ] {
                    for offset in [0, 1, 7, 15, 31] {
                        let mut storage = vec![8; offset + length];
                        let name = &mut storage[offset..];
                        assert_eq!(
                            wide_with(backend, name, &pattern),
                            wide_scalar(name, &pattern)
                        );
                        if !pattern.is_empty() && pattern.len() <= length {
                            for at in 0..=length - pattern.len() {
                                name.fill(8);
                                name[at..at + pattern.len()].copy_from_slice(&pattern);
                                assert!(
                                    wide_with(backend, name, &pattern),
                                    "{backend:?} length={length} start={at} offset={offset}"
                                );
                            }
                        }
                    }
                }
            }
            // First/last matches are only candidates; interiors must be checked.
            let name = vec![b'a' as u16; 128];
            assert!(!wide_with(
                backend,
                &name,
                &[b'a' as u16, b'b' as u16, b'a' as u16]
            ));
        }
    }

    #[test]
    fn wide_search_matches_scalar_for_deterministic_full_range_inputs() {
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as u16
        };
        for _ in 0..512 {
            let length = next() as usize % 257;
            let pattern_len = next() as usize % 65;
            let mut name: Vec<u16> = (0..length).map(|_| next()).collect();
            let pattern: Vec<u16> = (0..pattern_len).map(|_| next()).collect();
            for insert in [false, true] {
                if insert && pattern_len <= length {
                    let at = next() as usize % (length - pattern_len + 1);
                    name[at..at + pattern_len].copy_from_slice(&pattern);
                }
                for backend in available_backends() {
                    assert_eq!(
                        wide_with(backend, &name, &pattern),
                        wide_scalar(&name, &pattern)
                    );
                }
            }
        }
    }

    #[test]
    fn dispatch_only_accepts_auto_or_scalar_and_never_requires_unsupported_isa() {
        assert_eq!(configured(Some(OsStr::new("scalar"))), Ok(Backend::Scalar));
        assert_eq!(configured(None), Ok(Backend::detect()));
        assert_eq!(configured(Some(OsStr::new("auto"))), Ok(Backend::detect()));
        for invalid in ["", "AVX2", "avx512", "neon", "AUTO", " auto", "other"] {
            assert_eq!(configured(Some(OsStr::new(invalid))), Err(()));
        }
        assert!(selected().available());
        if std::env::var_os("HYPERDU_SIMD").as_deref() == Some(OsStr::new("scalar")) {
            assert_eq!(selected(), Backend::Scalar);
        }
    }

    #[test]
    fn supported_backends_execute_full_blocks_and_public_dispatch_matches() {
        let ascii: Vec<u8> = (0..128).flat_map(|n| (n as u16).to_le_bytes()).collect();
        let mut name = vec![12u16; 128];
        name[97..100].copy_from_slice(&[1, 2, 3]);
        for backend in available_backends() {
            assert_eq!(decode_with(backend, &ascii), reference_decode(&ascii));
            assert!(wide_with(backend, &name, &[1, 2, 3]));
            eprintln!("simd_executed_backend={backend:?}");
        }
        assert_eq!(decode_utf16le_lossy(&ascii), reference_decode(&ascii));
        assert!(contains_wide(&name, &[1, 2, 3]));
        for pattern in [b"".as_slice(), b"123", b"not-there", b"x123y", b"x123yz"] {
            let expected =
                !pattern.is_empty() && b"x123y".windows(pattern.len()).any(|w| w == pattern);
            assert_eq!(contains_bytes(b"x123y", pattern), expected);
        }
        eprintln!("simd_selected_backend={:?}", selected());
        #[cfg(all(
            feature = "simd-avx512",
            any(target_arch = "x86", target_arch = "x86_64")
        ))]
        if !Backend::Avx512.available() {
            eprintln!("simd_compile_only_backend=Avx512 (host lacks required features)");
        }
    }
}
