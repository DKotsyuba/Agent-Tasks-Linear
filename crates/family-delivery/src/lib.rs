//! Immutable local delivery for the single-binary profile, with or without
//! local state. A release declares the state schema it expects; activation
//! refuses to run an older schema against newer on-disk state, and never
//! touches anything outside `<home>/standalone/`.
//!
//! Product adaptation (agent-tasks): this product is `resident` with
//! `external` state. Its manifests therefore carry `state_schema = 0`, which
//! here means **no local business state** (workflow truth lives in Linear) —
//! it does not relabel the product as a stateless `none` profile. The
//! owner's config, credentials and any local directories outside
//! `standalone/` survive install, upgrade, rollback and legacy adoption
//! unchanged.
//!
//! Hash verification proves integrity, not provenance. Callers authenticate downloads.
//!
//! Copied from the family template at commit 7f094e0 (MIT; see
//! docs/TEMPLATE_MIT_LICENSE.txt) with the documented adaptations above and
//! the explicit legacy-launcher adoption route below.
#![forbid(unsafe_code)]
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Delivery errors never contain credentials or raw provider bodies.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
/// Only one bounded executable and a small manifest are accepted.
pub const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;
/// The template's local-state layout version. This product packages with 0
/// (external state); the value only bounds accepted manifests.
pub const CURRENT_STATE_SCHEMA: u32 = 1;

