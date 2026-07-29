//! Deterministic stereo float32 WAV and Roon ZIP export.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

pub const EXPORT_FORMAT_VERSION: &str = "roon-stereo-wav-v1";
pub const ROON_SIX_RATE_SET: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
const MAX_STEREO_FIR_FRAMES: u32 = 1_048_576;

const README_NAME: &str = "README.txt";
const MANIFEST_NAME: &str = "manifest.json";
const MAX_STEREO_FIR_FILE_BYTES: u64 = MAX_STEREO_FIR_FRAMES as u64 * 2 * 4 + 1_048_576;
const MAX_ROON_ZIP_BYTES: u64 = 64 * 1_048_576;

#[derive(Debug)]
pub enum ExportError {
    Io(std::io::Error),
    Wav(hound::Error),
    Zip(zip::result::ZipError),
    Json(serde_json::Error),
    Invalid(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Wav(error) => write!(f, "WAV error: {error}"),
            Self::Zip(error) => write!(f, "ZIP error: {error}"),
            Self::Json(error) => write!(f, "JSON error: {error}"),
            Self::Invalid(message) => write!(f, "invalid export: {message}"),
        }
    }
}

impl std::error::Error for ExportError {}

impl From<std::io::Error> for ExportError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<hound::Error> for ExportError {
    fn from(value: hound::Error) -> Self {
        Self::Wav(value)
    }
}

impl From<zip::result::ZipError> for ExportError {
    fn from(value: zip::result::ZipError) -> Self {
        Self::Zip(value)
    }
}

impl From<serde_json::Error> for ExportError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub type ExportResult<T> = Result<T, ExportError>;

