use eqforbeginner_cli::{
    analyze_phase4_offline, analyze_sub_dataset, prepare_phase6_measured_preview,
};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const EXPORT_BLOCK_REASON: &str = "Final Roon export is blocked: the measured project has no post-FIR playback-path remeasurement and export_eligible is false.";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeveloperStagePreflight {
    id: String,
    available: bool,
    detail: String,
    required_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeveloperBetaPreflight {
    workspace_root: String,
    measurement_runtime_connected: bool,
    stages: Vec<DeveloperStagePreflight>,
    export_eligible: bool,
    export_block_reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeveloperStageResult {
    id: String,
    output_directory: String,
    verification_state: String,
    artifacts: Vec<String>,
    logs: Vec<String>,
    numerical_passed: Option<bool>,
    export_eligible: bool,
    export_block_reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeveloperStage {
    Phase3,
    Phase4,
    Phase6,
}

impl DeveloperStage {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "phase3" => Ok(Self::Phase3),
            "phase4" => Ok(Self::Phase4),
            "phase6" => Ok(Self::Phase6),
            _ => Err(format!("unknown developer beta stage `{value}`")),
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::Phase3 => "phase3",
            Self::Phase4 => "phase4",
            Self::Phase6 => "phase6",
        }
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn default_workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn resolve_workspace_root(requested: Option<String>) -> Result<PathBuf, String> {
    let requested = requested
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_workspace_root);
    let root = requested.canonicalize().map_err(|error| {
        format!(
            "cannot resolve workspace `{}`: {error}",
            requested.display()
        )
    })?;
    for marker in [
        "Cargo.toml",
        "apps/desktop/package.json",
        "crates/dsp-cli/Cargo.toml",
    ] {
        let path = root.join(marker);
        if !path.is_file() {
            return Err(format!(
                "`{}` is not a EQforBeginner workspace: missing `{marker}`",
                root.display()
            ));
        }
    }
    Ok(root)
}

fn stage_requirements(root: &Path, stage: DeveloperStage) -> Vec<PathBuf> {
    match stage {
        DeveloperStage::Phase3 => vec![
            root.join("measurments/derived/phase3-responses.json"),
            root.join("measurments"),
        ],
        DeveloperStage::Phase4 => vec![
            root.join("measurments/derived/phase4-offline-measurements.json"),
            root.join("measurments/phase4"),
        ],
        DeveloperStage::Phase6 => vec![
            root.join("measurments/derived/phase4-offline-measurements.json"),
            root.join("measurments/phase4"),
        ],
    }
}

fn requirement_exists(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

pub fn preflight(workspace_root: Option<String>) -> Result<DeveloperBetaPreflight, String> {
    let root = resolve_workspace_root(workspace_root)?;
    let stages = [
        DeveloperStage::Phase3,
        DeveloperStage::Phase4,
        DeveloperStage::Phase6,
    ]
    .into_iter()
    .map(|stage| {
        let required = stage_requirements(&root, stage);
        let missing = required
            .iter()
            .filter(|path| !requirement_exists(path))
            .map(|path| path_string(path))
            .collect::<Vec<_>>();
        let available = missing.is_empty();
        let detail = if available {
            match stage {
                DeveloperStage::Phase3 => "Measured-response fixture and source MDAT root found",
                DeveloperStage::Phase4 => "48 kHz measured-response fixture found; replay remains predicted-only",
                DeveloperStage::Phase6 => "Phase 4 evidence found for native-rate measured preview; final ZIP remains gated",
            }
            .to_string()
        } else {
            format!("missing: {}", missing.join(", "))
        };
        DeveloperStagePreflight {
            id: stage.id().to_string(),
            available,
            detail,
            required_paths: required.iter().map(|path| path_string(path)).collect(),
        }
    })
    .collect();

    Ok(DeveloperBetaPreflight {
        workspace_root: path_string(&root),
        measurement_runtime_connected: false,
        stages,
        export_eligible: false,
        export_block_reason: EXPORT_BLOCK_REASON.to_string(),
    })
}

fn validate_run_id(run_id: &str) -> Result<(), String> {
    if run_id.is_empty()
        || run_id.len() > 80
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("runId must contain only ASCII letters, digits, `-`, or `_` and be at most 80 characters".into());
    }
    Ok(())
}

fn ensure_directory_not_symlink(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "refusing symlinked developer output directory `{}`",
            path.display()
        )),
        Ok(metadata) if !metadata.is_dir() => Err(format!(
            "developer output path `{}` is not a directory",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| {
                format!(
                    "cannot create developer output directory `{}`: {error}",
                    path.display()
                )
            })
        }
        Err(error) => Err(format!(
            "cannot inspect developer output directory `{}`: {error}",
            path.display()
        )),
    }
}

