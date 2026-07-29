use eqforbeginner_cli::{
    analyze_phase4_offline, analyze_sub_dataset, generate_example,
    generate_phase6_reference_package, prepare_phase6_measured_preview,
};
use std::path::PathBuf;

fn usage() -> &'static str {
    "Usage:\n  eqforbeginner-cli generate-example [--output DIRECTORY]\n  eqforbeginner-cli analyze-sub [--dataset FILE] [--source-root DIRECTORY] [--output DIRECTORY]\n  eqforbeginner-cli verify-48k-offline [--dataset FILE] [--source-root DIRECTORY] [--output DIRECTORY]\n  eqforbeginner-cli prepare-phase6-beta [--phase4-project FILE] [--design-csv FILE] [--phase4-wav FILE] [--output DIRECTORY]\n  eqforbeginner-cli generate-phase6-reference [--output DIRECTORY]"
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments
        .next()
        .unwrap_or_else(|| "generate-example".into());
    if command == "--help" || command == "-h" {
        println!("{}", usage());
        return Ok(());
    }
    match command.as_str() {
        "generate-example" => {
            let mut output = PathBuf::from("examples/phase1");
            while let Some(argument) = arguments.next() {
                match argument.as_str() {
                    "--output" => {
                        output = PathBuf::from(
                            arguments
                                .next()
                                .ok_or_else(|| "--output requires a directory".to_string())?,
                        );
                    }
                    "--help" | "-h" => {
                        println!("{}", usage());
                        return Ok(());
                    }
                    _ => return Err(format!("unknown argument `{argument}`\n{}", usage())),
                }
            }
            let generated = generate_example(&output).map_err(|error| error.to_string())?;
            println!("Phase 1 synthetic validation passed.");
            println!("Project: {}", generated.project_file.display());
            println!("48 kHz FIR: {}", generated.filter_wav.display());
            println!("Roon structural trial: {}", generated.roon_zip.display());
        }
        "analyze-sub" => {
            let mut dataset = PathBuf::from("measurments/derived/phase3-responses.json");
            let mut source_root = PathBuf::from("measurments");
            let mut output = PathBuf::from("examples/phase3-measured");
            while let Some(argument) = arguments.next() {
                let destination = match argument.as_str() {
                    "--dataset" => &mut dataset,
                    "--source-root" => &mut source_root,
                    "--output" => &mut output,
                    "--help" | "-h" => {
                        println!("{}", usage());
                        return Ok(());
                    }
                    _ => return Err(format!("unknown argument `{argument}`\n{}", usage())),
                };
                *destination = PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| format!("{argument} requires a path"))?,
                );
            }
            let generated = analyze_sub_dataset(&dataset, &source_root, &output)
                .map_err(|error| error.to_string())?;
            println!("Measured Phase 3 candidate ranking completed (provisional).");
            println!("Best measured candidate: {}", generated.best_candidate_id);
            println!("Confirmation measurement required: yes");
            println!("Report: {}", generated.ranking_json.display());
        }
        "verify-48k-offline" => {
            let mut dataset = PathBuf::from("measurments/derived/phase4-offline-measurements.json");
            let mut source_root = PathBuf::from("measurments/phase4");
            let mut output = PathBuf::from("examples/phase4-offline-measured");
            while let Some(argument) = arguments.next() {
                let destination = match argument.as_str() {
                    "--dataset" => &mut dataset,
                    "--source-root" => &mut source_root,
                    "--output" => &mut output,
                    "--help" | "-h" => {
                        println!("{}", usage());
                        return Ok(());
                    }
                    _ => return Err(format!("unknown argument `{argument}`\n{}", usage())),
                };
                *destination = PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| format!("{argument} requires a path"))?,
                );
            }
            let generated = analyze_phase4_offline(&dataset, &source_root, &output)
                .map_err(|error| error.to_string())?;
            println!("Phase 4 measured-response offline replay completed.");
            println!(
                "Numerical prediction gates passed: {}",
                generated.numerical_passed
            );
            println!("Verification state: {}", generated.verification_state);
            println!("Hardware verification: unverified");
            println!("Roon export eligible: no");
            println!("Project: {}", generated.project_file.display());
            println!("48 kHz trial FIR: {}", generated.filter_wav.display());
        }
        "prepare-phase6-beta" => {
            let mut phase4_project = PathBuf::from("examples/phase4-offline-measured/project.json");
            let mut design_csv =
                PathBuf::from("examples/phase4-offline-measured/filter-design.csv");
            let mut phase4_wav = PathBuf::from(
                "examples/phase4-offline-measured/filter/EQforBeginner_48000_Phase4_Trial.wav",
            );
            let mut output = PathBuf::from("examples/phase6-measured-preview");
            while let Some(argument) = arguments.next() {
                let destination = match argument.as_str() {
                    "--phase4-project" => &mut phase4_project,
                    "--design-csv" => &mut design_csv,
                    "--phase4-wav" => &mut phase4_wav,
                    "--output" => &mut output,
                    "--help" | "-h" => {
                        println!("{}", usage());
                        return Ok(());
                    }
                    _ => return Err(format!("unknown argument `{argument}`\n{}", usage())),
                };
                *destination = PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| format!("{argument} requires a path"))?,
                );
            }
            let generated =
                prepare_phase6_measured_preview(&phase4_project, &design_csv, &phase4_wav, &output)
                    .map_err(|error| error.to_string())?;
            println!("Phase 6 six-native-rate developer preview completed.");
            println!("Cross-rate gates passed: {}", generated.cross_rate_passed);
            println!("Verification state: {}", generated.verification_state);
            println!("Export eligible: no");
            println!("Roon ZIP created: no (real-path verification is absent)");
            println!("Project: {}", generated.project_file.display());
        }
        "generate-phase6-reference" => {
            let mut output = PathBuf::from("examples/phase6-synthetic-reference");
            while let Some(argument) = arguments.next() {
                match argument.as_str() {
                    "--output" => {
                        output = PathBuf::from(
                            arguments
                                .next()
                                .ok_or_else(|| "--output requires a directory".to_string())?,
                        );
                    }
                    "--help" | "-h" => {
                        println!("{}", usage());
                        return Ok(());
                    }
                    _ => return Err(format!("unknown argument `{argument}`\n{}", usage())),
                }
            }
            let generated =
                generate_phase6_reference_package(&output).map_err(|error| error.to_string())?;
            println!("Phase 6 synthetic structural reference completed.");
            println!("Cross-rate gates passed: {}", generated.cross_rate_passed);
            println!("Export eligible: no (synthetic reference only)");
            if let Some(zip) = generated.roon_zip {
                println!("Structural reference ZIP: {}", zip.display());
            }
        }
        _ => return Err(format!("unknown command `{command}`\n{}", usage())),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
