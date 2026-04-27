use std::path::PathBuf;

fn runtime_dockerfile() -> String {
    let repo_root = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .expect("repo root should be discoverable");
    let path = repo_root.join("Dockerfile");
    std::fs::read_to_string(path).expect("Dockerfile should be readable")
}

#[test]
fn runtime_image_declares_and_prepares_t3claw_home() {
    let dockerfile = runtime_dockerfile();

    assert!(
        dockerfile.contains("useradd -m -d /home/t3claw -u 1000 t3claw"),
        "runtime image must create the t3claw user with the expected home directory",
    );
    assert!(
        dockerfile.contains("ENV HOME=/home/t3claw"),
        "runtime image must set HOME to /home/t3claw for ~/.t3claw state",
    );
    assert!(
        dockerfile.contains("WORKDIR /home/t3claw"),
        "runtime image must start in the t3claw home directory",
    );
    assert!(
        dockerfile.contains("mkdir -p /home/t3claw/.t3claw"),
        "runtime image must pre-create ~/.t3claw before dropping privileges",
    );
}