fn output_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn ensure_output_absent(path: &Path) -> ExportResult<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(ExportError::Invalid(format!(
            "output path already exists; refusing to overwrite it: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn persist_new_output(temporary: NamedTempFile, path: &Path) -> ExportResult<()> {
    temporary
        .persist_noclobber(path)
        .map_err(|error| ExportError::Io(error.error))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct StereoFir {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterFile {
    pub file: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub sample_format: String,
    pub frames: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportManifest {
    pub format_version: String,
    pub algorithm_version: String,
    pub filters: Vec<FilterFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageValidation {
    pub entries: Vec<String>,
    pub manifest: ExportManifest,
}

fn validate_sample_rate(sample_rate: u32) -> ExportResult<()> {
    if !(8_000..=768_000).contains(&sample_rate) {
        return Err(ExportError::Invalid(format!(
            "sample rate {sample_rate} is outside 8000..=768000 Hz"
        )));
    }
    Ok(())
}

/// Restrict archive names to portable, single-component ASCII file names.
/// This avoids traversal as well as extraction collisions on Windows.
fn validate_flat_entry_name(name: &str) -> ExportResult<()> {
    let valid_characters = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    let base_name = name.split('.').next().unwrap_or_default();
    let upper_base = base_name.to_ascii_uppercase();
    let windows_device_name = matches!(
        upper_base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$"
    ) || (upper_base.len() == 4
        && (upper_base.starts_with("COM") || upper_base.starts_with("LPT"))
        && matches!(upper_base.as_bytes()[3], b'1'..=b'9'));

    if name.is_empty()
        || name.len() > 255
        || name.starts_with('.')
        || name.ends_with('.')
        || !valid_characters
        || windows_device_name
    {
        return Err(ExportError::Invalid(format!(
            "package entry must be a flat safe name: {name}"
        )));
    }
    Ok(())
}

fn validate_wav_entry_name(name: &str) -> ExportResult<()> {
    validate_flat_entry_name(name)?;
    if !name.to_ascii_lowercase().ends_with(".wav") {
        return Err(ExportError::Invalid(format!(
            "filter file must have a .wav extension: {name}"
        )));
    }
    if name.eq_ignore_ascii_case(README_NAME) || name.eq_ignore_ascii_case(MANIFEST_NAME) {
        return Err(ExportError::Invalid(format!(
            "filter file name is reserved: {name}"
        )));
    }
    Ok(())
}

fn declared_zip_entry_count(bytes: &[u8]) -> ExportResult<usize> {
    const END_OF_CENTRAL_DIRECTORY: &[u8; 4] = b"PK\x05\x06";
    const FIXED_END_SIZE: usize = 22;
    const MAX_COMMENT_SIZE: usize = u16::MAX as usize;

    if bytes.len() < FIXED_END_SIZE {
        return Err(ExportError::Invalid(
            "ZIP end-of-central-directory record is absent".into(),
        ));
    }
    let earliest = bytes
        .len()
        .saturating_sub(FIXED_END_SIZE + MAX_COMMENT_SIZE);
    let end_offset = (earliest..=bytes.len() - FIXED_END_SIZE)
        .rev()
        .find(|&offset| {
            if &bytes[offset..offset + 4] != END_OF_CENTRAL_DIRECTORY {
                return false;
            }
            let comment_size =
                u16::from_le_bytes([bytes[offset + 20], bytes[offset + 21]]) as usize;
            offset + FIXED_END_SIZE + comment_size == bytes.len()
        })
        .ok_or_else(|| {
            ExportError::Invalid("ZIP end-of-central-directory record is absent".into())
        })?;

    let disk_number = u16::from_le_bytes([bytes[end_offset + 4], bytes[end_offset + 5]]);
    let central_directory_disk = u16::from_le_bytes([bytes[end_offset + 6], bytes[end_offset + 7]]);
    let entries_on_disk = u16::from_le_bytes([bytes[end_offset + 8], bytes[end_offset + 9]]);
    let entry_count = u16::from_le_bytes([bytes[end_offset + 10], bytes[end_offset + 11]]);
    if disk_number != 0
        || central_directory_disk != 0
        || entries_on_disk != entry_count
        || entry_count == u16::MAX
    {
        return Err(ExportError::Invalid(
            "multi-disk and ZIP64 packages are not supported".into(),
        ));
    }
    Ok(usize::from(entry_count))
}

fn validate_fir(fir: &StereoFir) -> ExportResult<()> {
    validate_sample_rate(fir.sample_rate)?;
    if fir.left.is_empty() || fir.left.len() != fir.right.len() {
        return Err(ExportError::Invalid(
            "left/right filters must be non-empty and have equal lengths".into(),
        ));
    }
    if fir
        .left
        .iter()
        .chain(&fir.right)
        .any(|sample| !sample.is_finite())
    {
        return Err(ExportError::Invalid(
            "filter contains NaN or infinity".into(),
        ));
    }
    Ok(())
}

/// Write interleaved L/R IEEE float32 samples. No normalization is performed.
pub fn write_stereo_wav(path: &Path, fir: &StereoFir) -> ExportResult<FilterFile> {
    validate_fir(fir)?;
    let output_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ExportError::Invalid("WAV path must have a UTF-8 file name".into()))?;
    ensure_output_absent(path)?;
    let parent = output_parent(path);
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: fir.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::new(temporary.as_file_mut(), spec)?;
    for (&left, &right) in fir.left.iter().zip(&fir.right) {
        writer.write_sample(left)?;
        writer.write_sample(right)?;
    }
    writer.finalize()?;
    temporary.as_file().sync_all()?;
    let mut metadata = inspect_wav(temporary.path())?;
    metadata.file = output_name.into();
    persist_new_output(temporary, path)?;
    Ok(metadata)
}

pub fn inspect_wav(path: &Path) -> ExportResult<FilterFile> {
    let mut reader = hound::WavReader::open(path)?;
    inspect_reader(
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ExportError::Invalid("WAV path must have a UTF-8 file name".into()))?,
        &mut reader,
    )
}

fn validated_stereo_fir_sample_count(name: &str, declared_samples: u32) -> ExportResult<usize> {
    if declared_samples == 0 || declared_samples % 2 != 0 {
        return Err(ExportError::Invalid(format!(
            "{name} must contain a non-empty whole number of stereo frames"
        )));
    }
    if declared_samples / 2 > MAX_STEREO_FIR_FRAMES {
        return Err(ExportError::Invalid(format!(
            "{name} exceeds the {MAX_STEREO_FIR_FRAMES}-frame FIR read limit"
        )));
    }
    Ok(declared_samples as usize)
}

/// Read a stereo IEEE-float32 FIR WAV without normalization or channel changes.
///
/// This is intentionally stricter than a general-purpose audio decoder: Phase 5
/// shadow analysis must consume the exact filter layout produced by
/// [`write_stereo_wav`].
pub fn read_stereo_wav(path: &Path) -> ExportResult<StereoFir> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ExportError::Invalid("WAV path must have a UTF-8 file name".into()))?;
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_STEREO_FIR_FILE_BYTES {
        return Err(ExportError::Invalid(format!(
            "{name} exceeds the {MAX_STEREO_FIR_FILE_BYTES}-byte FIR read limit"
        )));
    }
    let bytes = fs::read(path)?;
    read_stereo_wav_bytes(name, &bytes)
}

/// Decode one already-read stereo FIR WAV payload.
///
/// Callers that also persist a hash should hash this same byte slice, avoiding a
/// second path read that could observe different file contents.
pub fn read_stereo_wav_bytes(name: &str, bytes: &[u8]) -> ExportResult<StereoFir> {
    if bytes.len() as u64 > MAX_STEREO_FIR_FILE_BYTES {
        return Err(ExportError::Invalid(format!(
            "{name} exceeds the {MAX_STEREO_FIR_FILE_BYTES}-byte FIR read limit"
        )));
    }
    let mut reader = hound::WavReader::new(Cursor::new(bytes))?;
    let spec = reader.spec();
    validate_sample_rate(spec.sample_rate)?;
    if spec.channels != 2
        || spec.bits_per_sample != 32
        || spec.sample_format != hound::SampleFormat::Float
    {
        return Err(ExportError::Invalid(format!(
            "{name} must be stereo IEEE float32 WAV"
        )));
    }
    let declared_samples = validated_stereo_fir_sample_count(name, reader.len())?;
    let samples: Vec<f32> = reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?;
    if samples.len() != declared_samples {
        return Err(ExportError::Invalid(format!(
            "{name} sample data is shorter than its declared WAV length"
        )));
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(ExportError::Invalid(format!(
            "{name} contains NaN or infinity"
        )));
    }
    let mut left = Vec::with_capacity(samples.len() / 2);
    let mut right = Vec::with_capacity(samples.len() / 2);
    for frame in samples.chunks_exact(2) {
        left.push(frame[0]);
        right.push(frame[1]);
    }
    let fir = StereoFir {
        sample_rate: spec.sample_rate,
        left,
        right,
    };
    validate_fir(&fir)?;
    Ok(fir)
}

fn inspect_wav_bytes(name: &str, bytes: Vec<u8>) -> ExportResult<FilterFile> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes))?;
    inspect_reader(name, &mut reader)
}

