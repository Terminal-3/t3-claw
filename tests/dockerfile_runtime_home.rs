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
fn runtime_image_declares_and_prepares_bastionclaw_home() {
    let dockerfile = runtime_dockerfile();

    assert!(
        dockerfile.contains("useradd -m -d /home/bastionclaw -u 1000 bastionclaw"),
        "runtime image must create the bastionclaw user with the expected home directory",
    );
    assert!(
        dockerfile.contains("ENV HOME=/home/bastionclaw"),
        "runtime image must set HOME to /home/bastionclaw for ~/.bastionclaw state",
    );
    assert!(
        dockerfile.contains("WORKDIR /home/bastionclaw"),
        "runtime image must start in the bastionclaw home directory",
    );
    assert!(
        dockerfile.contains("mkdir -p /home/bastionclaw/.bastionclaw"),
        "runtime image must pre-create ~/.bastionclaw before dropping privileges",
    );
}