/// The explicit single-binary profile is separate from the older bundle manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Version of this file contract.
    pub schema_version: u32,
    /// Must equal single-binary-v1.
    pub profile: String,
    /// Product/binary identity.
    pub product: String,
    /// Immutable product release version.
    pub version: String,
    /// Exact source commit, not a branch.
    pub source_commit: String,
    /// Rust target triple.
    pub target: String,
    /// Bare release asset name.
    pub binary: String,
    /// Exact byte length.
    pub size: u64,
    /// SHA-256 of the executable.
    pub sha256: String,
    /// State schema the release expects: 0 = no local business state,
    /// 1 = `CURRENT_STATE_SCHEMA`.
    pub state_schema: u32,
    /// GitHub run identity, when built there.
    pub run_id: Option<u64>,
    /// GitHub run attempt, when built there.
    pub run_attempt: Option<u64>,
}
/// Restrict path components; callers must never sanitize a component into a different identity.
pub fn component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn regular(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err("expected a regular file, not a link".into());
    }
    Ok(())
}
/// Compute a bounded streaming digest without reading the entire binary into memory.
pub fn digest(path: &Path) -> Result<(u64, String)> {
    regular(path)?;
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let (mut total, mut buffer) = (0u64, [0u8; 65536]);
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total = total.checked_add(n as u64).ok_or("size overflow")?;
        if total > MAX_BINARY_BYTES {
            return Err("binary exceeds size limit".into());
        }
        hasher.update(&buffer[..n]);
    }
    Ok((total, format!("{:x}", hasher.finalize())))
}
impl Manifest {
    /// Check owned fields before using any path or identity from the manifest.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.profile != "single-binary-v1"
            || self.state_schema > CURRENT_STATE_SCHEMA
        {
            return Err("unsupported delivery or state profile".into());
        }
        if !self.product.starts_with("agent-")
            || !component(&self.product)
            || !component(&self.version)
            || !component(&self.target)
            || !component(&self.binary)
            || self.binary != format!("{}-{}", self.product, self.target)
            || self.source_commit.len() != 40
            || !self.source_commit.bytes().all(|b| b.is_ascii_hexdigit())
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || self.size == 0
            || self.size > MAX_BINARY_BYTES
            || self.run_id.is_some() != self.run_attempt.is_some()
            || self.run_id == Some(0)
            || self.run_attempt == Some(0)
        {
            return Err("invalid delivery manifest".into());
        }
        Ok(())
    }
}
/// Validate a downloaded directory. Extra files, links and mismatching bytes are refused.
pub fn verify(bundle: &Path) -> Result<Manifest> {
    if !fs::symlink_metadata(bundle)?.is_dir() {
        return Err("bundle must be a real directory".into());
    }
    let manifest_path = bundle.join("release-manifest.json");
    regular(&manifest_path)?;
    if fs::metadata(&manifest_path)?.len() > 16384 {
        return Err("manifest too large".into());
    }
    let m: Manifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    m.validate()?;
    let entries: Vec<_> = fs::read_dir(bundle)?.collect::<std::io::Result<_>>()?;
    if entries.len() != 2
        || entries
            .iter()
            .any(|e| e.file_name() != "release-manifest.json" && e.file_name() != m.binary.as_str())
    {
        return Err("unexpected bundle inventory".into());
    }
    let (size, sha) = digest(&bundle.join(&m.binary))?;
    if size != m.size || sha != m.sha256 {
        return Err("binary integrity mismatch".into());
    }
    Ok(m)
}
/// Write a newly built binary to a NEW bundle directory. No existing version is overwritten.
pub fn package(binary: &Path, output: &Path, mut manifest: Manifest) -> Result<Manifest> {
    let (size, sha) = digest(binary)?;
    manifest.size = size;
    manifest.sha256 = sha;
    manifest.validate()?;
    fs::create_dir(output)?;
    fs::copy(binary, output.join(&manifest.binary))?;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("release-manifest.json"))?;
    out.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
    out.sync_all()?;
    verify(output)
}
fn private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if !m.is_dir() => return Err("installation path is a link or non-directory".into()),
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(path)?,
        Err(e) => return Err(e.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn layout(home: &Path) -> Result<(PathBuf, File)> {
    if !home.is_absolute() {
        return Err("installation home must be absolute".into());
    }
    private_dir(home)?;
    let base = home.join("standalone");
    private_dir(&base)?;
    private_dir(&base.join("releases"))?;
    let lock_path = base.join("install.lock");
    if fs::symlink_metadata(&lock_path).is_ok() {
        regular(&lock_path)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    FileExt::lock_exclusive(&file)?;
    Ok((base, file))
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
/// Whether the launcher path holds this product's managed launcher.
fn managed_launcher(launcher: &Path, product: &str) -> Result<bool> {
    match fs::symlink_metadata(launcher) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
        Ok(_) => {
            regular(launcher)?;
            if fs::metadata(launcher)?.len() > 8192 {
                return Ok(false);
            }
            let marker = format!("#!/bin/sh\n# {product} managed launcher v1\n");
            Ok(fs::read_to_string(launcher)?.starts_with(&marker))
        }
    }
}
#[cfg(unix)]
fn activate(base: &Path, manifest: &Manifest, bin_dir: &Path) -> Result<()> {
    use std::os::unix::{fs::PermissionsExt, fs::symlink};
    if !bin_dir.is_absolute() {
        return Err("bin directory must be absolute".into());
    }
    if !bin_dir.is_dir() {
        return Err("create the bin directory explicitly before installation".into());
    }
    let launcher = bin_dir.join(&manifest.product);
    if fs::symlink_metadata(&launcher).is_ok() && !managed_launcher(&launcher, &manifest.product)? {
        return Err(
            "refusing to replace an unmanaged launcher; use the explicit adoption path".into(),
        );
    }
    let current = base.join("current");
    if let Ok(m) = fs::symlink_metadata(&current) {
        if !m.file_type().is_symlink() {
            return Err("current must be a managed symlink".into());
        }
        let previous = fs::read_link(&current)?;
        if previous.is_absolute()
            || !previous.starts_with("releases")
            || previous.components().count() != 2
        {
            return Err("unmanaged current target".into());
        }
        let old = verify(&base.join(previous))?;
        if old.product != manifest.product || old.target != manifest.target {
            return Err("incompatible previous installation".into());
        }
        // Never run an older state schema against newer on-disk state; same or
        // newer is fine (newer code reads or migrates the older state).
        if manifest.state_schema < old.state_schema {
            return Err("release predates the state schema already in use".into());
        }
    }
    // Preserve executable identity for every new process; it does not repeatedly resolve current.
    // The launcher also pins the installation default through ATL_CONFIG —
    // only when the caller has not set it — and forwards argv unchanged, so
    // precedence stays: explicit --config > explicit ATL_CONFIG > pinned
    // installation default. A child that changes HOME cannot redirect the
    // installed product to an empty config, and existing wrappers that pass
    // their own --config keep working (no duplicate argument).
    let marker = format!("#!/bin/sh\n# {} managed launcher v1\n", manifest.product);
    let body = format!(
        "{marker}set -eu\nhome={}\nbase=\"$home/standalone\"\nrelease=$(readlink \"$base/current\")\nif [ -z \"${{ATL_CONFIG:-}}\" ] && [ -f \"$home/config.toml\" ]; then\n  ATL_CONFIG=\"$home/config.toml\"\n  export ATL_CONFIG\nfi\nexec \"$base/$release/{}\" \"$@\"\n",
        shell_quote(
            base.parent()
                .ok_or("installation base without a home parent")?
                .to_str()
                .ok_or("non-UTF8 installation path")?
        ),
        manifest.binary
    );
    let temp_launcher = bin_dir.join(format!(
        ".{}.install-{}",
        manifest.product,
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_launcher)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(&temp_launcher, fs::Permissions::from_mode(0o755))?;
    let next = base.join(format!(".current-{}", std::process::id()));
    symlink(Path::new("releases").join(&manifest.version), &next)?;
    // Both versions use the same target-specific asset name. Existing services are not restarted.
    fs::rename(&temp_launcher, &launcher)?;
    fs::rename(&next, current)?;
    Ok(())
}
#[cfg(not(unix))]
fn activate(_: &Path, _: &Manifest, _: &Path) -> Result<()> {
    Err("installer supports Unix only".into())
}
/// Stage the release payload into the immutable versions tree under an
/// already-held installation lock. Every install-side refusal (conflicting
/// immutable version, damaged bundle) happens here, before anything at the
/// launcher path is touched.
fn stage_release(base: &Path, bundle: &Path, manifest: &Manifest) -> Result<()> {
    let destination = base.join("releases").join(&manifest.version);
    if fs::symlink_metadata(&destination).is_ok() {
        if verify(&destination)? != *manifest {
            return Err("immutable version already exists with different bytes or metadata".into());
        }
    } else {
        let stage = base.join(format!(".install-{}", std::process::id()));
        fs::create_dir(&stage)?;
        fs::copy(bundle.join(&manifest.binary), stage.join(&manifest.binary))?;
        fs::copy(
            bundle.join("release-manifest.json"),
            stage.join("release-manifest.json"),
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                stage.join(&manifest.binary),
                fs::Permissions::from_mode(0o755),
            )?;
        }
        verify(&stage)?;
        fs::rename(stage, &destination)?;
    }
    Ok(())
}
/// Install the release payload under the exclusive lock. Does not migrate,
/// prune, restart services or change host configuration.
pub fn install(bundle: &Path, home: &Path, bin_dir: &Path) -> Result<Manifest> {
    let manifest = verify(bundle)?;
    let (base, _lock) = layout(home)?;
    stage_release(&base, bundle, &manifest)?;
    activate(&base, &manifest, bin_dir)?;
    Ok(manifest)
}
/// Activate a retained version only after verifying its integrity and state profile.
pub fn use_version(home: &Path, bin_dir: &Path, version: &str) -> Result<Manifest> {
    if !component(version) {
        return Err("invalid version component".into());
    }
    let (base, _lock) = layout(home)?;
    let manifest = verify(&base.join("releases").join(version))?;
    activate(&base, &manifest, bin_dir)?;
    Ok(manifest)
}
/// Evidence recorded when a pre-template executable is adopted.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Adoption {
    /// Version the legacy executable reported for this product.
    pub version: String,
    /// SHA-256 of the adopted legacy bytes, for audit and verification.
    pub sha256: String,
    /// Where the untouched legacy executable was preserved.
    pub backup: PathBuf,
}
/// Restore the verified legacy bytes to the launcher path after a failed
/// adoption. A file that appeared concurrently at that path is never
/// overwritten; the backup always survives either way.
fn restore_legacy(legacy: &Path, backup: &Path, sha256: &str) -> Result<()> {
    if fs::symlink_metadata(legacy).is_ok() {
        return Err("launcher path changed during the failed adoption; backup preserved".into());
    }
    fs::copy(backup, legacy)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(legacy, fs::Permissions::from_mode(0o755))?;
    }
    if digest(legacy)?.1 != sha256 {
        return Err("legacy restore verification failed; backup preserved".into());
    }
    Ok(())
}
/// Explicitly adopt a known legacy launcher: verify the unmanaged file is
/// really this product (its `--version` output must identify the same
/// product), preserve it byte-for-byte next to the launcher, then install the
/// managed release — all under one installation lock, with every install-side
/// refusal preflighted before the stable launcher path is touched and the
/// verified backup restored if activation still fails. Foreign or
/// unrecognized executables are always refused.
pub fn adopt_legacy_launcher(
    bundle: &Path,
    home: &Path,
    bin_dir: &Path,
) -> Result<(Manifest, Adoption)> {
    let manifest = verify(bundle)?;
    if fs::symlink_metadata(bin_dir.join(&manifest.product)).is_err() {
        return Err("no existing launcher to adopt; use self-install".into());
    }
    let legacy = bin_dir.join(&manifest.product);
    regular(&legacy)?;
    if managed_launcher(&legacy, &manifest.product)? {
        return Err("launcher is already managed; use self-install".into());
    }
    // Identity check: the file must identify itself as this exact product.
    let identified = std::process::Command::new(&legacy)
        .arg("--version")
        .output()
        .map_err(|_| "legacy launcher could not be executed for an identity check")?;
    if !identified.status.success() {
        return Err("legacy launcher --version failed; refusing adoption".into());
    }
    let reported = String::from_utf8_lossy(&identified.stdout)
        .trim()
        .to_owned();
    let expected_prefix = format!("{} ", manifest.product);
    let Some(version) = reported.strip_prefix(&expected_prefix) else {
        return Err(format!(
            "launcher identifies as {reported:?}, not this product; refusing adoption"
        )
        .into());
    };
    let version = version.to_owned();
    if !component(&version) {
        return Err("legacy launcher reported an invalid version".into());
    }
    let (_, sha256) = digest(&legacy)?;
    let backup = bin_dir.join(format!("{}-legacy-{}", manifest.product, version));
    if fs::symlink_metadata(&backup).is_ok() {
        return Err("legacy backup already exists; inspect it before adoption".into());
    }
    fs::copy(&legacy, &backup)?;
    if digest(&backup)?.1 != sha256 {
        return Err("legacy backup verification failed".into());
    }
    // One lock covers staging, replacement and activation. Staging preflights
    // every install-side refusal (conflicting immutable version, damaged
    // payload) before the stable launcher path is removed.
    let (base, _lock) = layout(home)?;
    stage_release(&base, bundle, &manifest)?;
    fs::remove_file(&legacy)?;
    match activate(&base, &manifest, bin_dir) {
        Ok(()) => Ok((
            manifest,
            Adoption {
                version,
                sha256,
                backup,
            },
        )),
        Err(error) => {
            restore_legacy(&legacy, &backup, &sha256)?;
            Err(error)
        }
    }
}
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Test fixtures fail explicitly"
)]
mod tests {
    use super::*;

