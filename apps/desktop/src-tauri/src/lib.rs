mod developer_beta;
mod live_measurement;
mod wireless_sweep;

use std::sync::Arc;
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
fn scan_input_devices() -> Result<eqforbeginner_audio_io::AudioDeviceInventory, String> {
    eqforbeginner_audio_io::scan_default_host_inputs().map_err(|error| error.to_string())
}

#[tauri::command]
fn import_wireless_sweep(
    request: tauri::ipc::Request<'_>,
    state: tauri::State<'_, wireless_sweep::WirelessSweepState>,
) -> Result<wireless_sweep::WirelessSweepImport, String> {
    import_wireless_sweep_body(request.body(), &state)
}

fn import_wireless_sweep_body(
    body: &tauri::ipc::InvokeBody,
    state: &wireless_sweep::WirelessSweepState,
) -> Result<wireless_sweep::WirelessSweepImport, String> {
    match body {
        tauri::ipc::InvokeBody::Raw(bytes) => state.import_reference(bytes),
        // Tauri normally transports a top-level ArrayBuffer as a raw body.
        // If its custom-protocol transport is unavailable, the postMessage
        // fallback serializes that same payload as a JSON byte array.
        tauri::ipc::InvokeBody::Json(value) => {
            let values = value.as_array().ok_or_else(|| {
                "wireless sweep import expected WAV bytes, but the IPC fallback payload was not a byte array"
                    .to_string()
            })?;
            if values.len() > wireless_sweep::MAX_REFERENCE_WAV_BYTES {
                return Err(format!(
                    "the selected WAV exceeds the {} MiB safety limit",
                    wireless_sweep::MAX_REFERENCE_WAV_BYTES / (1024 * 1024)
                ));
            }
            let mut bytes = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let byte = value
                    .as_u64()
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or_else(|| {
                        format!(
                            "wireless sweep IPC byte {index} is not an integer between 0 and 255"
                        )
                    })?;
                bytes.push(byte);
            }
            state.import_reference(&bytes)
        }
    }
}

#[tauri::command]
async fn capture_wireless_sweep(
    sweep_id: String,
    input_device_id: String,
    input_channel_index: u16,
    wait_seconds: u64,
    state: tauri::State<'_, wireless_sweep::WirelessSweepState>,
) -> Result<wireless_sweep::WirelessSweepCaptureResult, String> {
    if !(5..=60).contains(&wait_seconds) {
        return Err("wireless sweep waitSeconds must be between 5 and 60".to_string());
    }
    let (reference, cancellation) = state.begin_capture(&sweep_id)?;
    let reference_duration_ms = u128::try_from(reference.metadata.frame_count)
        .ok()
        .and_then(|frames| frames.checked_mul(1_000))
        .and_then(|duration| {
            duration.checked_add(u128::from(reference.metadata.sample_rate_hz) - 1)
        })
        .map(|duration| duration / u128::from(reference.metadata.sample_rate_hz));
    let duration_ms = reference_duration_ms
        .and_then(|reference_duration| {
            u128::from(wait_seconds)
                .checked_mul(1_000)
                .and_then(|duration| duration.checked_add(reference_duration))
        })
        .and_then(|duration| duration.checked_add(1_000))
        .and_then(|duration| u64::try_from(duration).ok());

    let result = if let Some(duration_ms) = duration_ms {
        let maximum_samples = u128::from(duration_ms)
            .checked_mul(u128::from(eqforbeginner_audio_io::PROJECT_SAMPLE_RATE_HZ))
            .and_then(|samples| samples.checked_add(999))
            .and_then(|samples| usize::try_from(samples / 1_000).ok());
        if let Some(maximum_samples) = maximum_samples {
            let request = eqforbeginner_audio_io::MonoInputCaptureRequest {
                device_id: input_device_id,
                input_channel_index,
                duration_ms,
                maximum_samples,
            };
            // A worker panic must not early-return before `finish_capture`
            // below, or `active_capture` stays set until app restart and
            // every later wireless capture fails as "already running"
            // (2026-07-28 release review). The live path already awaits
            // without `?` for the same reason.
            match tauri::async_runtime::spawn_blocking(move || {
                let capture = eqforbeginner_audio_io::capture_mono_input_with_cancellation(
                    &request,
                    &cancellation,
                )
                .map_err(|error| error.to_string())?;
                wireless_sweep::analyze_capture(&reference, &capture)
            })
            .await
            {
                Ok(analysis) => analysis,
                Err(error) => Err(format!("wireless capture worker failed: {error}")),
            }
        } else {
            Err("wireless capture sample count overflowed".to_string())
        }
    } else {
        Err("wireless capture duration overflowed".to_string())
    };
    state.finish_capture();
    result
}

#[tauri::command]
fn cancel_wireless_sweep_capture(
    state: tauri::State<'_, wireless_sweep::WirelessSweepState>,
) -> Result<bool, String> {
    state.cancel_capture()
}

#[tauri::command]
fn start_live_measurement_session(
    system_mode: live_measurement::LiveSystemMode,
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveSessionSummary, String> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("could not resolve the app-data directory: {error}"))?
        .join("live-projects");
    state.start_session_with_mode(&base, system_mode)
}

