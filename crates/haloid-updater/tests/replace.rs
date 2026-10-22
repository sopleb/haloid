//! The pure parts of the sidecar: what it reads from the running container,
//! and the replacement it builds. The fixtures are a real `docker inspect`
//! of a container started the way compose starts Haloid, and its image.

use bollard::models::{ContainerInspectResponse, ImageConfig};
use haloid_release::Edition;
use haloid_updater::{parse_version, replacement, tag_of};

fn fixture<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn the_container_says_what_it_is() {
    assert_eq!(
        parse_version("haloid-server 0.2.0 pro x86_64-unknown-linux-musl\n"),
        Some(("0.2.0".to_string(), Edition::Pro))
    );
    // Docker's exec stream can lead with other output.
    assert_eq!(
        parse_version("warning: something\nhaloid-server 0.1.0 community x\n"),
        Some(("0.1.0".to_string(), Edition::Community))
    );
    assert_eq!(parse_version("sh: haloid-server: not found"), None);
    assert_eq!(parse_version("haloid-server 0.1.0 enterprise x"), None);
}

#[test]
fn the_name_a_container_was_started_from_is_kept_unless_it_has_none() {
    assert_eq!(
        tag_of("git.plb.so:5050/sopleb/haloid:stable"),
        Some(("git.plb.so:5050/sopleb/haloid", "stable"))
    );
    assert_eq!(
        tag_of("localhost:5000/haloid"),
        Some(("localhost:5000/haloid", "latest"))
    );
    assert_eq!(tag_of("haloid"), Some(("haloid", "latest")));
    assert_eq!(tag_of("registry/haloid@sha256:abc"), None);
    assert_eq!(tag_of("sha256:0123"), None);
}

#[test]
fn the_replacement_keeps_the_containers_own_settings_and_drops_the_images() {
    let old: ContainerInspectResponse = fixture("container.json");
    let image: ImageConfig = fixture("image-config.json");
    let new = replacement(&old, Some(&image), "registry/haloid:stable");

    assert_eq!(new.image.as_deref(), Some("registry/haloid:stable"));
    // The operator's environment stays; the old image's goes, so the new
    // image's own PATH and versions apply.
    assert_eq!(
        new.env.as_deref(),
        Some(&["HALOID_SITE_BASE_URL=https://img.example.com".to_string()][..])
    );
    assert_eq!(
        new.cmd, None,
        "the image's command is the new image's business"
    );
    let labels = new.labels.unwrap();
    assert_eq!(
        labels.get("so.plb.haloid.updates").map(String::as_str),
        Some("server")
    );
    assert_eq!(
        labels.get("com.docker.compose.service").map(String::as_str),
        Some("haloid")
    );
    assert_eq!(
        new.hostname, None,
        "Docker's made-up hostname is not copied"
    );

    // Ports, mounts and restart policy come across whole.
    let host = new.host_config.unwrap();
    assert_eq!(
        host.restart_policy.unwrap().name.unwrap().to_string(),
        "unless-stopped"
    );
    let bindings = host.port_bindings.unwrap();
    let binding = &bindings["6379/tcp"].as_ref().unwrap()[0];
    assert_eq!(binding.host_port.as_deref(), Some("18099"));
    assert!(
        host.binds
            .unwrap()
            .iter()
            .any(|b| b.starts_with("upd-fixture-data:"))
    );

    // Both networks, with the alias, and without what Docker assigned.
    let endpoints = new.networking_config.unwrap().endpoints_config.unwrap();
    assert_eq!(endpoints.len(), 2);
    let own = &endpoints["upd-fixture-net"];
    assert!(own.aliases.as_ref().unwrap().iter().any(|a| a == "haloid"));
    assert_eq!(own.ip_address, None);
    assert_eq!(own.endpoint_id, None);
}
