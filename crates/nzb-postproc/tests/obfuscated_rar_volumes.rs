//! Detection-layer regression suite for issue #87: obfuscated multi-volume RAR
//! sets named `<32 hex digits>.NN` were never detected, never extracted, and
//! still let the job report Completed.
//!
//! The suite is deliberately paired:
//!
//! * `neg_*` — **negative controls**. Conventional naming (`.rar`/`.r00`,
//!   `.partNNN.rar`, `.7z.001`) and true payload. These passed before the fix
//!   as well as after, so a `pos_*` failure is about the *name*, not the
//!   plumbing. They are the guard against the fix breaking ordinary sets.
//!
//! * `pos_*` — the obfuscated `<hash>.NN` set. Each of these failed before the
//!   fix, one per broken layer.
//!
//! * `guard_*` — **false-positive guards**. A naive fix ("treat any numeric
//!   extension as a RAR volume") breaks these; they are why detection reads
//!   the RAR signature instead of trusting the extension.
//!
//! Fixtures carry real RAR signature bytes because detection now sniffs them.

use std::fs;
use std::path::Path;

use nzb_postproc::ArchiveType;
use nzb_postproc::detect::{
    find_archives, find_cleanup_files, has_usable_output, parse_rar_volume, parse_rar_volume_at,
};

/// RAR 4.x volume signature.
const RAR4_MAGIC: &[u8] = b"Rar!\x1a\x07\x00";
/// RAR 5.x volume signature.
const RAR5_MAGIC: &[u8] = b"Rar!\x1a\x07\x01\x00";

/// The obfuscated set name from the issue report.
const HASH: &str = "cfd4be79c0fb01d429c52a9f8551ee79";

/// Write `files` (name, contents) into a fresh temp dir.
fn make_dir(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, contents) in files {
        fs::write(dir.path().join(name), contents).unwrap();
    }
    dir
}

/// A RAR volume body: signature + filler so the file is not empty.
fn rar_volume(magic: &[u8]) -> Vec<u8> {
    let mut body = magic.to_vec();
    body.extend_from_slice(&[0u8; 64]);
    body
}

