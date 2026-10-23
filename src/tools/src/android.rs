//! Explicit Android supplier profiles; ELF checks do not qualify a device runtime.
use crate::{Error, files, hash, identity, output};
use serde_json::{Value, json};
use std::{env, fs, path::Path};

fn required(name: &str) -> Result<String, Error> {
    env::var(name).map_err(|_| format!("required Android supplier input missing: {name}").into())
}
pub(super) fn pinned_archive(
    path: &Path,
    bytes: u64,
    algorithm: &str,
    digest: &str,
) -> Result<(), Error> {
    if fs::metadata(path)?.len() != bytes {
        return Err("Android supplier archive length mismatch".into());
    }
    let actual = output(algorithm, &[path.to_str().ok_or("non-UTF8 archive path")?])?;
    if actual.split_whitespace().next() != Some(digest) {
        return Err("Android supplier archive digest mismatch".into());
    }
    Ok(())
}
pub fn inspect(
    target: &str,
    library: &Path,
    header: &str,
    dependencies: &[&str],
    versions: &str,
) -> Result<Value, Error> {
    let pins: Value = serde_json::from_str(include_str!("../../../docker/android-profiles.json"))?;
    let profile = &pins["targets"][target];
    if profile.is_null() {
        return Err("unsupported Core Android target".into());
    }
    let machine = profile["elf_machine"].as_str().unwrap();
    if !header.contains("ELF64")
        || !header.lines().any(|line| {
            line.split_once(':')
                .is_some_and(|(key, value)| key.trim() == "Machine" && value.trim() == machine)
        })
    {
        return Err("Core Android ELF target mismatch".into());
    }
    if !dependencies.contains(&"libc.so")
        || versions.contains("GLIBC_")
        || dependencies
            .iter()
            .any(|name| !["libc.so", "libm.so", "libdl.so", "liblog.so"].contains(name))
    {
        return Err("Core Android runtime dependency closure is not supported".into());
    }
    let ndk = required("SKVOZ_CORE_ANDROID_NDK")?;
    let sysroot = required("SKVOZ_CORE_ANDROID_SYSROOT")?;
    let ndk_archive = required("SKVOZ_CORE_ANDROID_NDK_ARCHIVE")?;
    let std_archive = required("SKVOZ_CORE_ANDROID_STD_ARCHIVE")?;
    pinned_archive(
        Path::new(&ndk_archive),
        pins["ndk_archive_bytes"].as_u64().unwrap(),
        "sha1sum",
        pins["ndk_archive_sha1"].as_str().unwrap(),
    )?;
    pinned_archive(
        Path::new(&std_archive),
        profile["rust_std_bytes"].as_u64().unwrap(),
        "sha256sum",
        profile["rust_std_sha256"].as_str().unwrap(),
    )?;
    let properties = fs::read_to_string(Path::new(&ndk).join("source.properties"))?;
    let revision = properties.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == "Pkg.Revision").then(|| value.trim())
    });
    if revision != pins["ndk_revision"].as_str() {
        return Err("Core Android NDK revision mismatch".into());
    }
    let target_std = Path::new(&sysroot).join("lib/rustlib").join(target);
    if !target_std.join("lib").is_dir() {
        return Err("Core Android Rust target sysroot missing".into());
    }
    let rustc = output("rustc", &["--version"])?;
    if rustc.split_whitespace().nth(1) != pins["rust_version"].as_str() {
        return Err("Core Android Rust version mismatch".into());
    }
    let flags = format!("-C link-arg=-Wl,-z,max-page-size=16384 --sysroot {sysroot}");
    if required("RUSTFLAGS")? != flags {
        return Err("Core Android RUSTFLAGS must match the pinned profile".into());
    }
    if env::var_os("CARGO_ENCODED_RUSTFLAGS").is_some() {
        return Err("Core Android profile does not allow CARGO_ENCODED_RUSTFLAGS".into());
    }
    let bin = Path::new(&ndk).join("toolchains/llvm/prebuilt/linux-x86_64/bin");
    let clang = bin.join(profile["clang"].as_str().unwrap());
    let ar = bin.join("llvm-ar");
    let target_env = target.replace('-', "_");
    let linker_env = format!("CARGO_TARGET_{}_LINKER", target_env.to_uppercase());
    let inputs = [
        (linker_env, clang.to_string_lossy().into_owned()),
        (
            format!("CC_{target_env}"),
            clang.to_string_lossy().into_owned(),
        ),
        (
            format!("AR_{target_env}"),
            ar.to_string_lossy().into_owned(),
        ),
        ("CARGO_INCREMENTAL".into(), "0".into()),
        ("CARGO_PROFILE_RELEASE_DEBUG".into(), "0".into()),
    ];
    for (name, expected) in &inputs {
        if required(name)? != *expected {
            return Err(format!("Core Android build input mismatch: {name}").into());
        }
    }
    let mut std_files = Vec::new();
    files(&target_std, &target_std, &mut std_files)?;
    let std_manifest = identity(&target_std, &mut std_files)?;
    let library_path = library.to_str().ok_or("non-UTF8 library path")?;
    let segments = output("readelf", &["-lW", library_path])?;
    let mut load_alignment = Vec::new();
    for line in segments
        .lines()
        .filter(|line| line.trim_start().starts_with("LOAD "))
    {
        let fields: Vec<_> = line.split_whitespace().collect();
        let number = |s: &str| u64::from_str_radix(s.trim_start_matches("0x"), 16);
        let offset = number(fields.get(1).ok_or("invalid ELF LOAD offset")?)?;
        let address = number(fields.get(2).ok_or("invalid ELF LOAD address")?)?;
        let alignment = number(fields.last().ok_or("invalid ELF LOAD alignment")?)?;
        if alignment < 16384 || offset % 16384 != address % 16384 {
            return Err("Core Android ELF LOAD segment is not 16 KiB aligned".into());
        }
        load_alignment.push(alignment);
    }
    if load_alignment.is_empty() {
        return Err("Core Android ELF LOAD segments missing".into());
    }
    let symbols = output("readelf", &["--dyn-syms", "--wide", library_path])?;
    for name in [
        "version",
        "capabilities",
        "buffer_free",
        "runtime_new",
        "runtime_free",
        "start",
        "poll",
        "operation_free",
        "call",
        "events",
    ] {
        let symbol = format!("skvoz_core_v1_{name}");
        if !symbols.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.last().copied() == Some(symbol.as_str())
                && fields.get(3).copied() == Some("FUNC")
                && fields.get(4).copied() == Some("GLOBAL")
                && fields.get(5).copied() == Some("DEFAULT")
                && fields.get(6).copied().is_some_and(|index| index != "UND")
        }) {
            return Err(format!("Core Android ABI export missing: {symbol}").into());
        }
    }
    Ok(json!({
        "target": target, "android_abi": profile["android_abi"],
        "elf_machine": machine, "elf_notes": output("readelf", &["--notes", library_path])?,
        "minimum_api": pins["minimum_api"], "page_alignment": pins["page_alignment"],
        "build_command": ["cargo", "build", "-p", "skvoz-core-abi", "--target", target, "--release", "--locked", "--offline"],
        "load_segment_alignments": load_alignment,
        "ndk_revision": pins["ndk_revision"], "ndk_archive_sha1": pins["ndk_archive_sha1"],
        "ndk_archive_sha256": hash(Path::new(&ndk_archive))?, "ndk_url": pins["ndk_url"],
        "rust_distribution": pins["rust_distribution"], "rust_std_url": profile["rust_std_url"],
        "rust_std_sha256": profile["rust_std_sha256"], "ndk": ndk, "rust_sysroot": sysroot,
        "rust_std_installed_manifest": std_manifest,
        "clang_sha256": hash(&bin.join("clang-21"))?, "ar_sha256": hash(&ar)?,
        "linker": output(clang.to_str().ok_or("non-UTF8 linker path")?, &["--version"] )?,
        "build_environment": inputs.into_iter().map(|(name, value)| (name, Value::String(value))).collect::<serde_json::Map<_, _>>(),
        "qualification": "candidate; target ELF inspection only; Android loader/JNI/runtime checks required"
    }))
}