fn output_for(root: &Path, run_id: &str, stage: DeveloperStage) -> Result<PathBuf, String> {
    validate_run_id(run_id)?;
    let beta_root = root.join(".eqforbeginner-beta");
    ensure_directory_not_symlink(&beta_root)?;
    let runs_root = beta_root.join("runs");
    ensure_directory_not_symlink(&runs_root)?;
    let run_root = runs_root.join(run_id);
    ensure_directory_not_symlink(&run_root)?;
    Ok(run_root.join(stage.id()))
}

fn ensure_stage_available(root: &Path, stage: DeveloperStage) -> Result<(), String> {
    let missing = stage_requirements(root, stage)
        .into_iter()
        .filter(|path| !requirement_exists(path))
        .map(|path| path_string(&path))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "stage input is missing or symlinked: {}",
            missing.join(", ")
        ))
    }
}

fn run_phase3(root: &Path, output: &Path) -> Result<DeveloperStageResult, String> {
    let dataset = root.join("measurments/derived/phase3-responses.json");
    let source_root = root.join("measurments");
    let generated = analyze_sub_dataset(&dataset, &source_root, output)
        .map_err(|error| format!("Phase 3 failed: {error}"))?;
    Ok(DeveloperStageResult {
        id: "phase3".into(),
        output_directory: path_string(&generated.output_directory),
        verification_state: "provisional-measured-candidate-ranking".into(),
        artifacts: vec![
            path_string(&generated.ranking_json),
            path_string(&generated.ranking_csv),
            path_string(&generated.readme),
        ],
        logs: vec![
            "Measured candidate ranking completed from the checked fixture.".into(),
            format!("Best candidate: {}", generated.best_candidate_id),
            "Hardware settings were not changed by the app; confirmation measurement is still required.".into(),
        ],
        numerical_passed: None,
        export_eligible: false,
        export_block_reason: EXPORT_BLOCK_REASON.into(),
    })
}

fn run_phase4(root: &Path, output: &Path) -> Result<DeveloperStageResult, String> {
    let dataset = root.join("measurments/derived/phase4-offline-measurements.json");
    let source_root = root.join("measurments/phase4");
    let generated = analyze_phase4_offline(&dataset, &source_root, output)
        .map_err(|error| format!("Phase 4 failed: {error}"))?;
    Ok(DeveloperStageResult {
        id: "phase4".into(),
        output_directory: path_string(&generated.output_directory),
        verification_state: generated.verification_state,
        artifacts: vec![
            path_string(&generated.project_file),
            path_string(&generated.filter_wav),
            path_string(&generated.readme),
        ],
        logs: vec![
            format!(
                "Numerical prediction gates passed: {}",
                generated.numerical_passed
            ),
            "Verification state is predicted-only; hardware verification remains unverified."
                .into(),
            "Roon export eligibility remains false.".into(),
        ],
        numerical_passed: Some(generated.numerical_passed),
        export_eligible: false,
        export_block_reason: EXPORT_BLOCK_REASON.into(),
    })
}

