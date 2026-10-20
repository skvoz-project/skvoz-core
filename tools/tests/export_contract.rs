//! Verify the public supplier package against actual files and immutable outputs.
use std::{fs, path::PathBuf, process::Command};

struct Directory(PathBuf);
impl Directory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("skvoz-core-{name}-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn tool(args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_skvoz-core-tools"))
        .args(args)
        .output()
        .unwrap()
}
fn digest(path: &std::path::Path) -> String {
    let output = Command::new("sha256sum").arg(path).output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}
#[test]
fn sdk_contains_only_consumer_inputs_and_refuses_reuse() {
    let directory = Directory::new("sdk-contract");
    let sdk = directory.0.join("sdk");
    let output = tool(&["sdk".as_ref(), sdk.as_os_str()]);
    assert!(output.status.success(), "{output:?}");
    let manifest = fs::read_to_string(sdk.join("sdk.sha256")).unwrap();
    let paths: Vec<_> = manifest
        .lines()
        .map(|line| line.split_once("  ").unwrap().1)
        .collect();
    assert_eq!(paths.len(), 16);
    assert!(paths.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(paths.iter().all(|path| {
        *path == "LICENSE"
            || ["contracts/", "tls/", "binding/", "include/"]
                .iter()
                .any(|prefix| path.starts_with(prefix))
    }));
    let checked = Command::new("sha256sum")
        .args(["-c", "sdk.sha256"])
        .current_dir(&sdk)
        .output()
        .unwrap();
    assert!(checked.status.success(), "{checked:?}");
    let output = tool(&["sdk".as_ref(), sdk.as_os_str()]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("SDK destination already exists"));
    assert_eq!(
        fs::read_to_string(sdk.join("sdk.sha256")).unwrap(),
        manifest
    );
}
#[test]
#[ignore = "requires prepared GNU Linux and Android Core artifacts"]
fn package_preserves_bytes_and_identifies_current_source() {
    let directory = Directory::new("package-contract");
    let artifact = PathBuf::from(std::env::var_os("SKVOZ_CORE_ARTIFACT").unwrap());
    let library = artifact.join("lib/libskvoz_core.so");
    let packaged = directory.0.join("artifact");
    let output = tool(&[
        "package".as_ref(),
        library.as_os_str(),
        packaged.as_os_str(),
    ]);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(packaged.join("artifact.json")).unwrap()).unwrap();
    let tree = digest(&packaged.join("source-inputs.sha256"));
    assert_eq!(
        value["source"],
        serde_json::json!({
            "repository":"https://github.com/skvoz-project/skvoz-core",
            "revision":null,
            "tree_sha256":tree
        })
    );
    assert_eq!(value["source_inputs_sha256"], tree);
    assert_eq!(value["sha256"], digest(&library));
    assert_eq!(
        digest(&packaged.join("lib/libskvoz_core.so")),
        digest(&library)
    );
    assert_eq!(value["abi"], 1);
    assert_eq!(value["version"], "5.0.0");
    assert_eq!(value["target"], "x86_64-unknown-linux-gnu");
    assert_eq!(
        value["sdk_manifest_sha256"],
        digest(&packaged.join("sdk/sdk.sha256"))
    );
    let source_check = Command::new("sha256sum")
        .arg("-c")
        .arg(packaged.join("source-inputs.sha256"))
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .output()
        .unwrap();
    assert!(source_check.status.success(), "{source_check:?}");
    let output = tool(&[
        "package".as_ref(),
        library.as_os_str(),
        packaged.as_os_str(),
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("artifact destination already exists")
    );
    let android = PathBuf::from(std::env::var_os("SKVOZ_CORE_ANDROID_ARTIFACT").unwrap())
        .join("lib/libskvoz_core.so");
    let rejected = directory.0.join("rejected");
    let output = tool(&[
        "package".as_ref(),
        android.as_os_str(),
        rejected.as_os_str(),
    ]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported Core GNU libc profile"));
    assert!(!rejected.exists());
}
