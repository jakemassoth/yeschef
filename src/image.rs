use std::io::Write;

use anyhow::{Context, Result};

use crate::backend::ContainerBackend;
use crate::names::{base_image_tag, project_image_tag};
use crate::store::Store;

// ---------------------------------------------------------------------------
// Dockerfile templates
// ---------------------------------------------------------------------------

// Notes:
//  - Base is plain Ubuntu + the Determinate Systems Nix installer (multi-user
//    mode). Nix is installed into `/nix/var/nix/profiles/default` and the
//    daemon socket lives at `/nix/var/nix/daemon-socket/socket`.
//  - claude-code is installed into the *system* (default) profile so the
//    non-root `sandbox` user can find `claude` on PATH (we set PATH below).
//  - We add a `sandbox` (uid 1000) user via `useradd` because
//    `claude --dangerously-skip-permissions` refuses to run under uid 0.
//  - `/usr/local/bin/nixsand-init` is the container entrypoint: it forks
//    `nix-daemon` (Determinate's multi-user install needs the daemon running
//    to serve store mutations) and then execs the command passed by the
//    create_container call (currently `sleep infinity`). Daemon stdout/stderr
//    go to /var/log/nix-daemon.log so it's debuggable but doesn't spam the
//    container's main log.
//  - The Determinate installer uses `linux --init none --no-confirm` because
//    Apple containers do not run systemd and the install must be
//    non-interactive.
const BASE_DOCKERFILE: &str = r#"FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      curl ca-certificates xz-utils git sudo \
 && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix \
    | sh -s -- install linux --init none --no-confirm
ENV PATH=/nix/var/nix/profiles/default/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
RUN printf '#!/bin/sh\nnix-daemon --daemon >/var/log/nix-daemon.log 2>&1 &\nexec "$@"\n' \
      > /usr/local/bin/nixsand-init \
 && chmod +x /usr/local/bin/nixsand-init
RUN nix-daemon --daemon >/var/log/nix-daemon.log 2>&1 & \
    for i in 1 2 3 4 5 6 7 8 9 10; do \
      [ -S /nix/var/nix/daemon-socket/socket ] && break; sleep 1; \
    done; \
    NIXPKGS_ALLOW_UNFREE=1 nix profile install \
      --profile /nix/var/nix/profiles/default \
      nixpkgs#claude-code --impure
RUN userdel -r ubuntu 2>/dev/null || true \
 && useradd -m -u 1000 -s /bin/bash sandbox
# Workspaces are bind-mounted from the host and Apple's virtiofs reports them
# as owned by root (0:0) regardless of the host UID. libgit2 — which Nix uses
# to evaluate `git+file://` flake inputs — refuses to open repos owned by a
# different user than the caller (`error code = 7`, "is not owned by current
# user"). Adding a wildcard `safe.directory` to the system gitconfig tells
# libgit2 (and git) to trust any directory, which is what we want here:
# every path inside this sandbox is project code the user asked us to run.
RUN install -d /etc \
 && printf '[safe]\n\tdirectory = *\n' > /etc/gitconfig
ENTRYPOINT ["/usr/local/bin/nixsand-init"]
CMD ["sleep", "infinity"]
"#;

fn project_dockerfile(flake_nix: &[u8], flake_lock: &[u8]) -> String {
    let _ = flake_nix;
    let _ = flake_lock;
    format!(
        r"FROM {}
COPY flake.nix /project-src/flake.nix
COPY flake.lock /project-src/flake.lock
RUN nix-daemon --daemon >/var/log/nix-daemon.log 2>&1 & \
    for i in 1 2 3 4 5 6 7 8 9 10; do \
      [ -S /nix/var/nix/daemon-socket/socket ] && break; sleep 1; \
    done; \
    cd /project-src && nix develop --command true
",
        base_image_tag()
    )
}

// ---------------------------------------------------------------------------
// Image lifecycle
// ---------------------------------------------------------------------------