    fn fixture(root: &Path, version: &str, state_schema: u32) -> PathBuf {
        let binary = root.join(format!("source-{version}"));
        fs::write(&binary, b"fixture, not executable").unwrap();
        let bundle = root.join(format!("bundle-{version}"));
        package(
            &binary,
            &bundle,
            Manifest {
                schema_version: 1,
                profile: "single-binary-v1".into(),
                product: "agent-test".into(),
                version: version.into(),
                source_commit: "a".repeat(40),
                target: "aarch64-apple-darwin".into(),
                binary: "agent-test-aarch64-apple-darwin".into(),
                size: 1,
                sha256: "0".repeat(64),
                state_schema,
                run_id: None,
                run_attempt: None,
            },
        )
        .unwrap();
        bundle
    }
    #[test]
    fn verified_bundle() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(
            verify(&fixture(t.path(), "0.1.0", 0)).unwrap().version,
            "0.1.0"
        );
    }
    #[test]
    fn tampered_binary_rejected() {
        let t = tempfile::tempdir().unwrap();
        let b = fixture(t.path(), "0.1.0", 0);
        fs::write(b.join("agent-test-aarch64-apple-darwin"), b"tampered").unwrap();
        assert!(verify(&b).is_err());
    }
    #[test]
    fn extra_file_rejected() {
        let t = tempfile::tempdir().unwrap();
        let b = fixture(t.path(), "0.1.0", 0);
        fs::write(b.join("extra"), b"x").unwrap();
        assert!(verify(&b).is_err());
    }
    #[test]
    fn traversal_rejected() {
        for s in ["..", "../x", "/tmp/x", "x/y", "x\\y", ""] {
            assert!(!component(s));
        }
    }
    #[cfg(unix)]
    #[test]
    fn binary_symlink_rejected() {
        let t = tempfile::tempdir().unwrap();
        let b = fixture(t.path(), "0.1.0", 0);
        let p = b.join("agent-test-aarch64-apple-darwin");
        fs::remove_file(&p).unwrap();
        std::os::unix::fs::symlink(t.path().join("source-0.1.0"), p).unwrap();
        assert!(verify(&b).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn install_noop_and_rollback() {
        let t = tempfile::tempdir().unwrap();
        let h = t.path().join("home");
        let bin = t.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let a = fixture(t.path(), "0.1.0", 0);
        let b = fixture(t.path(), "0.2.0", 0);
        install(&a, &h, &bin).unwrap();
        install(&a, &h, &bin).unwrap();
        install(&b, &h, &bin).unwrap();
        use_version(&h, &bin, "0.1.0").unwrap();
        assert_eq!(
            fs::read_link(h.join("standalone/current")).unwrap(),
            PathBuf::from("releases/0.1.0")
        );
    }
    #[cfg(unix)]
    #[test]
    fn stateful_upgrade_and_rollback_keep_owner_state_intact() {
        let t = tempfile::tempdir().unwrap();
        let h = t.path().join("home");
        // The owner's pre-existing local state, outside `standalone/`.
        fs::create_dir_all(h.join("state/v1")).unwrap();
        fs::write(h.join("config.toml"), b"[storage]\nroot = \"/tmp/wt\"\n").unwrap();
        fs::write(h.join("state/v1/registry.json"), b"{\"entries\":[]}\n").unwrap();
        let (config, registry) = (
            fs::read(h.join("config.toml")).unwrap(),
            fs::read(h.join("state/v1/registry.json")).unwrap(),
        );
        let bin = t.path().join("bin");
        fs::create_dir(&bin).unwrap();
        install(&fixture(t.path(), "0.1.0", CURRENT_STATE_SCHEMA), &h, &bin).unwrap();
        install(&fixture(t.path(), "0.2.0", CURRENT_STATE_SCHEMA), &h, &bin).unwrap();
        use_version(&h, &bin, "0.1.0").unwrap();
        assert_eq!(
            fs::read_link(h.join("standalone/current")).unwrap(),
            PathBuf::from("releases/0.1.0")
        );
        assert_eq!(fs::read(h.join("config.toml")).unwrap(), config);
        assert_eq!(
            fs::read(h.join("state/v1/registry.json")).unwrap(),
            registry
        );
    }
    /// This product's actual profile: state_schema = 0 (external Linear state)
    /// must install, upgrade and roll back while preserving owner files.
    #[cfg(unix)]
    #[test]
    fn external_state_schema_zero_keeps_owner_files_intact() {
        let t = tempfile::tempdir().unwrap();
        let h = t.path().join("home");
        fs::create_dir(&h).unwrap();
        fs::write(h.join("config.toml"), b"listen = \"127.0.0.1:8777\"\n").unwrap();
        let config = fs::read(h.join("config.toml")).unwrap();
        let bin = t.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let m = install(&fixture(t.path(), "0.5.0", 0), &h, &bin).unwrap();
        assert_eq!(m.state_schema, 0);
        install(&fixture(t.path(), "0.5.1", 0), &h, &bin).unwrap();
        use_version(&h, &bin, "0.5.0").unwrap();
        assert_eq!(fs::read(h.join("config.toml")).unwrap(), config);
        assert!(!h.join("state").exists(), "no local business state created");
    }
    #[cfg(unix)]
    #[test]
    fn older_state_schema_cannot_run_over_newer_state() {
        let t = tempfile::tempdir().unwrap();
        let h = t.path().join("home");
        let bin = t.path().join("bin");
        fs::create_dir(&bin).unwrap();
        install(&fixture(t.path(), "0.1.0", CURRENT_STATE_SCHEMA), &h, &bin).unwrap();
        assert!(install(&fixture(t.path(), "0.2.0", 0), &h, &bin).is_err());
        assert!(use_version(&h, &bin, "0.2.0").is_err());
        assert_eq!(
            fs::read_link(h.join("standalone/current")).unwrap(),
            PathBuf::from("releases/0.1.0")
        );
    }
    #[cfg(unix)]
    #[test]
    fn unmanaged_launcher_preserved() {
        let t = tempfile::tempdir().unwrap();
        let b = fixture(t.path(), "0.1.0", 0);
        let bin = t.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::write(bin.join("agent-test"), b"mine").unwrap();
        assert!(install(&b, &t.path().join("home"), &bin).is_err());
        assert_eq!(fs::read(bin.join("agent-test")).unwrap(), b"mine");
    }
    /// A rejected adoption never leaves the stable launcher path missing:
    /// install-side refusals preflight before removal, and an activation
    /// failure after removal restores the verified legacy bytes; the owner's
    /// config is untouched in both cases.
    #[cfg(unix)]
    #[test]
    fn failed_adoption_leaves_prior_launcher_and_config_intact() {
        let t = tempfile::tempdir().unwrap();
        let h = t.path().join("home");
        let bin = t.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&h).unwrap();
        fs::write(h.join("config.toml"), b"listen = \"127.0.0.1:8777\"\n").unwrap();
        let config = fs::read(h.join("config.toml")).unwrap();
        let script = b"#!/bin/sh\necho agent-test 0.4.0\n";
        let legacy = bin.join("agent-test");
        fn install_legacy(path: &Path, script: &[u8]) {
            fs::write(path, script).unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Case 1: a conflicting immutable version (same version, different
        // bytes) is refused BEFORE the legacy executable is removed.
        install(&fixture(t.path(), "0.9.0", 0), &h, &bin).unwrap();
        fs::remove_file(&legacy).unwrap();
        install_legacy(&legacy, script);
        let conflicting = t.path().join("conflicting-source");
        fs::write(&conflicting, b"different bytes entirely").unwrap();
        let conflict_bundle =
            repackage_with_source(fixture(t.path(), "0.9.0-x", 0), &conflicting, "0.9.0");
        assert!(adopt_legacy_launcher(&conflict_bundle, &h, &bin).is_err());
        assert_eq!(fs::read(&legacy).unwrap(), script);
        assert_eq!(fs::read(h.join("config.toml")).unwrap(), config);

        // Case 2: activation refuses a state-schema downgrade after removal;
        // the verified backup restores the legacy bytes byte-identically and
        // the executable stays runnable.
        fs::remove_file(&legacy).unwrap();
        install(&fixture(t.path(), "0.8.0", CURRENT_STATE_SCHEMA), &h, &bin).unwrap();
        fs::remove_file(&legacy).unwrap();
        install_legacy(&legacy, script);
        let before = fs::read(&legacy).unwrap();
        assert!(adopt_legacy_launcher(&fixture(t.path(), "0.9.2", 0), &h, &bin).is_err());
        assert_eq!(fs::read(&legacy).unwrap(), before);
        let out = std::process::Command::new(&legacy)
            .arg("--version")
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "agent-test 0.4.0"
        );
        assert_eq!(fs::read(h.join("config.toml")).unwrap(), config);
        // The backup survives for audit.
        assert!(bin.join("agent-test-legacy-0.4.0").is_file());
    }

    /// Re-badge one fixture bundle to a version and swap its binary for other
    /// bytes, refreshing the manifest hashes so integrity checks pass.
    fn repackage_with_source(bundle: PathBuf, source: &Path, version: &str) -> PathBuf {
        let manifest_path = bundle.join("release-manifest.json");
        let mut m: Manifest = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        m.version = version.into();
        let staged = bundle.join(&m.binary);
        fs::copy(source, &staged).unwrap();
        let (size, sha) = digest(&staged).unwrap();
        m.size = size;
        m.sha256 = sha;
        fs::write(&manifest_path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
        bundle
    }
    /// A known legacy product executable is adopted with an identity check and
    /// a byte-exact backup; a foreign executable is refused untouched.
    #[cfg(unix)]
    #[test]
    fn legacy_launcher_adoption_is_explicit_backed_up_and_identity_checked() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let h = t.path().join("home");
        let bin = t.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let legacy = bin.join("agent-test");
        let script = b"#!/bin/sh\necho agent-test 0.4.0\n";
        fs::write(&legacy, script).unwrap();
        fs::set_permissions(&legacy, fs::Permissions::from_mode(0o755)).unwrap();
        let (m, adoption) = adopt_legacy_launcher(&fixture(t.path(), "0.5.0", 0), &h, &bin)
            .expect("known legacy launcher is adoptable");
        assert_eq!(m.version, "0.5.0");
        assert_eq!(adoption.version, "0.4.0");
        assert_eq!(adoption.sha256, digest(&adoption.backup).unwrap().1);
        assert_eq!(fs::read(&adoption.backup).unwrap(), script);
        let managed = fs::read_to_string(&legacy).unwrap();
        assert!(managed.starts_with("#!/bin/sh\n# agent-test managed launcher v1\n"));
        // Adopting a foreign executable must refuse and leave it untouched.
        let foreign = t.path().join("bin2");
        fs::create_dir(&foreign).unwrap();
        fs::write(
            foreign.join("agent-test"),
            b"#!/bin/sh\necho something-else 1.0\n",
        )
        .unwrap();
        fs::set_permissions(
            foreign.join("agent-test"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(
            adopt_legacy_launcher(
                &fixture(t.path(), "0.6.0", 0),
                &t.path().join("home2"),
                &foreign
            )
            .is_err()
        );
        assert!(foreign.join("agent-test").is_file());
        assert_eq!(
            fs::read(foreign.join("agent-test")).unwrap(),
            b"#!/bin/sh\necho something-else 1.0\n"
        );
    }
}