fn inspect_reader<R: Read>(
    name: &str,
    reader: &mut hound::WavReader<R>,
) -> ExportResult<FilterFile> {
    let spec = reader.spec();
    validate_sample_rate(spec.sample_rate)?;
    if spec.channels != 2
        || spec.bits_per_sample != 32
        || spec.sample_format != hound::SampleFormat::Float
    {
        return Err(ExportError::Invalid(format!(
            "{name} must be stereo IEEE float32 WAV"
        )));
    }
    let sample_count = reader.len();
    if sample_count == 0 || sample_count % 2 != 0 {
        return Err(ExportError::Invalid(format!(
            "{name} must contain a non-empty whole number of stereo frames"
        )));
    }
    for sample in reader.samples::<f32>() {
        if !sample?.is_finite() {
            return Err(ExportError::Invalid(format!(
                "{name} contains NaN or infinity"
            )));
        }
    }
    Ok(FilterFile {
        file: name.into(),
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        bits_per_sample: spec.bits_per_sample,
        sample_format: "ieee-float".into(),
        frames: sample_count / 2,
    })
}

/// Package already-written WAVs plus the required human instructions and a
/// machine-readable manifest. Roon does not require a `.cfg` for stereo WAVs.
pub fn create_roon_zip(
    path: &Path,
    wav_paths: &[PathBuf],
    algorithm_version: &str,
    readme: &str,
) -> ExportResult<ExportManifest> {
    if wav_paths.is_empty() {
        return Err(ExportError::Invalid("at least one WAV is required".into()));
    }
    if algorithm_version.trim().is_empty() {
        return Err(ExportError::Invalid(
            "algorithm version must not be empty".into(),
        ));
    }
    if readme.trim().is_empty() {
        return Err(ExportError::Invalid("README.txt must not be empty".into()));
    }
    ensure_output_absent(path)?;

    let mut sources = Vec::with_capacity(wav_paths.len());
    let mut rates = BTreeSet::new();
    let mut names = BTreeSet::new();
    for wav_path in wav_paths {
        let metadata = inspect_wav(wav_path)?;
        validate_wav_entry_name(&metadata.file)?;
        if !rates.insert(metadata.sample_rate) {
            return Err(ExportError::Invalid(format!(
                "duplicate sample rate {}",
                metadata.sample_rate
            )));
        }
        if !names.insert(metadata.file.to_ascii_lowercase()) {
            return Err(ExportError::Invalid(format!(
                "duplicate file name {}",
                metadata.file
            )));
        }
        sources.push((metadata, fs::read(wav_path)?));
    }
    sources.sort_by_key(|(filter, _)| filter.sample_rate);
    let manifest = ExportManifest {
        format_version: EXPORT_FORMAT_VERSION.into(),
        algorithm_version: algorithm_version.into(),
        filters: sources.iter().map(|(filter, _)| filter.clone()).collect(),
    };

    let parent = output_parent(path);
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    let mut archive = ZipWriter::new(temporary.as_file_mut());
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(DateTime::default())
        .unix_permissions(0o644);
    for (filter, bytes) in &sources {
        archive.start_file(&filter.file, options)?;
        archive.write_all(bytes)?;
    }
    archive.start_file(README_NAME, options)?;
    archive.write_all(readme.as_bytes())?;
    archive.start_file(MANIFEST_NAME, options)?;
    archive.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    archive.finish()?;
    temporary.as_file().sync_all()?;
    validate_roon_zip(temporary.path(), &rates)?;
    persist_new_output(temporary, path)?;
    Ok(manifest)
}

