//! Exercise the actual consumer build script against missing or mismatched artifacts.
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::Command,
};
#[test]
#[ignore = "requires a prepared artifact and the compiled binding build script"]
fn consumer_rejects_missing_digest_abi_and_target_without_source_fallback() {
    let script = PathBuf::from(
        std::env::var_os("SKVOZ_CORE_BUILD_SCRIPT")
            .expect("compiled binding build script required"),
    );
    let artifact =
        PathBuf::from(std::env::var_os("SKVOZ_CORE_ARTIFACT").expect("prepared artifact required"));
    let directory = std::env::temp_dir().join(format!(
        "skvoz-core-artifact-negative-{}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let run = |location: Option<&std::path::Path>, message: &str| {
        let mut command = Command::new(&script);
        command
            .env("TARGET", "x86_64-unknown-linux-gnu")
            .env_remove("SKVOZ_CORE_ARTIFACT");
        if let Some(location) = location {
            command.env("SKVOZ_CORE_ARTIFACT", location);
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "unexpected artifact failure: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(None, "source fallback is forbidden");
    run(Some(&directory), "Core artifact manifest missing");
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(artifact.join("artifact.json")).unwrap()).unwrap();
    fs::create_dir(directory.join("lib")).unwrap();
    symlink(
        artifact.join("lib/libskvoz_core.so"),
        directory.join("lib/libskvoz_core.so"),
    )
    .unwrap();
    for (field, value, message) in [
        ("abi", serde_json::json!(999), "unsupported Core ABI"),
        (
            "target",
            serde_json::json!("unsupported-target"),
            "Core target mismatch",
        ),
        (
            "sha256",
            serde_json::json!("0".repeat(64)),
            "Core artifact digest mismatch",
        ),
    ] {
        let mut manifest = original.clone();
        manifest[field] = value;
        fs::write(
            directory.join("artifact.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        run(Some(&directory), message);
    }
}