/// Count the RAR entries `find_archives` reported.
fn rar_archives(dir: &Path) -> Vec<String> {
    find_archives(dir)
        .into_iter()
        .filter(|(kind, _)| *kind == ArchiveType::Rar)
        .map(|(_, path)| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

/// Cleanup candidates as bare filenames.
fn cleanup_names(dir: &Path) -> Vec<String> {
    find_cleanup_files(dir)
        .into_iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Negative controls — conventional naming. Must stay green.
// ---------------------------------------------------------------------------

#[test]
fn neg_old_style_rar_set_is_detected_extracted_and_cleaned() {
    let body = rar_volume(RAR4_MAGIC);
    let dir = make_dir(&[
        ("movie.rar", &body),
        ("movie.r00", &body),
        ("movie.r01", &body),
        ("movie.par2", b""),
    ]);

    // Detected, and only the first volume is offered for extraction.
    assert_eq!(
        rar_archives(dir.path()),
        vec!["movie.rar".to_string()],
        "old-style set should yield exactly the first volume"
    );

    // Every volume is a cleanup candidate.
    let cleanup = cleanup_names(dir.path());
    for name in ["movie.rar", "movie.r00", "movie.r01", "movie.par2"] {
        assert!(
            cleanup.contains(&name.to_string()),
            "{name} should be cleanup"
        );
    }

    // Raw archives alone are not a usable completion.
    assert!(
        !has_usable_output(dir.path()).unwrap(),
        "a directory of raw volumes must not count as usable output"
    );

    // Volume numbering is understood.
    assert_eq!(parse_rar_volume("movie.rar").unwrap().volume_number, 0);
    assert_eq!(parse_rar_volume("movie.r00").unwrap().volume_number, 1);
    assert_eq!(parse_rar_volume("movie.r01").unwrap().volume_number, 2);
}

#[test]
fn neg_new_style_part_set_is_detected() {
    let body = rar_volume(RAR5_MAGIC);
    let dir = make_dir(&[
        ("movie.part001.rar", &body),
        ("movie.part002.rar", &body),
        ("movie.part003.rar", &body),
    ]);

    assert_eq!(
        rar_archives(dir.path()),
        vec!["movie.part001.rar".to_string()],
        "new-style set should yield exactly part001"
    );
    assert!(!has_usable_output(dir.path()).unwrap());

    let parsed = parse_rar_volume("movie.part002.rar").unwrap();
    assert_eq!(parsed.set_name, "movie");
    assert_eq!(parsed.volume_number, 1);
}

#[test]
fn neg_split_7z_set_is_detected() {
    let dir = make_dir(&[
        ("archive.7z.001", b"7z\xbc\xaf\x27\x1c"),
        ("archive.7z.002", b"data"),
    ]);

    let archives = find_archives(dir.path());
    assert_eq!(archives.len(), 1, "only the .001 volume starts extraction");
    assert_eq!(archives[0].0, ArchiveType::SevenZip);
    assert!(!has_usable_output(dir.path()).unwrap());
}

#[test]
fn neg_real_payload_counts_as_usable_output() {
    let dir = make_dir(&[("Movie.2024.1080p.mkv", b"\x1a\x45\xdf\xa3matroska")]);
    assert!(
        has_usable_output(dir.path()).unwrap(),
        "extracted media must count as usable output"
    );
    assert!(
        cleanup_names(dir.path()).is_empty(),
        "payload must never be a cleanup candidate"
    );
}

// ---------------------------------------------------------------------------
// The obfuscated `<hash>.NN` set — each of these failed before the fix.
// ---------------------------------------------------------------------------

/// Build the obfuscated set from the issue: `<hash>.45`, `.46`, `.47` + par2.
fn obfuscated_dir() -> tempfile::TempDir {
    let body = rar_volume(RAR4_MAGIC);
    let dir = tempfile::tempdir().unwrap();
    for n in 45..=47u32 {
        fs::write(dir.path().join(format!("{HASH}.{n}")), &body).unwrap();
    }
    fs::write(dir.path().join(format!("{HASH}.par2")), b"").unwrap();
    dir
}

#[test]
fn pos_obfuscated_numeric_volumes_are_detected_as_rar() {
    let dir = obfuscated_dir();
    let archives = rar_archives(dir.path());
    assert!(
        !archives.is_empty(),
        "obfuscated RAR set went undetected; find_archives returned nothing, \
         so the Extract stage reports \"No archives found\" and skips"
    );
    assert_eq!(
        archives.len(),
        1,
        "exactly one volume should start extraction, got {archives:?}"
    );
}

#[test]
fn pos_obfuscated_volumes_are_not_usable_output() {
    let dir = obfuscated_dir();
    assert!(
        !has_usable_output(dir.path()).unwrap(),
        "raw <hash>.NN volumes were reported as usable payload, which is what \
         lets move_to_history stamp the job Completed"
    );
}

#[test]
fn pos_obfuscated_volumes_are_cleanup_candidates() {
    let dir = obfuscated_dir();
    let cleanup = cleanup_names(dir.path());
    for n in 45..=47u32 {
        let name = format!("{HASH}.{n}");
        assert!(
            cleanup.contains(&name),
            "{name} should be a cleanup candidate; got {cleanup:?}"
        );
    }
}

#[test]
fn pos_obfuscated_volume_numbers_are_parsed() {
    // Name alone cannot distinguish an obfuscated volume from any other file
    // ending in digits, so numbering is resolved through the content-aware
    // entry point. `parse_rar_volume` deliberately still rejects these.
    let dir = obfuscated_dir();
    assert!(
        parse_rar_volume(&format!("{HASH}.45")).is_none(),
        "the name-only parser must not claim bare numeric extensions"
    );

    let parsed = parse_rar_volume_at(&dir.path().join(format!("{HASH}.45")))
        .expect("bare numeric RAR volume should parse from disk");
    assert_eq!(parsed.set_name, HASH);

    let next = parse_rar_volume_at(&dir.path().join(format!("{HASH}.46")))
        .expect("bare numeric RAR volume should parse from disk");
    assert_eq!(
        next.volume_number,
        parsed.volume_number + 1,
        "consecutive numeric volumes must order consecutively"
    );
}

#[test]
fn guard_numeric_file_without_signature_is_not_a_volume() {
    // The content check is what keeps `parse_rar_volume_at` honest.
    let dir = make_dir(&[("logfile.01", b"2026-08-11 INFO started")]);
    assert!(
        parse_rar_volume_at(&dir.path().join("logfile.01")).is_none(),
        "a numeric-suffixed file with no RAR signature is not a volume"
    );
}

#[test]
fn pos_obfuscated_rar5_volumes_are_detected() {
    let body = rar_volume(RAR5_MAGIC);
    let dir = tempfile::tempdir().unwrap();
    for n in 1..=3u32 {
        fs::write(dir.path().join(format!("{HASH}.{n:02}")), &body).unwrap();
    }
    assert!(
        !rar_archives(dir.path()).is_empty(),
        "RAR5 obfuscated set went undetected"
    );
}

// ---------------------------------------------------------------------------
// False-positive guards — a naive "any numeric extension is a RAR volume" fix
// breaks these. They are why detection sniffs the RAR signature.
// ---------------------------------------------------------------------------

#[test]
fn guard_numeric_suffix_without_rar_signature_is_not_an_archive() {
    // Looks like `<name>.NN` but holds no RAR signature — e.g. a stray data
    // file. Detection must not claim it.
    let dir = make_dir(&[
        (
            "Concert.Recording.1987",
            b"plain text notes, not an archive",
        ),
        ("logfile.01", b"2026-08-11 INFO started"),
    ]);
    assert!(
        rar_archives(dir.path()).is_empty(),
        "non-RAR numeric-suffixed files must not be detected as archives"
    );
}

#[test]
fn guard_split_7z_volumes_are_not_reclassified_as_rar() {
    // `.001` is numeric too. It must stay a 7z volume, not become a RAR set,
    // or extraction hands the wrong file to the wrong extractor.
    let dir = make_dir(&[
        ("archive.7z.001", b"7z\xbc\xaf\x27\x1c"),
        ("archive.7z.002", b"more data"),
    ]);
    assert!(
        rar_archives(dir.path()).is_empty(),
        "split 7z volumes must not be classified as RAR"
    );
    assert_eq!(find_archives(dir.path()).len(), 1);
}

#[test]
fn guard_media_file_with_year_suffix_stays_payload() {
    // `has_usable_output` must not start discarding real payload just because
    // the name ends in digits.
    let dir = make_dir(&[("Movie.Title.2024.mkv", b"\x1a\x45\xdf\xa3matroska")]);
    assert!(has_usable_output(dir.path()).unwrap());
}