/// Create the product's complete six-native-rate Roon package.
///
/// This function verifies exact sample-rate coverage before writing. Export
/// eligibility remains an application-layer evidence decision; callers must
/// never use this primitive to promote an unverified measured project.
pub fn create_roon_six_rate_zip(
    path: &Path,
    wav_paths: &[PathBuf],
    algorithm_version: &str,
    readme: &str,
) -> ExportResult<ExportManifest> {
    let expected_rates: BTreeSet<u32> = ROON_SIX_RATE_SET.into_iter().collect();
    let found_rates: BTreeSet<u32> = wav_paths
        .iter()
        .map(|wav| inspect_wav(wav).map(|metadata| metadata.sample_rate))
        .collect::<ExportResult<_>>()?;
    if wav_paths.len() != ROON_SIX_RATE_SET.len() || found_rates != expected_rates {
        return Err(ExportError::Invalid(format!(
            "six-rate Roon export requires exactly {expected_rates:?}, found {found_rates:?}"
        )));
    }
    for wav in wav_paths {
        let metadata = inspect_wav(wav)?;
        let expected_name = format!("EQforBeginner_{}_stereo.wav", metadata.sample_rate);
        if metadata.file != expected_name {
            return Err(ExportError::Invalid(format!(
                "six-rate Roon WAV must use canonical name {expected_name}, found {}",
                metadata.file
            )));
        }
    }
    let manifest = create_roon_zip(path, wav_paths, algorithm_version, readme)?;
    validate_roon_six_rate_zip(path)?;
    Ok(manifest)
}

