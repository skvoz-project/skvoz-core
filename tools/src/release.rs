//! Exact-commit release transport; existing candidate artifacts remain unchanged.
use crate::{Error, hash, output};
use serde_json::Value;
use std::{
    env, fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

const REPOSITORY: &str = "https://github.com/skvoz-project/skvoz-core";
const SLUG: &str = "skvoz-project/skvoz-core";
const TARGETS: [&str; 3] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-linux-android",
    "x86_64-linux-android",
];
fn revision(value: &str) -> Result<(), Error> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("release revision must be a full lowercase Git commit SHA".into());
    }
    Ok(())
}
pub fn source_revision(root: &Path) -> Result<Option<String>, Error> {
    let Some(value) = env::var_os("SKVOZ_CORE_SOURCE_REVISION") else {
        return Ok(None);
    };
    let value = value.into_string().map_err(|_| "invalid source revision")?;
    revision(&value)?;
    let root = root.to_str().ok_or("invalid source root")?;
    if output("git", &["-C", root, "rev-parse", "HEAD"])?.trim() != value
        || !output(
            "git",
            &["-C", root, "status", "--porcelain", "--untracked-files=all"],
        )?
        .trim()
        .is_empty()
    {
        return Err("release packaging requires the clean owning commit".into());
    }
    Ok(Some(value))
}
fn filename(target: &str) -> String {
    format!("skvoz-core-5.0.0-{target}.tar.gz")
}
fn safe_path(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(char::is_whitespace)
        && Path::new(name)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
fn inventory(root: &Path, name: &str, complete: bool) -> Result<(), Error> {
    let text = fs::read_to_string(root.join(name))?;
    let mut listed = std::collections::BTreeSet::new();
    for line in text.lines() {
        let (digest, path) = line.split_once("  ").ok_or("invalid release inventory")?;
        if digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            || !safe_path(path)
            || !listed.insert(PathBuf::from(path))
        {
            return Err("unsafe or duplicate release inventory entry".into());
        }
        if complete && hash(&root.join(path))? != digest {
            return Err("SDK inventory digest mismatch".into());
        }
    }
    if listed.is_empty() {
        return Err("empty release inventory".into());
    }
    if complete {
        let mut actual = Vec::new();
        all_files(root, root, &mut actual)?;
        actual.retain(|path| path != Path::new(name));
        if actual
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            != listed
        {
            return Err("SDK inventory closure mismatch".into());
        }
    }
    Ok(())
}
fn all_files(root: &Path, path: &Path, paths: &mut Vec<PathBuf>) -> Result<(), Error> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            all_files(root, &entry.path(), paths)?;
        } else if entry.file_type()?.is_file() {
            paths.push(entry.path().strip_prefix(root)?.to_owned());
        } else {
            return Err("release contains a non-regular file".into());
        }
    }
    Ok(())
}
fn validate(root: &Path, commit: &str) -> Result<String, Error> {
    revision(commit)?;
    all_files(root, root, &mut Vec::new())?;
    let value: Value = serde_json::from_slice(&fs::read(root.join("artifact.json"))?)?;
    let target = value["target"]
        .as_str()
        .ok_or("Core release target missing")?;
    if !TARGETS.contains(&target)
        || value["component"] != "skvoz-core"
        || value["version"] != "5.0.0"
        || value["abi"] != 1
        || value["kind"] != "shared-library"
        || value["profile"] != "release"
        || value["features"] != serde_json::json!(["nats"])
        || value["owning_repository"] != REPOSITORY
        || value["source"]["repository"] != REPOSITORY
        || value["source"]["revision"] != commit
    {
        return Err("Core release identity mismatch".into());
    }
    for (path, expected) in [
        ("lib/libskvoz_core.so", &value["sha256"]),
        ("sdk/sdk.sha256", &value["sdk_manifest_sha256"]),
        ("source-inputs.sha256", &value["source_inputs_sha256"]),
    ] {
        if expected.as_str() != Some(hash(&root.join(path))?.as_str()) {
            return Err("Core release digest mismatch".into());
        }
    }
    if value["source"]["tree_sha256"] != value["source_inputs_sha256"] {
        return Err("Core release source identity mismatch".into());
    }
    inventory(&root.join("sdk"), "sdk.sha256", true)?;
    inventory(root, "source-inputs.sha256", false)?;
    for name in ["sdk/LICENSE", "LICENSE-async-nats", "PATCHES-async-nats.md"] {
        if fs::metadata(root.join(name))?.len() == 0 {
            return Err("empty Core release license".into());
        }
    }
    Ok(target.into())
}
pub fn archive(artifact: &Path, commit: &str, destination: &Path) -> Result<(), Error> {
    let target = validate(artifact, commit)?;
    fs::create_dir_all(destination)?;
    let archive = destination.join(filename(&target));
    let reservation = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive)?;
    drop(reservation);
    let result = Command::new("tar")
        .args([
            "--sort=name",
            "--mtime=@0",
            "--owner=0",
            "--group=0",
            "--numeric-owner",
            "-czf",
        ])
        .arg(&archive)
        .arg("-C")
        .arg(artifact)
        .arg(".")
        .status();
    if !result?.success() {
        let _ = fs::remove_file(&archive);
        return Err("Core release archive failed".into());
    }
    println!("Core release archive prepared: {}", archive.display());
    Ok(())
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn unpack(archive: &Path) -> Result<Scratch, Error> {
    if fs::metadata(archive)?.len() > 256 * 1024 * 1024 {
        return Err("Core release archive exceeds byte cap".into());
    }
    let path = archive.to_str().ok_or("invalid archive path")?;
    let listing = output(
        "tar",
        &["-tvzf", path, "--numeric-owner", "--quoting-style=literal"],
    )?;
    let mut count = 0;
    let mut bytes = 0_u64;
    let mut names = std::collections::BTreeSet::new();
    for line in listing.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 6 {
            return Err("invalid Core release tar listing".into());
        }
        let name = fields[5]
            .strip_prefix("./")
            .ok_or("invalid Core release tar root")?
            .trim_end_matches('/');
        if name.is_empty() && fields[0].starts_with('d') {
            continue;
        }
        if !safe_path(name)
            || !names.insert(name.to_owned())
            || !(fields[0].starts_with('-') || fields[0].starts_with('d'))
        {
            return Err("unsafe Core release tar member".into());
        }
        count += 1;
        bytes = bytes
            .checked_add(fields[2].parse::<u64>()?)
            .ok_or("tar byte count overflow")?;
        if count > 10000 || bytes > 512 * 1024 * 1024 {
            return Err("Core release tar exceeds extraction cap".into());
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let scratch =
        Scratch(env::temp_dir().join(format!("core-release-{}-{nonce}", std::process::id())));
    fs::create_dir(&scratch.0)?;
    if !Command::new("tar")
        .args([
            "-xzf",
            path,
            "--no-same-owner",
            "--no-same-permissions",
            "--keep-old-files",
        ])
        .arg("-C")
        .arg(&scratch.0)
        .status()?
        .success()
    {
        return Err("Core release extraction failed".into());
    }
    Ok(scratch)
}
fn validate_assets(directory: &Path, commit: &str) -> Result<String, Error> {
    revision(commit)?;
    let mut sums = String::new();
    for target in TARGETS {
        let name = filename(target);
        let archive = directory.join(&name);
        let scratch = unpack(&archive)?;
        if validate(&scratch.0, commit)? != target {
            return Err("Core release filename/target mismatch".into());
        }
        sums.push_str(&format!("{}  {name}\n", hash(&archive)?));
    }
    Ok(sums)
}
pub fn checksums(directory: &Path, commit: &str) -> Result<(), Error> {
    let sums = validate_assets(directory, commit)?;
    use std::io::Write;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("SHA256SUMS"))?
        .write_all(sums.as_bytes())?;
    Ok(())
}
pub fn publish(directory: &Path, commit: &str) -> Result<(), Error> {
    if env::var("GITHUB_REPOSITORY").as_deref() != Ok(SLUG)
        || env::var("GITHUB_REF").as_deref() != Ok("refs/heads/main")
        || env::var("GITHUB_SHA").as_deref() != Ok(commit)
        || !matches!(
            env::var("GITHUB_EVENT_NAME").as_deref(),
            Ok("push" | "workflow_dispatch")
        )
    {
        return Err("Core publication requires a trusted owning main run".into());
    }
    let sums = validate_assets(directory, commit)?;
    if fs::read_to_string(directory.join("SHA256SUMS"))? != sums {
        return Err("Core release SHA256SUMS mismatch".into());
    }
    let tag = format!("core-v5.0.0-{commit}");
    // Atomic creation rejects an existing tag, including a concurrent publication.
    let created = Command::new("gh")
        .args([
            "api",
            "--method",
            "POST",
            &format!("repos/{SLUG}/git/refs"),
            "-f",
            &format!("ref=refs/tags/{tag}"),
            "-f",
            &format!("sha={commit}"),
        ])
        .output()?;
    if !created.status.success() {
        return Err("Core release tag creation failed; existing tags are never overwritten".into());
    }
    let mut command = Command::new("gh");
    command.args(["release", "create", &tag, "--repo", SLUG, "--verify-tag", "--latest=false", "--title", &tag, "--notes", "Core 5.0.0: библиотеки Linux и Android с SDK, ABI v1. Проверка Android runtime принадлежит потребителю."]);
    for target in TARGETS {
        command.arg(directory.join(filename(target)));
    }
    command.arg(directory.join("SHA256SUMS"));
    if !command.status()?.success() {
        return Err("Core publication failed; existing releases are never overwritten".into());
    }
    Ok(())
}
