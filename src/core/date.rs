//! Image extension filtering and date extraction (EXIF / filesystem times).

use super::fs_guard::open_regular;
use super::{DateBasis, ResolvedBasis};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::io::{self, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

pub(crate) fn image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|s| {
            matches!(
                s.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "bmp" | "gif" | "heic" | "heif"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn exif_date(file: &File) -> Option<NaiveDate> {
    let mut reader = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let field = exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)?;
    if let exif::Value::Ascii(values) = &field.value {
        let value = std::str::from_utf8(values.first()?).ok()?;
        parse_exif_date(value)
    } else {
        None
    }
}

pub(crate) fn parse_exif_date(value: &str) -> Option<NaiveDate> {
    let text = value.trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace());
    // Check the entire date/time, not only its date portion (bad times are bad EXIF).
    NaiveDateTime::parse_from_str(text, "%Y:%m:%d %H:%M:%S")
        .ok()
        .map(|d| d.date())
}

pub(crate) fn local_date(time: SystemTime) -> NaiveDate {
    DateTime::<Local>::from(time).date_naive()
}

pub(crate) fn select_date(
    path: &Path,
    metadata: &Metadata,
    basis: DateBasis,
) -> io::Result<(NaiveDate, ResolvedBasis)> {
    match basis {
        DateBasis::TakenFirst => {
            let mut file = open_regular(path)?;
            if let Some(date) = exif_date(&file) {
                return Ok((date, ResolvedBasis::ExifOriginal));
            }
            // The handle can have been advanced by the EXIF reader; no subsequent
            // file reads are needed, but reset it for clarity.
            let _ = file.seek(SeekFrom::Start(0));
            Ok((
                local_date(metadata.modified()?),
                ResolvedBasis::ModifiedFallback,
            ))
        }
        DateBasis::Modified => Ok((local_date(metadata.modified()?), ResolvedBasis::Modified)),
        DateBasis::Created => Ok((local_date(metadata.created()?), ResolvedBasis::Created)),
    }
}
