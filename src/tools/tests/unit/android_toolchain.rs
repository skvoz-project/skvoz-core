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
