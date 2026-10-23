//! Selective extraction of pinned public tools into one private supplier directory.
use crate::{Error, android::pinned_archive, hash, output};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Component, Path, PathBuf},
    process::Command,
};
const MIB: u64 = 1024 * 1024;

struct Scratch(PathBuf);
impl Scratch {
    fn create(path: PathBuf) -> Result<Self, Error> {
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn path_text(path: &Path) -> Result<&str, Error> {
    path.to_str().ok_or_else(|| "non-UTF8 tool path".into())
}
fn safe_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.len() > 1024
        || name
            .chars()
            .any(|c| c.is_whitespace() || "*?[]\\".contains(c))
        || Path::new(name)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("unsafe archive member name".into());
    }
    Ok(())
}
fn choose_ndk(name: &str) -> bool {
    if name == "android-ndk-r30/source.properties" {
        return true;
    }
    let Some(tail) = name.strip_prefix("android-ndk-r30/toolchains/llvm/prebuilt/linux-x86_64/")
    else {
        return false;
    };
    if let Some(bin) = tail.strip_prefix("bin/") {
        return [
            "clang",
            "clang++",
            "clang-21",
            "ld.lld",
            "lld",
            "llvm-ar",
            "llvm-objcopy",
            "llvm-strip",
            "aarch64-linux-android31-clang",
            "x86_64-linux-android31-clang",
        ]
        .contains(&bin);
    }
    if tail.starts_with("lib/clang/") || tail.starts_with("sysroot/usr/include/") {
        return true;
    }
    ["aarch64-linux-android", "x86_64-linux-android"]
        .iter()
        .any(|target| {
            tail.strip_prefix(&format!("sysroot/usr/lib/{target}/"))
                .is_some_and(|name| !name.contains('/') || name.starts_with("31/"))
        })
}
fn extract_ndk(archive: &Path, stage: &Path) -> Result<Value, Error> {
    let listing = output("zipinfo", &["-l", path_text(archive)?])?;
    let mut regular = Vec::new();
    let mut links = Vec::new();
    let mut names = BTreeSet::new();
    let mut bytes = 0_u64;
    for line in listing.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let Some(name) = fields.last().copied().filter(|name| choose_ndk(name)) else {
            continue;
        };
        let mode = fields.first().ok_or("missing ZIP mode")?;
        if mode.starts_with('d') {
            continue;
        }
        safe_name(name)?;
        if !names.insert(name.to_owned()) || names.len() > 12000 {
            return Err("duplicate or excessive ZIP members".into());
        }
        let size: u64 = fields.get(3).ok_or("missing ZIP size")?.parse()?;
        match mode.as_bytes().first() {
            Some(b'-') => {
                bytes = bytes.checked_add(size).ok_or("ZIP byte count overflow")?;
                if bytes > 1200 * MIB {
                    return Err("minimal NDK exceeds extraction byte cap".into());
                }
                regular.push((name.to_owned(), size, mode.contains('x')));
            }
            Some(b'l') if size <= 1024 => links.push(name.to_owned()),
            _ => return Err("unsupported selected ZIP member type".into()),
        }
    }
    if regular.is_empty() {
        return Err("minimal NDK selection is empty".into());
    }
    for (name, _, _) in &regular {
        if links
            .iter()
            .any(|link| name.starts_with(&format!("{link}/")))
        {
            return Err("ZIP symlink used as a regular file parent".into());
        }
    }
    let mut targets = Vec::new();
    for name in &links {
        let target = output("unzip", &["-p", path_text(archive)?, name])?;
        if target.len() > 1024 || target.contains('\0') || Path::new(&target).is_absolute() {
            return Err("unsafe ZIP symlink target".into());
        }
        let mut normalized = Path::new(name)
            .parent()
            .ok_or("invalid ZIP link parent")?
            .to_owned();
        for part in Path::new(&target).components() {
            match part {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => (),
                Component::ParentDir if normalized.pop() => (),
                _ => return Err("ZIP symlink escapes staging".into()),
            }
        }
        if !normalized.starts_with("android-ndk-r30") {
            return Err("ZIP symlink leaves NDK".into());
        }
        targets.push((name, target));
    }
    let destination = stage.join("ndk");
    fs::create_dir(&destination)?;
    for batch in regular.chunks(128) {
        let status = Command::new("unzip")
            .arg("-qq")
            .arg(archive)
            .args(batch.iter().map(|(name, _, _)| name))
            .arg("-d")
            .arg(&destination)
            .status()?;
        if !status.success() {
            return Err("selective NDK extraction failed".into());
        }
    }
    for (name, size, executable) in &regular {
        let file = destination.join(name);
        if !fs::symlink_metadata(&file)?.is_file() || fs::metadata(&file)?.len() != *size {
            return Err("extracted NDK regular file mismatch".into());
        }
        fs::set_permissions(
            &file,
            fs::Permissions::from_mode(if *executable { 0o755 } else { 0o644 }),
        )?;
    }
    for (name, target) in targets {
        let link = destination.join(name);
        fs::create_dir_all(link.parent().ok_or("invalid ZIP link parent")?)?;
        symlink(target, link)?;
    }
    let canonical = destination.canonicalize()?;
    for name in &links {
        if !destination
            .join(name)
            .canonicalize()?
            .starts_with(&canonical)
        {
            return Err("extracted NDK symlink escapes staging".into());
        }
    }
    Ok(json!({"regular_files": regular.len(), "unpacked_bytes": bytes, "symlinks": links.len()}))
}
fn fetch_std(profile: &Value, download: &Path) -> Result<(), Error> {
    if !download.exists() {
        let part = download.with_extension("part");
        if part.exists() {
            return Err("incomplete Rust std download requires explicit cleanup".into());
        }
        let status = Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--proto",
                "=https",
                "--connect-timeout",
                "15",
                "--max-time",
                "180",
                "--max-filesize",
            ])
            .arg(profile["rust_std_bytes"].to_string())
            .arg("--output")
            .arg(&part)
            .arg(profile["rust_std_url"].as_str().unwrap())
            .status()?;
        if !status.success() {
            let _ = fs::remove_file(part);
            return Err("pinned Rust std download failed".into());
        }
        pinned_archive(
            &part,
            profile["rust_std_bytes"].as_u64().unwrap(),
            "sha256sum",
            profile["rust_std_sha256"].as_str().unwrap(),
        )?;
        fs::rename(part, download)?;
    }
    pinned_archive(
        download,
        profile["rust_std_bytes"].as_u64().unwrap(),
        "sha256sum",
        profile["rust_std_sha256"].as_str().unwrap(),
    )
}
fn extract_std(archive: &Path, target: &str, stage: &Path) -> Result<Value, Error> {
    let prefix = format!("rust-std-1.92.0-{target}/rust-std-{target}/lib/rustlib/{target}/");
    let listing = output(
        "tar",
        &[
            "-tvJf",
            path_text(archive)?,
            "--numeric-owner",
            "--quoting-style=literal",
        ],
    )?;
    let mut members = BTreeSet::new();
    let mut bytes = 0_u64;
    for line in listing.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let Some(name) = fields
            .last()
            .copied()
            .filter(|name| name.starts_with(&prefix))
        else {
            continue;
        };
        if fields.first().is_some_and(|mode| mode.starts_with('d')) {
            continue;
        }
        safe_name(name)?;
        if !fields.first().is_some_and(|mode| mode.starts_with('-'))
            || !members.insert(name.to_owned())
            || members.len() > 1000
        {
            return Err("unsupported or duplicate Rust std member".into());
        }
        bytes = bytes
            .checked_add(
                fields
                    .get(2)
                    .ok_or("missing tar member size")?
                    .parse::<u64>()?,
            )
            .ok_or("tar byte count overflow")?;
        if bytes > 384 * MIB {
            return Err("Rust std exceeds extraction byte cap".into());
        }
    }
    if members.is_empty() {
        return Err("Rust std target selection is empty".into());
    }
    let temporary = Scratch::create(stage.join(format!("unpack-{target}")))?;
    let selection = temporary.0.join("members");
    fs::write(
        &selection,
        members
            .iter()
            .map(|name| format!("{name}\n"))
            .collect::<String>(),
    )?;
    let status = Command::new("tar")
        .arg("-xJf")
        .arg(archive)
        .arg("--no-same-owner")
        .arg("--no-same-permissions")
        .arg("--keep-old-files")
        .arg("--verbatim-files-from")
        .arg("-T")
        .arg(selection)
        .current_dir(&temporary.0)
        .status()?;
    if !status.success() {
        return Err("selective Rust std extraction failed".into());
    }
    fn validate(root: &Path, current: &Path, members: &BTreeSet<String>) -> Result<usize, Error> {
        let mut count = 0;
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            if entry.path() == root.join("members") {
                continue;
            }
            if entry.file_type()?.is_dir() {
                count += validate(root, &entry.path(), members)?;
            } else if entry.file_type()?.is_file()
                && members.contains(path_text(entry.path().strip_prefix(root)?)?)
            {
                count += 1;
            } else {
                return Err("unexpected file in selected Rust std extraction".into());
            }
        }
        Ok(count)
    }
    if validate(&temporary.0, &temporary.0, &members)? != members.len() {
        return Err("selected Rust std extraction is incomplete".into());
    }
    let rustlib = stage.join("rust-android/lib/rustlib");
    fs::create_dir_all(&rustlib)?;
    fs::rename(temporary.0.join(prefix), rustlib.join(target))
        .map_err(|error| format!("moving verified Rust std target {target} failed: {error}"))?;
    Ok(json!({"regular_files": members.len(), "unpacked_bytes": bytes}))
}