/// Ensure the base image exists, building it if necessary.
pub fn ensure_base_image(container: &dyn ContainerBackend) -> Result<()> {
    let tag = base_image_tag();
    if container.image_exists(tag)? {
        eprintln!("[image] base image '{tag}' already exists, skipping build");
        return Ok(());
    }
    eprintln!("[image] building base image '{tag}'...");
    let context_dir = tempfile::tempdir().context("failed to create temp dir for base image build")?;
    let dockerfile_path = context_dir.path().join("Dockerfile");
    std::fs::write(&dockerfile_path, BASE_DOCKERFILE)
        .context("failed to write base Dockerfile")?;
    container
        .build_image(tag, context_dir.path())
        .with_context(|| format!("failed to build base image '{tag}'"))?;
    eprintln!("[image] base image '{tag}' built successfully");
    Ok(())
}

/// Ensure the per-project image is up to date.
/// Rebuilds if the image doesn't exist or if the flake.lock hash has changed.
pub fn ensure_project_image(
    project: &str,
    container: &dyn ContainerBackend,
    store: &Store,
    flake_nix_content: &[u8],
    flake_lock_content: &[u8],
) -> Result<()> {
    let tag = project_image_tag(project);
    let current_hash = sha256_hex(flake_lock_content);

    let recorded_hash = store.get_flake_lock_hash(project)?;
    let needs_build = if container.image_exists(&tag)? {
        match &recorded_hash {
            None => {
                eprintln!(
                    "[image] no flake.lock hash recorded for '{project}', rebuilding image..."
                );
                true
            }
            Some(recorded) if recorded != &current_hash => {
                eprintln!(
                    "[image] flake.lock changed for '{project}', rebuilding per-project image..."
                );
                true
            }
            _ => {
                eprintln!(
                    "[image] per-project image '{tag}' is up to date, skipping build"
                );
                false
            }
        }
    } else {
        eprintln!("[image] per-project image '{tag}' not found, building...");
        true
    };

    if !needs_build {
        return Ok(());
    }

    // Build the image using a temp context dir
    let context_dir =
        tempfile::tempdir().context("failed to create temp dir for project image build")?;

    // Write flake.nix and flake.lock into the context dir
    std::fs::write(context_dir.path().join("flake.nix"), flake_nix_content)
        .context("failed to write flake.nix to build context")?;
    std::fs::write(context_dir.path().join("flake.lock"), flake_lock_content)
        .context("failed to write flake.lock to build context")?;

    let dockerfile_content = project_dockerfile(flake_nix_content, flake_lock_content);
    let dockerfile_path = context_dir.path().join("Dockerfile");
    let mut f = std::fs::File::create(&dockerfile_path)
        .context("failed to create Dockerfile in build context")?;
    f.write_all(dockerfile_content.as_bytes())
        .context("failed to write Dockerfile")?;
    drop(f);

    container
        .build_image(&tag, context_dir.path())
        .with_context(|| format!("failed to build per-project image '{tag}'"))?;

    // Record the new hash only after a successful build
    store
        .set_flake_lock_hash(project, &current_hash)
        .context("failed to record flake.lock hash after build")?;

    eprintln!("[image] per-project image '{tag}' built successfully");
    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    // Simple FNV-based hash for now; replaced with a real SHA-256 if sha2 is added.
    // For correctness we use a deterministic but simple approach.
    // Since sha2 is not in the deps, we implement a minimal SHA-256.
    use std::fmt::Write as FmtWrite;
    let hash = sha256(data);
    let mut s = String::with_capacity(64);
    for byte in &hash {
        write!(s, "{byte:02x}").unwrap();
    }
    s
}

