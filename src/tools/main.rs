//! Explicit supplier packaging; consumers never call Cargo to rebuild Core.
mod android;
#[cfg(unix)]
mod prepare_android;
mod release;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};
type Error = Box<dyn std::error::Error>;
fn output(program: &str, args: &[&str]) -> Result<String, Error> {
    let out = Command::new(program)
        .env("LC_ALL", "C")
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(format!("{program} failed").into());
    }
    Ok(String::from_utf8(out.stdout)?)
}
fn hash(path: &Path) -> Result<String, Error> {
    Ok(output(
        "sha256sum",
        &[path.to_str().ok_or("non-UTF8 artifact path")?],
    )?
    .split_whitespace()
    .next()
    .ok_or("empty digest")?
    .to_owned())
}
fn copy_tree(source: &Path, target: &Path) -> Result<(), Error> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let to = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}
fn files(root: &Path, current: &Path, list: &mut Vec<PathBuf>) -> Result<(), Error> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        if entry.path() == root.join("src/target")
            || [".knowledge", ".agents", "temp", "AGENTS.md"]
                .contains(&entry.file_name().to_string_lossy().as_ref())
        {
            continue;
        }
        if entry.file_type()?.is_dir() {
            files(root, &entry.path(), list)?;
        } else {
            list.push(entry.path().strip_prefix(root)?.to_owned());
        }
    }
    Ok(())
}
fn identity(root: &Path, paths: &mut [PathBuf]) -> Result<String, Error> {
    paths.sort();
    paths
        .iter()
        .map(|path| Ok(format!("{}  {}\n", hash(&root.join(path))?, path.display())))
        .collect::<Result<Vec<_>, Error>>()
        .map(|lines| lines.join(""))
}
fn sdk_manifest(mut manifest: toml::Table, workspace: &toml::Table) -> Result<String, Error> {
    for (name, value) in manifest["package"].as_table_mut().unwrap() {
        if value.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
            *value = workspace["package"][name].clone();
        }
    }
    manifest.remove("dev-dependencies");
    for section in ["dependencies", "build-dependencies"] {
        let Some(dependencies) = manifest
            .get_mut(section)
            .and_then(toml::Value::as_table_mut)
        else {
            continue;
        };
        for (name, value) in dependencies {
            if value.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                let mut inherited = match &workspace["dependencies"][name] {
                    toml::Value::String(version) => {
                        toml::Table::from_iter([("version".into(), version.clone().into())])
                    }
                    toml::Value::Table(dependency) => dependency.clone(),
                    _ => return Err("invalid workspace dependency".into()),
                };
                let mut local = value.as_table().unwrap().clone();
                local.remove("workspace");
                if let Some(toml::Value::Array(mut features)) = inherited.remove("features") {
                    if let Some(toml::Value::Array(local_features)) = local.remove("features") {
                        features.extend(local_features);
                    }
                    inherited.insert("features".into(), features.into());
                }
                inherited.extend(local);
                *value = inherited.into();
            }
        }
    }
    Ok(toml::to_string(&manifest)?)
}
fn sdk(root: &Path, target: &Path) -> Result<(), Error> {
    if target.exists() {
        return Err("SDK destination already exists".into());
    }
    let workspace: toml::Table = fs::read_to_string(root.join("src/Cargo.toml"))?.parse()?;
    let workspace = workspace["workspace"].as_table().unwrap();
    for package in ["contracts", "tls", "binding"] {
        let source = root.join("src").join(package);
        let destination = target.join(package);
        fs::create_dir_all(&destination)?;
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().is_some_and(|ext| ext == "rs")
            {
                fs::copy(entry.path(), destination.join(entry.file_name()))?;
            }
        }
        let manifest = fs::read_to_string(source.join("Cargo.toml"))?.parse()?;
        fs::write(
            destination.join("Cargo.toml"),
            sdk_manifest(manifest, workspace)?,
        )?;
    }
    fs::copy(root.join("LICENSE"), target.join("LICENSE"))?;
    copy_tree(&root.join("src/include"), &target.join("include"))?;
    let mut paths = Vec::new();
    files(target, target, &mut paths)?;
    fs::write(target.join("sdk.sha256"), identity(target, &mut paths)?)?;
    Ok(())
}
fn package(
    root: &Path,
    library: &Path,
    destination: &Path,
    target: Option<&str>,
) -> Result<(), Error> {
    let revision = release::source_revision(root)?;
    if destination.exists() {
        return Err("artifact destination already exists".into());
    }
    let header = output(
        "readelf",
        &["-h", library.to_str().ok_or("invalid library path")?],
    )?;
    if target.is_none() && (!header.contains("ELF64") || !header.contains("X86-64")) {
        return Err("unsupported Core artifact target".into());
    }
    let dependencies = output("readelf", &["-d", library.to_str().unwrap()])?;
    let needed: Vec<_> = dependencies
        .lines()
        .filter(|line| line.contains("(NEEDED)"))
        .filter_map(|line| line.split('[').nth(1)?.split(']').next())
        .collect();
    let versions = output("readelf", &["--version-info", library.to_str().unwrap()])?;
    let android = target
        .map(|target| android::inspect(target, library, &header, &needed, &versions))
        .transpose()?;
    let glibc: Vec<_> = versions
        .split_whitespace()
        .filter(|s| s.starts_with("GLIBC_"))
        .collect();
    if target.is_none() && (!needed.contains(&"libc.so.6") || glibc.is_empty()) {
        return Err("unsupported Core GNU libc profile".into());
    }
    fs::create_dir_all(destination.join("lib"))?;
    let installed = destination.join("lib/libskvoz_core.so");
    fs::copy(library, &installed)?;
    sdk(root, &destination.join("sdk"))?;
    fs::copy(
        root.join("src/vendor/async-nats/LICENSE"),
        destination.join("LICENSE-async-nats"),
    )?;
    fs::copy(
        root.join("src/vendor/async-nats/PATCHES.md"),
        destination.join("PATCHES-async-nats.md"),
    )?;
    let mut inputs = vec![PathBuf::from(".dockerignore")];
    for directory in ["src", "docker"] {
        files(root, &root.join(directory), &mut inputs)?;
    }
    fs::write(
        destination.join("source-inputs.sha256"),
        identity(root, &mut inputs)?,
    )?;
    let library_hash = hash(&installed)?;
    let sdk_hash = hash(&destination.join("sdk/sdk.sha256"))?;
    let source_hash = hash(&destination.join("source-inputs.sha256"))?;
    let target = target.unwrap_or("x86_64-unknown-linux-gnu");
    let platform = if android.is_some() {
        target
    } else {
        "linux-amd64-gnu"
    };
    let linker = if let Some(android) = &android {
        android["linker"].as_str().unwrap().to_owned()
    } else {
        output("cc", &["--version"])?
            .lines()
            .next()
            .unwrap_or("unknown")
            .to_owned()
    };
    let mut value = serde_json::json!({
        "component":"skvoz-core", "version":"5.0.0", "abi":1,
        "artifact_id":format!("5.0.0-abi1-{platform}-{library_hash}-{sdk_hash}"), "kind":"shared-library",
        "target":target, "profile":"release", "features":["nats"], "rustflags":env::var("RUSTFLAGS").unwrap_or_default(),
        "owning_repository":"https://github.com/skvoz-project/skvoz-core",
        "source":{"repository":"https://github.com/skvoz-project/skvoz-core","revision":revision,"tree_sha256":source_hash},
        "source_inputs_sha256":source_hash, "sdk_manifest_sha256":sdk_hash, "sha256":library_hash, "lock_sha256":hash(&root.join("src/Cargo.lock"))?,
        "rustc":output("rustc", &["--version"] )?.trim(),
        "rustc_verbose":output("rustc", &["-vV"] )?, "cargo":output("cargo", &["--version"] )?.trim(),
        "linker":linker,
        "target_cpu":"Rust target default; no target-cpu override",
        "runtime_dependencies":needed, "required_glibc_symbols":glibc,
        "layout":{"manager_entry":592,"manager_peer":656,"event_bridge":16384},
        "qualification":"candidate; see owning verification evidence"
    });
    if let Some(android) = android {
        value
            .as_object_mut()
            .unwrap()
            .remove("required_glibc_symbols");
        value["layout"]["manager_entry"] = 1024.into();
        value["layout"]["manager_peer"] = 1024.into();
        value["layout"]["bounds_kind"] =
            "conservative; supplier compile-time actual<=bound assertions".into();
        value["linker"] = android["linker"].clone();
        value["android"] = android;
    }
    value["soname"] = dependencies
        .lines()
        .find(|line| line.contains("(SONAME)"))
        .and_then(|line| line.split('[').nth(1)?.split(']').next())
        .into();
    fs::write(
        destination.join("artifact.json"),
        serde_json::to_vec_pretty(&value)?,
    )?;
    println!("Core artifact prepared: {}", destination.display());
    Ok(())
}
fn main() -> Result<(), Error> {
    let args: Vec<_> = env::args().skip(1).collect();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    match args.first().map(String::as_str) {
        Some("archive") if args.len() == 4 => release::archive(Path::new(&args[1]), &args[2], Path::new(&args[3])),
        Some("checksums") if args.len() == 3 => release::checksums(Path::new(&args[1]), &args[2]),
        Some("publish") if args.len() == 3 => release::publish(Path::new(&args[1]), &args[2]),
        #[cfg(unix)]
        Some("prepare-android") if args.len() == 3 => prepare_android::prepare(Path::new(&args[1]), Path::new(&args[2])),
        Some("sdk") if args.len() == 2 => sdk(&root, Path::new(&args[1])),
        Some("package") if args.len() == 3 => package(&root, Path::new(&args[1]), Path::new(&args[2]), None),
        Some("package-android") if args.len() == 4 => package(&root, Path::new(&args[2]), Path::new(&args[3]), Some(&args[1])),
        _ => {
            Err("usage: skvoz-core-tools sdk <output> | package <library> <output> | package-android <target> <library> <output> | prepare-android <ndk-zip> <shared-output> | archive <artifact> <revision> <output-directory> | checksums <release-directory> <revision> | publish <release-directory> <revision>".into())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sdk_manifest_resolves_inheritance_without_losing_local_features() {
        let workspace: toml::Table = r#"
            [package]
            edition = "2024"
            [dependencies]
            serde = { version = "=1.0.228", features = ["std"], default-features = false }
        "#
        .parse()
        .unwrap();
        let source = r#"
            [package]
            name = "consumer"
            edition.workspace = true
            [dependencies]
            serde = { workspace = true, features = ["derive"], optional = true }
            [build-dependencies]
            serde = { workspace = true }
            [dev-dependencies]
            private-fixture = { path = "../private" }
            [lib]
            path = "lib.rs"
        "#
        .parse()
        .unwrap();
        let exported: toml::Table = super::sdk_manifest(source, &workspace)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(exported["package"]["edition"].as_str(), Some("2024"));
        assert_eq!(exported["lib"]["path"].as_str(), Some("lib.rs"));
        assert!(exported.get("dev-dependencies").is_none());
        let dependency = &exported["dependencies"]["serde"];
        assert_eq!(dependency["version"].as_str(), Some("=1.0.228"));
        assert_eq!(dependency["default-features"].as_bool(), Some(false));
        assert_eq!(dependency["optional"].as_bool(), Some(true));
        assert_eq!(
            dependency["features"].as_array().unwrap(),
            &[toml::Value::from("std"), toml::Value::from("derive")]
        );
        assert!(dependency.get("workspace").is_none());
        assert_eq!(
            exported["build-dependencies"]["serde"]["features"]
                .as_array()
                .unwrap(),
            &[toml::Value::from("std")]
        );
    }
}
