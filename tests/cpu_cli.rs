use std::{fs, process::Command};

use serde_json::Value;
use tempfile::tempdir;

use ntlmrain::gpu::{BackendChoice, DeviceCatalog};

fn candidate_fixture(path: &std::path::Path) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"NTLMCAN1");
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&881_688u64.to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&0x0088_46f7_eaee_8fb1u64.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

#[test]
fn forced_cpu_verify_is_machine_readable_and_records_metadata() {
    let temp = tempdir().unwrap();
    let candidates = temp.path().join("known.candidates");
    let artifacts = temp.path().join("artifacts");
    candidate_fixture(&candidates);

    let output = Command::new(env!("CARGO_BIN_EXE_ntlmrain"))
        .args([
            "--json",
            "--quiet",
            "--artifacts-dir",
            artifacts.to_str().unwrap(),
            "verify",
            "--compute",
            "cpu",
            "--cpu-threads",
            "2",
            "--des",
            "727B4E35F947129E",
            candidates.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "ok");
    assert!(result.get("keys").is_none());
    assert!(result["recovered"][0]["role"].is_null());
    assert_eq!(result["recovered"][0]["plaintext"], "8846F7EAEE8FB1");
    assert_eq!(result["recovered"][0]["des_key"], "8923BDFDAF753F63");

    let manifest_path =
        std::path::Path::new(result["artifact_dir"].as_str().unwrap()).join("manifest.json");
    let manifest: Value = serde_json::from_slice(&fs::read(manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["tool_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["compute"]["kind"], "cpu");
    assert_eq!(manifest["compute"]["threads"], 2);
    assert!(manifest["result"].get("keys").is_none());
    assert_eq!(manifest["result"]["recovered"], result["recovered"]);
    assert!(manifest.get("selected_device").is_none());
    assert!(manifest.get("tuning").is_none());
}

#[test]
fn incompatible_compute_options_fail_before_webgpu_creation() {
    let temp = tempdir().unwrap();
    let candidates = temp.path().join("known.candidates");
    candidate_fixture(&candidates);

    let webgpu_with_threads = Command::new(env!("CARGO_BIN_EXE_ntlmrain"))
        .args([
            "--quiet",
            "--artifacts-dir",
            temp.path().to_str().unwrap(),
            "verify",
            "--compute",
            "webgpu",
            "--cpu-threads",
            "2",
            "--des",
            "727B4E35F947129E",
            candidates.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(webgpu_with_threads.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&webgpu_with_threads.stderr)
            .contains("--cpu-threads is not used with --compute webgpu")
    );

    let cpu_with_shader = Command::new(env!("CARGO_BIN_EXE_ntlmrain"))
        .args([
            "--quiet",
            "--artifacts-dir",
            temp.path().to_str().unwrap(),
            "verify",
            "--compute",
            "cpu",
            "--shader",
            "compact",
            "--des",
            "727B4E35F947129E",
            candidates.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(cpu_with_shader.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&cpu_with_shader.stderr)
            .contains("--compute cpu cannot be combined with --shader")
    );
}

#[test]
fn auto_uses_native_cpu_when_catalog_has_no_hardware_gpu() {
    let catalog = DeviceCatalog::enumerate(BackendChoice::Auto).unwrap();
    let reports = catalog.reports();
    if reports.iter().any(|report| report.hardware_eligible) {
        return;
    }

    let temp = tempdir().unwrap();
    let candidates = temp.path().join("known.candidates");
    let artifacts = temp.path().join("artifacts");
    candidate_fixture(&candidates);
    let output = Command::new(env!("CARGO_BIN_EXE_ntlmrain"))
        .args([
            "--json",
            "--artifacts-dir",
            artifacts.to_str().unwrap(),
            "verify",
            "--des",
            "727B4E35F947129E",
            candidates.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[CPU] selected automatically: no hardware GPU detected"));
    assert!(stderr.contains("fast-des SIMD, 512-key bitslice"));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let manifest_path =
        std::path::Path::new(result["artifact_dir"].as_str().unwrap()).join("manifest.json");
    let manifest: Value = serde_json::from_slice(&fs::read(manifest_path).unwrap()).unwrap();
    assert_eq!(manifest["compute"]["kind"], "cpu");
    assert_eq!(manifest["compute"]["selection"], "auto-fallback");

    if let Some(software) = reports.iter().find(|report| !report.hardware_eligible) {
        let explicit_artifacts = temp.path().join("explicit-artifacts");
        let explicit = Command::new(env!("CARGO_BIN_EXE_ntlmrain"))
            .args([
                "--json",
                "--artifacts-dir",
                explicit_artifacts.to_str().unwrap(),
                "verify",
                "--device",
                &software.selector_id,
                "--des",
                "727B4E35F947129E",
                candidates.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(explicit.status.success());
        let stderr = String::from_utf8_lossy(&explicit.stderr);
        assert!(stderr.contains("is a CPU/software WebGPU adapter"));
        assert!(stderr.contains("use --compute webgpu to force"));
        let result: Value = serde_json::from_slice(&explicit.stdout).unwrap();
        let manifest_path =
            std::path::Path::new(result["artifact_dir"].as_str().unwrap()).join("manifest.json");
        let manifest: Value = serde_json::from_slice(&fs::read(manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["compute"]["kind"], "cpu");
        assert_eq!(manifest["compute"]["ignored_webgpu_adapter"], software.name);
    }
}