/// Minimal SHA-256 implementation to avoid adding a dependency.
#[allow(clippy::unreadable_literal, clippy::many_single_char_names)]
fn sha256(data: &[u8]) -> [u8; 32] {
    // SHA-256 constants
    let k: [u32; 64] = [
        0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5,
        0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
        0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3,
        0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
        0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc,
        0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
        0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7,
        0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
        0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13,
        0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
        0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3,
        0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
        0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5,
        0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
        0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208,
        0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a,
        0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19,
    ];

    // Pre-processing: padding
    let bit_len = (data.len() as u64) * 8;
    let mut msg: Vec<u8> = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit chunk
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i*4], chunk[i*4+1], chunk[i*4+2], chunk[i*4+3]]);
        }
        for i in 16..64 {
            let s0 = w[i-15].rotate_right(7) ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
            let s1 = w[i-2].rotate_right(17) ^ w[i-2].rotate_right(19) ^ (w[i-2] >> 10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(k[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g; g = f; f = e;
            e = d.wrapping_add(temp1);
            d = c; c = b; b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        result[i*4..(i+1)*4].copy_from_slice(&word.to_be_bytes());
    }
    result
}

#[cfg(test)]
mod tests {
    use crate::backend::mock::MockContainerBackend;
    use crate::store::Store;

    use super::*;

    fn make_store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn setup_project(store: &Store) {
        store.add_project("myproject", "https://example.com/foo.git").unwrap();
    }

    #[test]
    fn base_absent_build_called() {
        let container = MockContainerBackend::new();
        ensure_base_image(&container).unwrap();
        let calls = container.recorded_calls();
        assert!(calls.contains(&"image_exists:nixsand-base".to_string()));
        assert!(calls.contains(&"build_image:nixsand-base".to_string()));
    }

    #[test]
    fn base_present_build_not_called() {
        let container = MockContainerBackend::with_existing_images(&["nixsand-base"]);
        ensure_base_image(&container).unwrap();
        let calls = container.recorded_calls();
        assert!(calls.contains(&"image_exists:nixsand-base".to_string()));
        assert!(!calls.contains(&"build_image:nixsand-base".to_string()));
    }

    #[test]
    fn project_image_absent_build_called() {
        let container = MockContainerBackend::with_existing_images(&["nixsand-base"]);
        let store = make_store();
        setup_project(&store);

        ensure_project_image("myproject", &container, &store, b"flake.nix", b"flake.lock").unwrap();

        let calls = container.recorded_calls();
        assert!(calls.contains(&"image_exists:nixsand-myproject".to_string()));
        assert!(calls.contains(&"build_image:nixsand-myproject".to_string()));
    }

    #[test]
    fn project_image_present_same_hash_no_rebuild() {
        let store = make_store();
        setup_project(&store);

        let flake_lock = b"flake.lock content";
        let hash = sha256_hex(flake_lock);
        store.set_flake_lock_hash("myproject", &hash).unwrap();

        let container =
            MockContainerBackend::with_existing_images(&["nixsand-base", "nixsand-myproject"]);

        ensure_project_image("myproject", &container, &store, b"flake.nix", flake_lock).unwrap();

        let calls = container.recorded_calls();
        assert!(!calls.contains(&"build_image:nixsand-myproject".to_string()));
    }

    #[test]
    fn project_image_present_different_hash_rebuild() {
        let store = make_store();
        setup_project(&store);
        store.set_flake_lock_hash("myproject", "oldhash").unwrap();

        let container =
            MockContainerBackend::with_existing_images(&["nixsand-base", "nixsand-myproject"]);

        ensure_project_image("myproject", &container, &store, b"flake.nix", b"new content").unwrap();

        let calls = container.recorded_calls();
        assert!(calls.contains(&"build_image:nixsand-myproject".to_string()));
    }

    #[test]
    fn build_failure_no_state_recorded() {
        let store = make_store();
        setup_project(&store);

        let container = MockContainerBackend::new();
        {
            let mut errors = container.build_errors.lock().unwrap();
            errors.insert("nixsand-myproject".to_string(), "simulated build failure".to_string());
        }

        let result =
            ensure_project_image("myproject", &container, &store, b"flake.nix", b"flake.lock");
        assert!(result.is_err());

        // Hash should NOT be recorded since build failed
        assert_eq!(store.get_flake_lock_hash("myproject").unwrap(), None);
    }
}
