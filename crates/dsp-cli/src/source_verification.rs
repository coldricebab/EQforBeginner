use super::{HarnessError, HarnessResult};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path};

#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceDeclaration<'a> {
    pub id: &'a str,
    pub relative_path: &'a str,
    pub sha256: &'a str,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedSource {
    pub id: String,
    pub relative_path: String,
    pub sha256: String,
    pub bytes: u64,
}

fn invalid(message: impl Into<String>) -> HarnessError {
    HarnessError::Invalid(message.into())
}

fn safe_relative_path(path: &str) -> bool {
    let path = Path::new(path);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub(crate) fn sha256_file(path: &Path) -> HarnessResult<(String, u64)> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        bytes += count as u64;
    }
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((sha256, bytes))
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn verify_declared_sources<'a>(
    source_root: &Path,
    declarations: impl IntoIterator<Item = SourceDeclaration<'a>>,
) -> HarnessResult<Vec<VerifiedSource>> {
    let canonical_root = fs::canonicalize(source_root)?;
    let mut seen_paths = HashSet::new();
    let mut verified = Vec::new();
    for declaration in declarations {
        if declaration.id.trim().is_empty() {
            return Err(invalid("source declaration id must not be empty"));
        }
        if !safe_relative_path(declaration.relative_path) {
            return Err(invalid(format!(
                "source `{}` has unsafe path `{}`",
                declaration.id, declaration.relative_path
            )));
        }
        if !seen_paths.insert(declaration.relative_path) {
            return Err(invalid(format!(
                "duplicate source path `{}` in fixture",
                declaration.relative_path
            )));
        }
        let source = source_root.join(declaration.relative_path);
        let canonical_source = fs::canonicalize(source)?;
        if !canonical_source.starts_with(&canonical_root) {
            return Err(invalid(format!(
                "source `{}` escapes the declared source root",
                declaration.id
            )));
        }
        let (sha256, bytes) = sha256_file(&canonical_source)?;
        if sha256 != declaration.sha256 || bytes != declaration.bytes {
            return Err(invalid(format!(
                "source `{}` hash or size changed; regenerate the fixture",
                declaration.id
            )));
        }
        verified.push(VerifiedSource {
            id: declaration.id.to_string(),
            relative_path: declaration.relative_path.to_string(),
            sha256,
            bytes,
        });
    }
    if verified.is_empty() {
        return Err(invalid("fixture must declare at least one source file"));
    }
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn verification_rejects_changed_and_unsafe_sources() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("measurement.bin");
        fs::write(&source, b"trusted").unwrap();
        let (sha256, bytes) = sha256_file(&source).unwrap();
        let verified = verify_declared_sources(
            temp.path(),
            [SourceDeclaration {
                id: "measurement",
                relative_path: "measurement.bin",
                sha256: &sha256,
                bytes,
            }],
        )
        .unwrap();
        assert_eq!(verified[0].bytes, 7);

        fs::write(&source, b"changed").unwrap();
        assert!(verify_declared_sources(
            temp.path(),
            [SourceDeclaration {
                id: "measurement",
                relative_path: "measurement.bin",
                sha256: &sha256,
                bytes,
            }],
        )
        .is_err());
        assert!(verify_declared_sources(
            temp.path(),
            [SourceDeclaration {
                id: "escape",
                relative_path: "../measurement.bin",
                sha256: &sha256,
                bytes,
            }],
        )
        .is_err());
    }
}
