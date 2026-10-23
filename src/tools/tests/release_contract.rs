//! Release transport tests use prepared supplier bytes and a fake GitHub endpoint.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const TARGETS: [&str; 3] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-linux-android",
    "x86_64-linux-android",
];
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn copy(from: &Path, to: &Path) {
    fs::create_dir(to).unwrap();
    for item in fs::read_dir(from).unwrap() {
        let item = item.unwrap();
        if item.file_type().unwrap().is_dir() {
            copy(&item.path(), &to.join(item.file_name()));
        } else {
            fs::copy(item.path(), to.join(item.file_name())).unwrap();
        }
    }
}
fn fixtures(root: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            fixtures(&path, found);
        } else if path.file_name().unwrap() == "artifact.json" {
            found.push(path.parent().unwrap().to_owned());
        }
    }
}
fn tool(args: &[&str], mock: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_skvoz-core-tools"));
    command
        .args(args)
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("SKVOZ_CORE_SOURCE_REVISION")
        .env(
            "PATH",
            format!("{}:{}", mock.display(), std::env::var("PATH").unwrap()),
        )
        .env("GITHUB_REPOSITORY", "skvoz-project/skvoz-core")
        .env("GITHUB_REF", "refs/heads/main")
        .env("GITHUB_SHA", COMMIT)
        .env("GITHUB_EVENT_NAME", "push")
        .env("MOCK_LOG", mock.join("calls"));
    command
}
fn ok(result: Output) {
    assert!(result.status.success(), "{result:?}");
}
fn fails(result: Output, text: &str) {
    assert!(!result.status.success(), "unexpected success: {result:?}");
    assert!(
        String::from_utf8_lossy(&result.stderr).contains(text),
        "{result:?}"
    );
}
#[test]
#[ignore = "requires SKVOZ_CORE_RELEASE_FIXTURES containing three prepared supplier targets"]
fn exact_release_archives_and_fail_closed_publication() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch = Directory(std::env::temp_dir().join(format!(
        "core-release-contract-{}-{nonce}",
        std::process::id()
    )));
    fs::create_dir(&scratch.0).unwrap();
    let mock = scratch.0.join("mock");
    fs::create_dir(&mock).unwrap();
    fs::write(mock.join("gh"), "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" >> \"$MOCK_LOG\"\nif [ \"$1\" = api ]; then exit \"${MOCK_TAG_EXIT:-0}\"; else exit \"${MOCK_CREATE_EXIT:-0}\"; fi\n").unwrap();
    fs::write(mock.join("git"), format!("#!/bin/sh\nset -eu\ncase \"$3\" in rev-parse) echo {COMMIT};; status) printf '%s' \"${{MOCK_DIRTY:-}}\";; *) exit 1;; esac\n")).unwrap();
    for name in ["gh", "git"] {
        fs::set_permissions(mock.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut prepared = Vec::new();
    fixtures(
        Path::new(&std::env::var("SKVOZ_CORE_RELEASE_FIXTURES").unwrap()),
        &mut prepared,
    );
    let release = scratch.0.join("release");
    let release_text = release.to_str().unwrap();
    let mut original_library = None;
    for target in TARGETS {
        let source = prepared
            .iter()
            .find(|root| {
                let value: serde_json::Value =
                    serde_json::from_slice(&fs::read(root.join("artifact.json")).unwrap()).unwrap();
                value["target"] == target
            })
            .expect("prepared target missing");
        let artifact = scratch.0.join(target);
        copy(source, &artifact);
        let manifest_path = artifact.join("artifact.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        fails(
            tool(
                &["archive", artifact.to_str().unwrap(), COMMIT, release_text],
                &mock,
            )
            .output()
            .unwrap(),
            "identity mismatch",
        );
        manifest["source"]["revision"] = COMMIT.into();
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        ok(tool(
            &["archive", artifact.to_str().unwrap(), COMMIT, release_text],
            &mock,
        )
        .output()
        .unwrap());
        fails(
            tool(
                &["archive", artifact.to_str().unwrap(), COMMIT, release_text],
                &mock,
            )
            .output()
            .unwrap(),
            "File exists",
        );
        fs::write(artifact.join("sdk/extra"), "unlisted").unwrap();
        fails(
            tool(
                &["archive", artifact.to_str().unwrap(), COMMIT, release_text],
                &mock,
            )
            .output()
            .unwrap(),
            "closure mismatch",
        );
        fs::remove_file(artifact.join("sdk/extra")).unwrap();
        fs::create_dir(artifact.join("sdk/.knowledge")).unwrap();
        fs::write(artifact.join("sdk/.knowledge/extra"), "unlisted").unwrap();
        fails(
            tool(
                &["archive", artifact.to_str().unwrap(), COMMIT, release_text],
                &mock,
            )
            .output()
            .unwrap(),
            "closure mismatch",
        );
        fs::remove_dir_all(artifact.join("sdk/.knowledge")).unwrap();
        fs::write(artifact.join("sdk/LICENSE"), "tampered").unwrap();
        fails(
            tool(
                &["archive", artifact.to_str().unwrap(), COMMIT, release_text],
                &mock,
            )
            .output()
            .unwrap(),
            "inventory digest mismatch",
        );
        if target == TARGETS[0] {
            original_library = Some(source.join("lib/libskvoz_core.so"));
        }
    }
    ok(tool(&["checksums", release_text, COMMIT], &mock)
        .output()
        .unwrap());
    let sums_path = release.join("SHA256SUMS");
    let sums = fs::read(&sums_path).unwrap();
    fails(
        tool(&["publish", release_text, COMMIT], &mock)
            .env("GITHUB_EVENT_NAME", "pull_request")
            .output()
            .unwrap(),
        "trusted owning main",
    );
    fails(
        tool(&["publish", release_text, COMMIT], &mock)
            .env("GITHUB_REF", "refs/heads/topic")
            .output()
            .unwrap(),
        "trusted owning main",
    );
    assert!(!mock.join("calls").exists());
    fs::write(&sums_path, "bad").unwrap();
    fails(
        tool(&["publish", release_text, COMMIT], &mock)
            .output()
            .unwrap(),
        "SHA256SUMS mismatch",
    );
    assert!(!mock.join("calls").exists());
    fs::write(&sums_path, sums).unwrap();
    fails(
        tool(&["publish", release_text, COMMIT], &mock)
            .env("MOCK_TAG_EXIT", "1")
            .output()
            .unwrap(),
        "tag creation failed",
    );
    assert!(
        !fs::read_to_string(mock.join("calls"))
            .unwrap()
            .contains("release create")
    );
    fs::remove_file(mock.join("calls")).unwrap();
    ok(tool(&["publish", release_text, COMMIT], &mock)
        .env("GITHUB_EVENT_NAME", "workflow_dispatch")
        .output()
        .unwrap());
    let calls = fs::read_to_string(mock.join("calls")).unwrap();
    assert_eq!(calls.lines().count(), 2);
    assert!(calls.contains(&format!("api --method POST repos/skvoz-project/skvoz-core/git/refs -f ref=refs/tags/core-v5.0.0-{COMMIT} -f sha={COMMIT}")));
    assert!(calls.contains(&format!("release create core-v5.0.0-{COMMIT} --repo skvoz-project/skvoz-core --verify-tag --latest=false")));
    assert!(!calls.contains("clobber"));
    fails(
        tool(&["publish", release_text, COMMIT], &mock)
            .env("MOCK_CREATE_EXIT", "1")
            .output()
            .unwrap(),
        "publication failed",
    );
    let library = original_library.unwrap();
    let packaged = scratch.0.join("owning-commit");
    ok(tool(
        &[
            "package",
            library.to_str().unwrap(),
            packaged.to_str().unwrap(),
        ],
        &mock,
    )
    .env("SKVOZ_CORE_SOURCE_REVISION", COMMIT)
    .output()
    .unwrap());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(packaged.join("artifact.json")).unwrap()).unwrap();
    assert_eq!(manifest["source"]["revision"], COMMIT);
    fails(
        tool(
            &[
                "package",
                library.to_str().unwrap(),
                scratch.0.join("dirty").to_str().unwrap(),
            ],
            &mock,
        )
        .env("SKVOZ_CORE_SOURCE_REVISION", COMMIT)
        .env("MOCK_DIRTY", " M src/tools/src/main.rs")
        .output()
        .unwrap(),
        "clean owning commit",
    );
    fails(
        tool(
            &[
                "archive",
                packaged.to_str().unwrap(),
                "latest",
                release_text,
            ],
            &mock,
        )
        .output()
        .unwrap(),
        "full lowercase Git commit",
    );
    fs::remove_file(release.join(format!("skvoz-core-5.0.0-{}.tar.gz", TARGETS[2]))).unwrap();
    fails(
        tool(&["publish", release_text, COMMIT], &mock)
            .output()
            .unwrap(),
        "No such file",
    );
}