pub fn prepare(ndk_archive: &Path, destination: &Path) -> Result<(), Error> {
    let pins: Value = serde_json::from_str(include_str!("../../../docker/android-profiles.json"))?;
    fs::create_dir_all(destination)?;
    for name in ["tooling.json", "ndk", "sdk", "rust-android"] {
        if fs::symlink_metadata(destination.join(name)).is_ok() {
            return Err("Android tools already present; do not overwrite shared staging".into());
        }
    }
    let _lock = Scratch::create(destination.join(".prepare-lock"))?;
    let stage = Scratch::create(destination.join(".prepare-stage"))?;
    pinned_archive(
        ndk_archive,
        pins["ndk_archive_bytes"].as_u64().unwrap(),
        "sha1sum",
        pins["ndk_archive_sha1"].as_str().unwrap(),
    )?;
    let ndk = extract_ndk(ndk_archive, &stage.0)?;
    let properties = fs::read_to_string(stage.0.join("ndk/android-ndk-r30/source.properties"))?;
    if !properties.lines().any(|line| {
        line.split_once('=').is_some_and(|(name, value)| {
            name.trim() == "Pkg.Revision" && Some(value.trim()) == pins["ndk_revision"].as_str()
        })
    }) {
        return Err("staged NDK revision mismatch".into());
    }
    let downloads = destination.join("downloads");
    fs::create_dir_all(&downloads)?;
    let mut std = serde_json::Map::new();
    for target in ["aarch64-linux-android", "x86_64-linux-android"] {
        let archive = downloads.join(format!("rust-std-1.92.0-{target}.tar.xz"));
        fetch_std(&pins["targets"][target], &archive)?;
        std.insert(target.to_owned(), extract_std(&archive, target, &stage.0)?);
    }
    symlink(
        "/toolchain/lib/rustlib/x86_64-unknown-linux-gnu",
        stage
            .0
            .join("rust-android/lib/rustlib/x86_64-unknown-linux-gnu"),
    )?;
    fs::create_dir_all(stage.0.join("sdk/ndk"))?;
    symlink(
        "../../ndk/android-ndk-r30",
        stage.0.join("sdk/ndk/30.0.16248370"),
    )?;
    let manifest = json!({"pins": pins, "ndk_archive_sha256": hash(ndk_archive)?, "ndk": ndk, "rust_std": std,
        "scope": "prepared pinned tools only; no Android runtime qualification"});
    for name in ["ndk", "sdk", "rust-android"] {
        fs::rename(stage.0.join(name), destination.join(name))?;
    }
    fs::write(
        destination.join("tooling.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!(
        "Shared pinned Android tools prepared: {}",
        destination.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn std_extraction_stays_in_its_destination() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = Scratch::create(std::env::temp_dir().join(format!(
            "core-std-extraction-{}-{nonce}",
            std::process::id()
        )))
        .unwrap();
        for target in ["aarch64-linux-android", "x86_64-linux-android"] {
            let archive_root = format!("rust-std-1.92.0-{target}");
            let member =
                format!("{archive_root}/rust-std-{target}/lib/rustlib/{target}/lib/libstd.rlib");
            let fixture = root.0.join(format!("fixture-{target}"));
            fs::create_dir_all(fixture.join(&member).parent().unwrap()).unwrap();
            fs::write(fixture.join(member), b"fixture").unwrap();
            fs::write(
                fixture.join(&archive_root).join("excluded.txt"),
                b"excluded",
            )
            .unwrap();
            let archive = root.0.join(format!("{target}.tar.xz"));
            assert!(
                Command::new("tar")
                    .arg("-cJf")
                    .arg(&archive)
                    .arg(&archive_root)
                    .current_dir(&fixture)
                    .status()
                    .unwrap()
                    .success()
            );
            let destination = Scratch::create(root.0.join(format!("output-{target}"))).unwrap();
            let report = extract_std(&archive, target, &destination.0).unwrap();
            assert_eq!(report["regular_files"], 1);
            assert_eq!(report["unpacked_bytes"], 7);
            assert_eq!(
                fs::read(
                    destination
                        .0
                        .join(format!("rust-android/lib/rustlib/{target}/lib/libstd.rlib"))
                )
                .unwrap(),
                b"fixture"
            );
            assert!(!std::env::current_dir().unwrap().join(archive_root).exists());
            assert!(!destination.0.join("excluded.txt").exists());
        }
    }
}