pub fn validate_roon_six_rate_zip(path: &Path) -> ExportResult<PackageValidation> {
    let validation = validate_roon_zip(path, &ROON_SIX_RATE_SET.into_iter().collect())?;
    for filter in &validation.manifest.filters {
        let expected_name = format!("EQforBeginner_{}_stereo.wav", filter.sample_rate);
        if filter.file != expected_name {
            return Err(ExportError::Invalid(format!(
                "six-rate Roon manifest requires canonical name {expected_name}, found {}",
                filter.file
            )));
        }
    }
    Ok(validation)
}

pub fn validate_roon_zip(
    path: &Path,
    expected_rates: &BTreeSet<u32>,
) -> ExportResult<PackageValidation> {
    if expected_rates.is_empty() {
        return Err(ExportError::Invalid(
            "at least one expected sample rate is required".into(),
        ));
    }
    for &rate in expected_rates {
        validate_sample_rate(rate)?;
    }

    let archive_size = fs::metadata(path)?.len();
    if archive_size > MAX_ROON_ZIP_BYTES {
        return Err(ExportError::Invalid(format!(
            "Roon ZIP exceeds the {MAX_ROON_ZIP_BYTES}-byte validation limit"
        )));
    }
    let archive_bytes = fs::read(path)?;
    let declared_entry_count = declared_zip_entry_count(&archive_bytes)?;
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))?;
    if declared_entry_count != archive.len() {
        return Err(ExportError::Invalid(
            "ZIP central directory contains duplicate package entry names".into(),
        ));
    }
    let mut entries = Vec::with_capacity(archive.len());
    let mut entry_names = BTreeSet::new();
    let mut wavs = Vec::new();
    let mut manifest_json = None;
    let mut readme = None;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().to_string();
        validate_flat_entry_name(&name)?;
        if !entry_names.insert(name.to_ascii_lowercase()) {
            return Err(ExportError::Invalid(format!(
                "duplicate package entry name: {name}"
            )));
        }
        if entry.compression() != CompressionMethod::Stored {
            return Err(ExportError::Invalid(format!(
                "package entry must use stored compression: {name}"
            )));
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        if name.to_ascii_lowercase().ends_with(".wav") {
            validate_wav_entry_name(&name)?;
            wavs.push(inspect_wav_bytes(&name, bytes)?);
        } else if name == README_NAME {
            readme = Some(bytes);
        } else if name == MANIFEST_NAME {
            manifest_json = Some(bytes);
        } else {
            return Err(ExportError::Invalid(format!(
                "unexpected package entry {name}"
            )));
        }
        entries.push(name);
    }

    let readme_bytes = readme.ok_or_else(|| ExportError::Invalid("README.txt is absent".into()))?;
    let readme_text = std::str::from_utf8(&readme_bytes)
        .map_err(|_| ExportError::Invalid("README.txt must be valid UTF-8".into()))?;
    if readme_text.trim().is_empty() {
        return Err(ExportError::Invalid("README.txt is empty".into()));
    }
    let manifest_bytes =
        manifest_json.ok_or_else(|| ExportError::Invalid("manifest.json is absent".into()))?;
    let manifest: ExportManifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.format_version != EXPORT_FORMAT_VERSION {
        return Err(ExportError::Invalid(format!(
            "unsupported manifest format version: {}",
            manifest.format_version
        )));
    }
    if manifest.algorithm_version.trim().is_empty() {
        return Err(ExportError::Invalid(
            "manifest algorithm version must not be empty".into(),
        ));
    }

    wavs.sort_by_key(|filter| filter.sample_rate);
    let found_rates: BTreeSet<u32> = wavs.iter().map(|filter| filter.sample_rate).collect();
    if found_rates.len() != wavs.len() {
        return Err(ExportError::Invalid(
            "package contains more than one WAV for a sample rate".into(),
        ));
    }
    if &found_rates != expected_rates {
        return Err(ExportError::Invalid(format!(
            "sample-rate mapping mismatch: expected {expected_rates:?}, found {found_rates:?}"
        )));
    }
    if wavs != manifest.filters {
        return Err(ExportError::Invalid(
            "manifest does not match parsed WAV metadata".into(),
        ));
    }
    if entries.len() != wavs.len() + 2 {
        return Err(ExportError::Invalid(
            "package must contain exactly one README, one manifest, and the mapped WAVs".into(),
        ));
    }
    Ok(PackageValidation { entries, manifest })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    fn impulse(rate: u32) -> StereoFir {
        StereoFir {
            sample_rate: rate,
            left: vec![1.0, 0.25, 0.0],
            right: vec![0.5, -0.25, 0.0],
        }
    }

    fn write_test_package(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create test ZIP");
        let mut archive = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .last_modified_time(DateTime::default())
            .unix_permissions(0o644);
        for &(name, bytes) in entries {
            archive.start_file(name, options).expect("start test entry");
            archive.write_all(bytes).expect("write test entry");
        }
        archive.finish().expect("finish test ZIP");
    }

    #[test]
    fn wav_round_trip_preserves_layout_rate_format_and_order() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("filter.wav");
        let metadata = write_stereo_wav(&path, &impulse(48_000)).expect("write WAV");
        assert_eq!(metadata.sample_rate, 48_000);
        assert_eq!(metadata.channels, 2);
        assert_eq!(metadata.bits_per_sample, 32);
        assert_eq!(metadata.sample_format, "ieee-float");
        assert_eq!(metadata.frames, 3);

        let mut reader = hound::WavReader::open(path).expect("open WAV");
        assert_eq!(
            reader.spec(),
            hound::WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            }
        );
        let samples: Vec<f32> = reader
            .samples::<f32>()
            .map(|sample| sample.expect("sample"))
            .collect();
        assert_eq!(samples, vec![1.0, 0.5, 0.25, -0.25, 0.0, 0.0]);

        let decoded = read_stereo_wav(&directory.path().join("filter.wav"))
            .expect("decode strict stereo FIR");
        assert_eq!(decoded, impulse(48_000));
    }

    #[test]
    fn stereo_fir_reader_rejects_oversized_headers_before_allocation() {
        assert!(validated_stereo_fir_sample_count("empty.wav", 0).is_err());
        assert!(validated_stereo_fir_sample_count("partial.wav", 3).is_err());
        assert_eq!(
            validated_stereo_fir_sample_count("limit.wav", MAX_STEREO_FIR_FRAMES * 2)
                .expect("configured maximum"),
            MAX_STEREO_FIR_FRAMES as usize * 2
        );
        assert!(
            validated_stereo_fir_sample_count("oversized.wav", MAX_STEREO_FIR_FRAMES * 2 + 2)
                .is_err()
        );
    }

    #[test]
    fn zip_is_deterministic_stored_and_manifest_matches_wav_headers() {
        let directory = tempdir().expect("temporary directory");
        let rates = [44_100, 48_000];
        let wavs: Vec<PathBuf> = rates
            .iter()
            .map(|rate| {
                let path = directory
                    .path()
                    .join(format!("EQforBeginner_{rate}_stereo.wav"));
                write_stereo_wav(&path, &impulse(*rate)).expect("write WAV");
                path
            })
            .collect();
        let zip_path = directory.path().join("filters-a.zip");
        let manifest = create_roon_zip(&zip_path, &wavs, "test-v1", "Test instructions\n")
            .expect("create ZIP");
        assert_eq!(manifest.filters.len(), 2);
        let expected = rates.into_iter().collect();
        let validation = validate_roon_zip(&zip_path, &expected).expect("validate ZIP");
        assert_eq!(
            validation.entries,
            vec![
                "EQforBeginner_44100_stereo.wav",
                "EQforBeginner_48000_stereo.wav",
                README_NAME,
                MANIFEST_NAME,
            ]
        );

        let second_zip_path = directory.path().join("filters-b.zip");
        let reversed_wavs: Vec<PathBuf> = wavs.iter().rev().cloned().collect();
        create_roon_zip(
            &second_zip_path,
            &reversed_wavs,
            "test-v1",
            "Test instructions\n",
        )
        .expect("create second ZIP");
        assert_eq!(
            fs::read(&zip_path).expect("read first ZIP"),
            fs::read(&second_zip_path).expect("read second ZIP")
        );

        let mut archive =
            ZipArchive::new(File::open(zip_path).expect("open ZIP")).expect("parse ZIP");
        for index in 0..archive.len() {
            let entry = archive.by_index(index).expect("read ZIP entry");
            assert_eq!(entry.compression(), CompressionMethod::Stored);
            assert_eq!(entry.last_modified(), Some(DateTime::default()));
        }
    }

    #[test]
    fn six_rate_package_requires_exact_native_rate_set() {
        let directory = tempdir().expect("temporary directory");
        let wavs: Vec<PathBuf> = ROON_SIX_RATE_SET
            .iter()
            .map(|rate| {
                let path = directory
                    .path()
                    .join(format!("EQforBeginner_{rate}_stereo.wav"));
                write_stereo_wav(&path, &impulse(*rate)).expect("write WAV");
                path
            })
            .collect();
        let zip = directory.path().join("six-rates.zip");
        create_roon_six_rate_zip(&zip, &wavs, "phase6-test", "Reference instructions\n")
            .expect("create exact six-rate ZIP");
        let validation = validate_roon_six_rate_zip(&zip).expect("validate six-rate ZIP");
        assert_eq!(validation.manifest.filters.len(), 6);

        let incomplete = directory.path().join("incomplete.zip");
        assert!(create_roon_six_rate_zip(
            &incomplete,
            &wavs[..5],
            "phase6-test",
            "Reference instructions\n",
        )
        .is_err());
        assert!(!incomplete.exists());

        let noncanonical = directory.path().join("wrong-name.wav");
        write_stereo_wav(&noncanonical, &impulse(44_100)).expect("write noncanonical WAV");
        let mut wrong_names = wavs.clone();
        wrong_names[0] = noncanonical;
        let bad_names_zip = directory.path().join("bad-names.zip");
        assert!(create_roon_six_rate_zip(
            &bad_names_zip,
            &wrong_names,
            "phase6-test",
            "Reference instructions\n",
        )
        .is_err());
        assert!(!bad_names_zip.exists());
    }

    #[test]
    fn non_finite_filter_is_rejected_before_write() {
        let directory = tempdir().expect("temporary directory");
        let invalid = StereoFir {
            sample_rate: 48_000,
            left: vec![f32::NAN],
            right: vec![1.0],
        };
        let error = write_stereo_wav(&directory.path().join("bad.wav"), &invalid)
            .expect_err("NaN must fail");
        assert!(error.to_string().contains("NaN"));
    }

    #[test]
    fn writers_refuse_existing_or_aliased_destinations_without_data_loss() {
        let directory = tempdir().expect("temporary directory");
        let wav_path = directory.path().join("keep.wav");
        write_stereo_wav(&wav_path, &impulse(48_000)).expect("write source WAV");
        let original_wav = fs::read(&wav_path).expect("read original WAV");

        let wav_error = write_stereo_wav(&wav_path, &impulse(48_000))
            .expect_err("existing WAV must not be overwritten");
        assert!(wav_error.to_string().contains("refusing to overwrite"));
        assert_eq!(fs::read(&wav_path).expect("reread WAV"), original_wav);

        let alias_error = create_roon_zip(
            &wav_path,
            std::slice::from_ref(&wav_path),
            "test-v1",
            "Instructions\n",
        )
        .expect_err("ZIP destination must not alias an input WAV");
        assert!(alias_error.to_string().contains("refusing to overwrite"));
        assert_eq!(
            fs::read(&wav_path).expect("reread aliased WAV"),
            original_wav
        );

        let zip_path = directory.path().join("keep.zip");
        fs::write(&zip_path, b"existing package").expect("write package sentinel");
        let zip_error = create_roon_zip(&zip_path, &[wav_path], "test-v1", "Instructions\n")
            .expect_err("existing ZIP must not be overwritten");
        assert!(zip_error.to_string().contains("refusing to overwrite"));
        assert_eq!(
            fs::read(&zip_path).expect("reread package sentinel"),
            b"existing package"
        );
    }

    #[test]
    fn non_finite_sample_in_existing_wav_is_rejected() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("bad.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::create(&path, spec).expect("create WAV");
        writer.write_sample(f32::INFINITY).expect("write infinity");
        writer.write_sample(0.0_f32).expect("write finite sample");
        writer.finalize().expect("finish WAV");

        let error = inspect_wav(&path).expect_err("infinity must fail");
        assert!(error.to_string().contains("infinity"));
    }

    #[test]
    fn unsafe_or_case_colliding_wav_names_are_rejected() {
        let directory = tempdir().expect("temporary directory");
        let unsafe_path = directory.path().join(r"..\unsafe.wav");
        write_stereo_wav(&unsafe_path, &impulse(44_100)).expect("write unsafe-name WAV");
        let error = create_roon_zip(
            &directory.path().join("unsafe.zip"),
            &[unsafe_path],
            "test-v1",
            "Instructions\n",
        )
        .expect_err("backslash name must fail");
        assert!(error.to_string().contains("flat safe name"));

        let lower = directory.path().join("lower/filter.wav");
        let upper = directory.path().join("upper/FILTER.wav");
        write_stereo_wav(&lower, &impulse(44_100)).expect("write first WAV");
        write_stereo_wav(&upper, &impulse(48_000)).expect("write second WAV");
        let error = create_roon_zip(
            &directory.path().join("collision.zip"),
            &[lower, upper],
            "test-v1",
            "Instructions\n",
        )
        .expect_err("case-insensitive collision must fail");
        assert!(error.to_string().contains("duplicate file name"));
    }

    #[test]
    fn validator_rejects_duplicate_required_entries() {
        let directory = tempdir().expect("temporary directory");
        let wav_path = directory.path().join("filter.wav");
        let metadata = write_stereo_wav(&wav_path, &impulse(48_000)).expect("write WAV");
        let wav_bytes = fs::read(&wav_path).expect("read WAV");
        let manifest = ExportManifest {
            format_version: EXPORT_FORMAT_VERSION.into(),
            algorithm_version: "test-v1".into(),
            filters: vec![metadata],
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).expect("serialize manifest");
        let zip_path = directory.path().join("duplicate.zip");
        write_test_package(
            &zip_path,
            &[
                ("filter.wav", &wav_bytes),
                ("READM1.txt", b"Instructions\n"),
                (README_NAME, b"Instructions\n"),
                (MANIFEST_NAME, &manifest_bytes),
            ],
        );
        let mut zip_bytes = fs::read(&zip_path).expect("read test ZIP");
        for start in 0..=zip_bytes.len() - b"READM1.txt".len() {
            if &zip_bytes[start..start + b"READM1.txt".len()] == b"READM1.txt" {
                zip_bytes[start..start + README_NAME.len()].copy_from_slice(README_NAME.as_bytes());
            }
        }
        fs::write(&zip_path, zip_bytes).expect("write duplicate-name ZIP");

        let expected = [48_000].into_iter().collect();
        let error = validate_roon_zip(&zip_path, &expected)
            .expect_err("duplicate README must fail validation");
        assert!(error.to_string().contains("duplicate package entry"));
    }

    #[test]
    fn validator_rejects_manifest_wav_mapping_mismatch() {
        let directory = tempdir().expect("temporary directory");
        let wav_path = directory.path().join("filter.wav");
        let mut metadata = write_stereo_wav(&wav_path, &impulse(48_000)).expect("write WAV");
        let wav_bytes = fs::read(&wav_path).expect("read WAV");
        metadata.file = "different.wav".into();
        let manifest = ExportManifest {
            format_version: EXPORT_FORMAT_VERSION.into(),
            algorithm_version: "test-v1".into(),
            filters: vec![metadata],
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).expect("serialize manifest");
        let zip_path = directory.path().join("mismatch.zip");
        write_test_package(
            &zip_path,
            &[
                ("filter.wav", &wav_bytes),
                (README_NAME, b"Instructions\n"),
                (MANIFEST_NAME, &manifest_bytes),
            ],
        );

        let expected = [48_000].into_iter().collect();
        let error = validate_roon_zip(&zip_path, &expected)
            .expect_err("manifest mapping mismatch must fail");
        assert!(error.to_string().contains("manifest does not match"));
    }
}
