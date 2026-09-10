//! Shared directory-report formats for CLI and GUI.
use std::{
    io::Write,
    path::{Path, PathBuf},
};

use serde::{
    ser::{SerializeSeq, Serializer},
    Serialize,
};

use crate::Stat;

#[derive(Serialize)]
struct Row<'a> {
    path: &'a Path,
    logical: u64,
    physical: u64,
    files: u64,
}

/// Write a JSON array without materializing a second copy of the rows.
pub fn write_json(writer: impl Write, rows: &[(PathBuf, Stat)]) -> anyhow::Result<()> {
    let mut serializer = serde_json::Serializer::pretty(writer);
    let mut sequence = serializer.serialize_seq(Some(rows.len()))?;
    for (path, s) in rows {
        sequence.serialize_element(&Row {
            path,
            logical: s.logical,
            physical: s.physical,
            files: s.files,
        })?;
    }
    sequence.end()?;
    Ok(())
}

/// Write path/logical/physical/files columns, prefixing formula-like CSV paths
/// with an apostrophe for spreadsheet safety. JSON preserves the original path.
pub fn write_csv(writer: impl Write, rows: &[(PathBuf, Stat)]) -> anyhow::Result<()> {
    let mut writer = csv::Writer::from_writer(writer);
    writer.write_record(["path", "logical", "physical", "files"])?;
    for (path, s) in rows {
        let path = spreadsheet_text(path.to_string_lossy());
        writer.write_record([
            path.as_ref(),
            &s.logical.to_string(),
            &s.physical.to_string(),
            &s.files.to_string(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn spreadsheet_text(value: std::borrow::Cow<'_, str>) -> std::borrow::Cow<'_, str> {
    // Importers may ignore whitespace before a formula; control prefixes also
    // need protection. Preserve the original text after the apostrophe.
    let trimmed = value.trim_start_matches(char::is_whitespace);
    if matches!(trimmed.chars().next(), Some('=' | '+' | '-' | '@'))
        || matches!(value.chars().next(), Some('\t' | '\r' | '\n'))
    {
        std::borrow::Cow::Owned(format!("'{value}"))
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_formats_preserve_special_path_characters_and_totals() {
        let rows = vec![(
            PathBuf::from("a,quoted\"name"),
            Stat {
                logical: 7,
                physical: 16,
                files: 1,
            },
        )];
        let mut json = Vec::new();
        write_json(&mut json, &rows).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(value[0]["logical"], 7);
        assert_eq!(value[0]["path"], "a,quoted\"name");
        let mut csv = Vec::new();
        write_csv(&mut csv, &rows).unwrap();
        let record = csv::Reader::from_reader(csv.as_slice())
            .records()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(&record[0], "a,quoted\"name");
        assert_eq!(&record[2], "16");
    }
    #[test]
    fn csv_neutralizes_formula_prefixes_without_changing_json() {
        for path in [
            "=2+2",
            "+SUM(1,2)",
            "-2+2",
            "@SUM(1,2)",
            "\t=2+2",
            "\r=2+2",
            "\n=2+2",
            "  =2+2",
        ] {
            let rows = vec![(
                PathBuf::from(path),
                Stat {
                    logical: 7,
                    physical: 16,
                    files: 1,
                },
            )];
            let mut output = Vec::new();
            write_csv(&mut output, &rows).unwrap();
            let record = csv::Reader::from_reader(output.as_slice())
                .records()
                .next()
                .unwrap()
                .unwrap();
            assert_eq!(&record[0], format!("'{path}"));
            assert_eq!(&record[1], "7");
            let mut json = Vec::new();
            write_json(&mut json, &rows).unwrap();
            let parsed: serde_json::Value = serde_json::from_slice(&json).unwrap();
            assert_eq!(parsed[0]["path"], path);
        }
    }

    #[test]
    fn csv_preserves_safe_paths_and_quotes_delimiters() {
        for path in [
            "normal",
            "./=2+2",
            "/tmp/+name",
            "C:\\data",
            "'already-text",
            "a,quoted\"name",
            "日本語",
        ] {
            let rows = vec![(PathBuf::from(path), Stat::default())];
            let mut output = Vec::new();
            write_csv(&mut output, &rows).unwrap();
            let record = csv::Reader::from_reader(output.as_slice())
                .records()
                .next()
                .unwrap()
                .unwrap();
            assert_eq!(&record[0], path);
        }
    }
}