#[tauri::command]
fn record_live_subwoofer_setup(
    setup: live_measurement::LiveSubwooferSetupRequest,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveSubwooferSetupSummary, String> {
    state.record_subwoofer_setup(setup)
}

#[tauri::command]
fn configure_live_subwoofer_search(
    search: live_measurement::LiveSubwooferSearchRequest,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveSubwooferSearchSummary, String> {
    state.configure_subwoofer_search(search)
}

#[tauri::command]
async fn optimize_live_subwoofer_paths(
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveSubwooferOptimizationSummary, String> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || state.optimize_subwoofer_paths())
        .await
        .map_err(|error| format!("subwoofer optimization worker failed: {error}"))?
}

#[tauri::command]
fn import_live_microphone_calibration(
    file_name: String,
    contents: String,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::CalibrationImportSummary, String> {
    state.import_calibration(&file_name, &contents)
}

#[tauri::command]
fn import_live_target_curve(
    file_name: String,
    contents: String,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::TargetImportSummary, String> {
    state.import_target(&file_name, &contents)
}

fn invoke_body_bytes(
    body: &tauri::ipc::InvokeBody,
    maximum_bytes: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    match body {
        tauri::ipc::InvokeBody::Raw(bytes) => {
            if bytes.len() > maximum_bytes {
                return Err(format!("{label} exceeds the bounded IPC size"));
            }
            Ok(bytes.clone())
        }
        tauri::ipc::InvokeBody::Json(value) => {
            let values = value
                .as_array()
                .ok_or_else(|| format!("{label} IPC fallback was not a byte array"))?;
            if values.len() > maximum_bytes {
                return Err(format!("{label} exceeds the bounded IPC size"));
            }
            values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    value
                        .as_u64()
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or_else(|| {
                            format!("{label} IPC byte {index} is not an integer from 0 to 255")
                        })
                })
                .collect()
        }
    }
}

fn import_live_sweep_body(
    body: &tauri::ipc::InvokeBody,
    channel: live_measurement::LiveChannel,
    state: &live_measurement::LiveMeasurementState,
) -> Result<live_measurement::LiveSweepImportSummary, String> {
    let bytes = invoke_body_bytes(
        body,
        live_measurement::MAX_LIVE_SWEEP_BYTES,
        "live measurement sweep",
    )?;
    state.import_sweep(channel, &bytes)
}

#[tauri::command]
fn import_live_left_sweep(
    request: tauri::ipc::Request<'_>,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveSweepImportSummary, String> {
    import_live_sweep_body(request.body(), live_measurement::LiveChannel::Left, &state)
}

#[tauri::command]
fn import_live_right_sweep(
    request: tauri::ipc::Request<'_>,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveSweepImportSummary, String> {
    import_live_sweep_body(request.body(), live_measurement::LiveChannel::Right, &state)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri keeps each audited IPC field explicit at the boundary.
async fn capture_live_measurement(
    kind: live_measurement::LiveCaptureKind,
    channel: live_measurement::LiveChannel,
    position_id: String,
    input_device_id: String,
    input_channel_index: u16,
    wait_seconds: u64,
    on_progress: tauri::ipc::Channel<live_measurement::LiveCaptureProgress>,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveCaptureSummary, String> {
    let state = Arc::clone(&state);
    let (position_id, _root, sweep, calibration, evidence, cancellation) = state.begin_capture(
        kind,
        channel,
        &position_id,
        &input_device_id,
        input_channel_index,
    )?;
    let request = match live_measurement::live_capture_request(
        input_device_id,
        input_channel_index,
        sweep.source_frame_count,
        wait_seconds,
    ) {
        Ok(request) => request,
        Err(error) => {
            state.finish_capture();
            return Err(error);
        }
    };
    let mut monitor =
        live_measurement::LiveCaptureMonitor::new(&sweep, calibration.sensitivity_factor_db);
    let _ = on_progress.send(monitor.initial_progress());
    let maximum_monitor_snapshot_samples = monitor.maximum_snapshot_samples();
    let worker_state = Arc::clone(&state);
    let worker = tauri::async_runtime::spawn_blocking(move || {
        let mut automatic_completion = |snapshot_start_sample: usize, samples: &[f32]| {
            let update = monitor.inspect(snapshot_start_sample, samples);
            let _ = on_progress.send(update.progress);
            update.should_complete
        };
        let capture = eqforbeginner_audio_io::capture_mono_input_with_automatic_completion(
            &request,
            &cancellation,
            maximum_monitor_snapshot_samples,
            &mut automatic_completion,
        )
        .map_err(|error| error.to_string())?;
        live_measurement::analyze_and_store_capture(
            &worker_state,
            kind,
            channel,
            position_id,
            &sweep,
            &calibration,
            &evidence,
            &capture,
        )
    })
    .await;
    state.finish_capture();
    worker.map_err(|error| format!("live measurement worker failed: {error}"))?
}

#[tauri::command]
fn cancel_live_measurement_capture(
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<bool, String> {
    state.cancel_capture()
}

#[tauri::command]
fn restore_live_accepted_measurements(
    input_device_id: String,
    input_channel_index: u16,
    scope: live_measurement::LiveRestoreScope,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveMeasurementCacheRestoreSummary, String> {
    state.restore_accepted_measurements(&input_device_id, input_channel_index, scope)
}

#[tauri::command]
fn set_live_trial_activation(
    active_in_roon: bool,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<bool, String> {
    state.set_trial_activation(active_in_roon)
}

#[tauri::command]
async fn design_live_trial_filter(
    target: String,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveDesignSummary, String> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || state.design_trial(&target))
        .await
        .map_err(|error| format!("live Phase 4 design worker failed: {error}"))?
}

#[tauri::command]
async fn validate_live_closed_loop(
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveVerificationSummary, String> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || state.verification_summary())
        .await
        .map_err(|error| format!("live verification worker failed: {error}"))?
}

#[tauri::command]
async fn export_live_roon_convolution(
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<live_measurement::LiveExportSummary, String> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || state.finalize_export())
        .await
        .map_err(|error| format!("live export worker failed: {error}"))?
}

#[tauri::command]
async fn download_live_roon_zip(
    artifact_kind: live_measurement::LiveZipArtifactKind,
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<live_measurement::LiveMeasurementState>>,
) -> Result<Option<live_measurement::LiveZipDownloadSummary>, String> {
    let file_name = state.zip_download_file_name(artifact_kind)?;
    let Some(selected) = app
        .dialog()
        .file()
        .set_title("Save EQforBeginner Roon ZIP")
        .set_file_name(file_name)
        .add_filter("ZIP archive", &["zip"])
        .blocking_save_file()
    else {
        return Ok(None);
    };
    let mut destination = selected
        .into_path()
        .map_err(|error| format!("the selected ZIP destination is not a local file: {error}"))?;
    if destination.extension().is_none() {
        destination.set_extension("zip");
    }
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        state.save_zip_artifact(artifact_kind, &destination)
    })
    .await
    .map_err(|error| format!("ZIP download worker failed: {error}"))?
    .map(Some)
}

#[tauri::command]
fn developer_beta_preflight(
    workspace_root: Option<String>,
) -> Result<developer_beta::DeveloperBetaPreflight, String> {
    developer_beta::preflight(workspace_root)
}

#[tauri::command]
async fn run_developer_beta_stage(
    stage: String,
    workspace_root: Option<String>,
    run_id: String,
) -> Result<developer_beta::DeveloperStageResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        developer_beta::run_stage(stage, workspace_root, run_id)
    })
    .await
    .map_err(|error| format!("developer beta worker failed: {error}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(wireless_sweep::WirelessSweepState::default())
        .manage(Arc::new(live_measurement::LiveMeasurementState::default()))
        .invoke_handler(tauri::generate_handler![
            scan_input_devices,
            import_wireless_sweep,
            capture_wireless_sweep,
            cancel_wireless_sweep_capture,
            start_live_measurement_session,
            configure_live_subwoofer_search,
            optimize_live_subwoofer_paths,
            record_live_subwoofer_setup,
            import_live_microphone_calibration,
            import_live_target_curve,
            import_live_left_sweep,
            import_live_right_sweep,
            capture_live_measurement,
            cancel_live_measurement_capture,
            restore_live_accepted_measurements,
            set_live_trial_activation,
            design_live_trial_filter,
            validate_live_closed_loop,
            export_live_roon_convolution,
            download_live_roon_zip,
            developer_beta_preflight,
            run_developer_beta_stage
        ])
        .run(tauri::generate_context!())
        .expect("failed to run EQforBeginner desktop shell");
}

#[cfg(test)]
mod tests {
    use super::*;
    use hound::{SampleFormat, WavSpec, WavWriter};
    use std::io::Cursor;

    fn one_second_wav() -> Vec<u8> {
        let spec = WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec).expect("create WAV");
            for frame in 0..48_000 {
                let sample = if frame % 48 < 24 {
                    4_000_i16
                } else {
                    -4_000_i16
                };
                writer.write_sample(sample).expect("write sample");
            }
            writer.finalize().expect("finalize WAV");
        }
        cursor.into_inner()
    }

    #[test]
    fn imports_a_json_byte_array_from_the_tauri_ipc_fallback() {
        let bytes = one_second_wav();
        let body = tauri::ipc::InvokeBody::Json(bytes.into());
        let state = wireless_sweep::WirelessSweepState::default();

        let imported =
            import_wireless_sweep_body(&body, &state).expect("import JSON fallback body");

        assert_eq!(imported.sample_rate_hz, 48_000);
        assert_eq!(imported.frame_count, 48_000);
    }

    #[test]
    fn rejects_a_non_byte_json_fallback_value_with_its_index() {
        let body = tauri::ipc::InvokeBody::Json(vec![82_i64, -1].into());
        let state = wireless_sweep::WirelessSweepState::default();

        let error = import_wireless_sweep_body(&body, &state).expect_err("must reject non-byte");

        assert!(error.contains("byte 1"));
    }
}