fn run_phase6(output: &Path, run_output: &Path) -> Result<DeveloperStageResult, String> {
    let phase4_project = run_output.join("phase4/project.json");
    let design_csv = run_output.join("phase4/filter-design.csv");
    let phase4_wav = run_output.join("phase4/filter/EQforBeginner_48000_Phase4_Trial.wav");
    for prerequisite in [&phase4_project, &design_csv, &phase4_wav] {
        if !prerequisite.is_file() {
            return Err(format!(
                "Phase 6 requires the Phase 4 result from this run: `{}`",
                prerequisite.display()
            ));
        }
    }
    let generated =
        prepare_phase6_measured_preview(&phase4_project, &design_csv, &phase4_wav, output)
            .map_err(|error| format!("Phase 6 failed: {error}"))?;
    if generated.export_eligible || generated.roon_zip.is_some() {
        return Err(
            "measured preview violated the beta safety boundary by enabling export or creating a ZIP"
                .into(),
        );
    }
    let mut artifacts = vec![
        path_string(&generated.project_file),
        path_string(&generated.readme),
    ];
    artifacts.extend(generated.filter_wavs.iter().map(|path| path_string(path)));
    Ok(DeveloperStageResult {
        id: "phase6".into(),
        output_directory: path_string(&generated.output_directory),
        verification_state: generated.verification_state,
        artifacts,
        logs: vec![
            format!("Native-rate WAV count: {}", generated.filter_wavs.len()),
            format!(
                "Cross-rate numerical gates passed: {}",
                generated.cross_rate_passed
            ),
            "Measured preview created no Roon ZIP because export_eligible is false.".into(),
        ],
        numerical_passed: Some(generated.cross_rate_passed),
        export_eligible: generated.export_eligible,
        export_block_reason: EXPORT_BLOCK_REASON.into(),
    })
}

pub fn run_stage(
    stage: String,
    workspace_root: Option<String>,
    run_id: String,
) -> Result<DeveloperStageResult, String> {
    let stage = DeveloperStage::parse(&stage)?;
    let root = resolve_workspace_root(workspace_root)?;
    ensure_stage_available(&root, stage)?;
    let output = output_for(&root, &run_id, stage)?;
    let run_output = output
        .parent()
        .ok_or_else(|| "developer stage output has no run directory".to_string())?;
    match stage {
        DeveloperStage::Phase3 => run_phase3(&root, &output),
        DeveloperStage::Phase4 => run_phase4(&root, &output),
        DeveloperStage::Phase6 => run_phase6(&output, run_output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_rejects_path_traversal() {
        assert!(validate_run_id("run-2026-07-21").is_ok());
        assert!(validate_run_id("../outside").is_err());
        assert!(validate_run_id("nested/run").is_err());
        assert!(validate_run_id("").is_err());
    }

    #[test]
    fn compiled_workspace_preflight_is_explicitly_offline() {
        let report = preflight(None).expect("workspace preflight");
        assert!(!report.measurement_runtime_connected);
        assert!(!report.export_eligible);
        assert_eq!(report.stages.len(), 3);
        assert!(report.export_block_reason.contains("post-FIR"));
    }

    #[test]
    #[ignore = "runs the complete measured-fixture developer pipeline"]
    fn full_fixture_pipeline_preserves_export_lock() {
        let root = resolve_workspace_root(None).expect("workspace");
        let output = tempfile::tempdir().expect("temporary output");
        let phase3 = run_phase3(&root, &output.path().join("phase3")).expect("Phase 3");
        let phase4 = run_phase4(&root, &output.path().join("phase4")).expect("Phase 4");
        let phase6 = run_phase6(&output.path().join("phase6"), output.path()).expect("Phase 6");

        assert!(!phase3.export_eligible);
        assert!(phase4.numerical_passed.unwrap_or(false));
        assert!(!phase4.export_eligible);
        assert_eq!(phase6.artifacts.len(), 8);
        assert!(phase6.numerical_passed.unwrap_or(false));
        assert!(!phase6.export_eligible);
        assert!(!phase6.artifacts.iter().any(|path| path.ends_with(".zip")));
    }
}
