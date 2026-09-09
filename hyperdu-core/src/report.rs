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

/// Write the same path/logical/physical/files columns used by JSON.
pub fn write_csv(writer: impl Write, rows: &[(PathBuf, Stat)]) -> anyhow::Result<()> {
    let mut writer = csv::Writer::from_writer(writer);
    writer.write_record(["path", "logical", "physical", "files"])?;
    for (path, s) in rows {
        writer.write_record([
            path.to_string_lossy().as_ref(),
            &s.logical.to_string(),
            &s.physical.to_string(),
            &s.files.to_string(),
        ])?;
    }
    writer.flush()?;
    Ok(())
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
}
