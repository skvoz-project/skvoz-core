//! Verify the public supplier package against actual files and immutable outputs.
use std::{fs, path::PathBuf, process::Command};

#[cfg(unix)]
#[test]
fn android_packaging_accepts_cargo_scheduling_choices() {
    use std::os::unix::fs::PermissionsExt;

    // Mock archive/tool transport: this checks packaging policy, not native ELF bytes.
    let directory = Directory::new("android-scheduling");
    let pins: serde_json::Value =
        serde_json::from_str(include_str!("../../../docker/android-profiles.json")).unwrap();
    let bin = directory.0.join("bin");
    let ndk = directory.0.join("ndk");
    let ndk_bin = ndk.join("toolchains/llvm/prebuilt/linux-x86_64/bin");
    let sysroot = directory.0.join("sysroot");
    fs::create_dir(&bin).unwrap();
    fs::create_dir_all(&ndk_bin).unwrap();
    fs::write(
        ndk.join("source.properties"),
        "Pkg.Revision = 30.0.16248370\n",
    )
    .unwrap();
    fs::write(ndk_bin.join("clang-21"), "compiler fixture").unwrap();
    fs::write(ndk_bin.join("llvm-ar"), "archiver fixture").unwrap();
    let ndk_archive = directory.0.join("ndk.zip");
    fs::File::create(&ndk_archive)
        .unwrap()
        .set_len(pins["ndk_archive_bytes"].as_u64().unwrap())
        .unwrap();
    let executable = |path: &std::path::Path, body: &str| {
        fs::write(path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    executable(
        &bin.join("sha1sum"),
        &format!("echo {}", pins["ndk_archive_sha1"].as_str().unwrap()),
    );
    executable(
        &bin.join("sha256sum"),
        "case $1 in *.tar.xz) echo \"$TEST_STD_SHA256\";; */ndk.zip) echo 0000000000000000000000000000000000000000000000000000000000000000;; *) exec /usr/bin/sha256sum \"$@\";; esac",
    );
    executable(&bin.join("rustc"), "echo 'rustc 1.92.0 (fixture)'");
    executable(&bin.join("cargo"), "echo 'cargo 1.92.0 (fixture)'");
    executable(
        &bin.join("readelf"),
        "case $1 in
          -h) printf 'Class: ELF64\nMachine: %s\n' \"$TEST_MACHINE\";;
          -d) echo '(NEEDED) Shared library: [libc.so]' ;;
          --version-info|--notes) :;;
          -lW) echo 'LOAD 0x0 0x0 0x0 0x100 0x100 R 0x4000';;
          --dyn-syms) for name in version capabilities buffer_free runtime_new runtime_free start poll operation_free call events; do
            echo \"1: 0 1 FUNC GLOBAL DEFAULT 1 skvoz_core_v1_$name\"
          done;;
          *) exit 1;;
        esac",
    );
    let library = directory.0.join("libskvoz_core.so");
    fs::write(&library, "library fixture").unwrap();
    for target in ["aarch64-linux-android", "x86_64-linux-android"] {
        let profile = &pins["targets"][target];
        let target_std = sysroot.join("lib/rustlib").join(target).join("lib");
        fs::create_dir_all(&target_std).unwrap();
        fs::write(target_std.join("libstd.rlib"), "target std fixture").unwrap();
        let std_archive = directory.0.join(format!("{target}.tar.xz"));
        fs::File::create(&std_archive)
            .unwrap()
            .set_len(profile["rust_std_bytes"].as_u64().unwrap())
            .unwrap();
        let clang = ndk_bin.join(profile["clang"].as_str().unwrap());
        executable(&clang, "echo 'Android clang fixture'");
        let target_env = target.replace('-', "_");
        let command = |destination: &std::path::Path| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_skvoz-core-tools"));
            command
                .args(["package-android", target])
                .arg(&library)
                .arg(destination)
                .env(
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                )
                .env("SKVOZ_CORE_ANDROID_NDK", &ndk)
                .env("SKVOZ_CORE_ANDROID_SYSROOT", &sysroot)
                .env("SKVOZ_CORE_ANDROID_NDK_ARCHIVE", &ndk_archive)
                .env("SKVOZ_CORE_ANDROID_STD_ARCHIVE", &std_archive)
                .env(
                    "RUSTFLAGS",
                    format!(
                        "-C link-arg=-Wl,-z,max-page-size=16384 --sysroot {}",
                        sysroot.display()
                    ),
                )
                .env(
                    format!("CARGO_TARGET_{}_LINKER", target_env.to_uppercase()),
                    &clang,
                )
                .env(format!("CC_{target_env}"), &clang)
                .env(format!("AR_{target_env}"), ndk_bin.join("llvm-ar"))
                .env("CARGO_INCREMENTAL", "0")
                .env("CARGO_PROFILE_RELEASE_DEBUG", "0")
                .env("TEST_MACHINE", profile["elf_machine"].as_str().unwrap())
                .env(
                    "TEST_STD_SHA256",
                    profile["rust_std_sha256"].as_str().unwrap(),
                )
                .env_remove("CARGO_BUILD_JOBS")
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .env_remove("SKVOZ_CORE_SOURCE_REVISION");
            command
        };
        for jobs in [None, Some("4")] {
            let destination = directory
                .0
                .join(format!("{target}-{}", jobs.unwrap_or("default")));
            let mut command = command(&destination);
            if let Some(jobs) = jobs {
                command.env("CARGO_BUILD_JOBS", jobs);
            }
            let result = command.output().unwrap();
            assert!(
                result.status.success(),
                "{target}, jobs={jobs:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(destination.join("artifact.json")).unwrap())
                    .unwrap();
            assert_eq!(manifest["target"], target);
            assert!(
                manifest["android"]["build_environment"]
                    .get("CARGO_BUILD_JOBS")
                    .is_none()
            );
            assert_eq!(
                fs::read(destination.join("lib/libskvoz_core.so")).unwrap(),
                fs::read(&library).unwrap()
            );
        }
        for name in [
            "RUSTFLAGS",
            "CARGO_INCREMENTAL",
            "CARGO_PROFILE_RELEASE_DEBUG",
        ] {
            let destination = directory.0.join(format!("{target}-invalid-{name}"));
            let result = command(&destination).env(name, "invalid").output().unwrap();
            assert!(!result.status.success(), "{name} was ignored");
            assert!(
                String::from_utf8_lossy(&result.stderr).contains(name),
                "{result:?}"
            );
            assert!(!destination.exists());
        }
    }
}

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
    assert_eq!(paths.len(), 18);
    for path in [
        "contracts/tests/unit/codec.rs",
        "contracts/tests/unit/diagnostics.rs",
    ] {
        assert!(paths.contains(&path));
    }
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
    for package in ["contracts", "tls", "binding"] {
        let exported = sdk.join(package).join("Cargo.toml");
        assert!(!fs::read_to_string(&exported).unwrap().contains("workspace"));
        let metadata = Command::new("cargo")
            .args([
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--locked",
                "--offline",
                "--manifest-path",
            ])
            .arg(exported)
            .output()
            .unwrap();
        assert!(metadata.status.success(), "{package}: {metadata:?}");
        let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
        assert!(
            metadata["packages"][0]["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|target| target["kind"][0] != "example")
        );
        assert_eq!(
            metadata["packages"][0]["targets"][0]["src_path"],
            sdk.join(package).join("lib.rs").to_str().unwrap()
        );
    }
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
        .current_dir(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap(),
        )
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
