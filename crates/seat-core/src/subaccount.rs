//! Sub-account management: dedicated Linux user accounts nested and protected by the parent.
//!
//! Sub-accounts run under their own persistent UID/GID and home directory (/home/<name>).
//! By default, they have ZERO access to the parent's home directory (/home/raymondj), no
//! sudo privileges, and locked passwords (no direct login).
//!
//! The parent grants access only to specific project folders via POSIX ACLs (`setfacl`) and
//! symlinks, and can terminate or inspect sub-account processes at any time.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAccountInfo {
    pub username: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shared_paths: Vec<PathBuf>,
    pub created_at_unix: u64,
}

pub fn subaccount_registry_dir() -> Result<PathBuf> {
    let dir = PathBuf::from("/home/raymondj/.config/input-director/subaccounts");
    if !dir.exists() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    }
    Ok(dir)
}

fn account_file(username: &str) -> Result<PathBuf> {
    Ok(subaccount_registry_dir()?.join(format!("{username}.json")))
}

pub fn get_user_id(username: &str) -> Option<(u32, u32, PathBuf)> {
    let passwd = fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() >= 6 && parts[0] == username {
            let uid: u32 = parts[2].parse().ok()?;
            let gid: u32 = parts[3].parse().ok()?;
            let home = PathBuf::from(parts[5]);
            return Some((uid, gid, home));
        }
    }
    None
}

/// List all registered subaccounts.
pub fn list() -> Result<Vec<SubAccountInfo>> {
    let dir = subaccount_registry_dir()?;
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Ok(out);
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().map_or(false, |ext| ext == "json") {
            if let Ok(data) = fs::read(&path) {
                if let Ok(info) = serde_json::from_slice::<SubAccountInfo>(&data) {
                    out.push(info);
                }
            }
        }
    }
    // Also include existing system accounts like 'james' or 'genysis' if present
    for sys in ["james", "genysis"] {
        if !out.iter().any(|a| a.username == sys) {
            if let Some((uid, gid, home)) = get_user_id(sys) {
                out.push(SubAccountInfo {
                    username: sys.to_string(),
                    uid,
                    gid,
                    home,
                    shared_paths: vec![],
                    created_at_unix: 0,
                });
            }
        }
    }
    out.sort_by(|a, b| a.username.cmp(&b.username));
    Ok(out)
}

/// Create a new dedicated Linux sub-account.
/// Requires pkexec/sudo elevation for useradd and permissions setup.
pub fn create(username: &str, shared_dirs: &[PathBuf]) -> Result<SubAccountInfo> {
    crate::session::validate_name(username)?;

    if get_user_id(username).is_none() {
        // Run useradd
        let status = Command::new("pkexec")
            .args(["useradd", "-m", "-s", "/bin/bash", username])
            .status()
            .context("failed to execute pkexec useradd")?;
        if !status.success() {
            bail!("pkexec useradd failed with exit code {:?}", status.code());
        }

        // Lock password
        let _ = Command::new("pkexec")
            .args(["passwd", "-l", username])
            .status();

        // Lock down subaccount home directory to 0750
        let _ = Command::new("pkexec")
            .args(["chmod", "750", &format!("/home/{username}")])
            .status();

        // Add parent sudoers rule so parent user can run commands as this subaccount without prompt
        let sudoers_entry = format!("raymondj ALL=({username}) NOPASSWD: ALL\n");
        let sudoers_file = format!("/etc/sudoers.d/99-input-director-{username}");
        let cmd = format!("printf '%s' '{sudoers_entry}' > '{sudoers_file}' && chmod 440 '{sudoers_file}'");
        let _ = Command::new("pkexec")
            .args(["sh", "-c", &cmd])
            .status();
    }

    let (uid, gid, home) = get_user_id(username)
        .ok_or_else(|| anyhow!("user '{username}' does not exist after creation"))?;

    let mut info = SubAccountInfo {
        username: username.to_string(),
        uid,
        gid,
        home,
        shared_paths: Vec::new(),
        created_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };

    for dir in shared_dirs {
        if let Ok(()) = grant_folder(username, dir, false) {
            info.shared_paths.push(dir.clone());
        }
    }

    save(&info)?;
    Ok(info)
}

fn save(info: &SubAccountInfo) -> Result<()> {
    let path = account_file(&info.username)?;
    fs::write(path, serde_json::to_vec_pretty(info)?)?;
    Ok(())
}

/// Grant a subaccount read/write access to a specific folder in the parent's directory.
pub fn grant_folder(username: &str, folder: &Path, readonly: bool) -> Result<()> {
    if !folder.exists() {
        bail!("path '{}' does not exist", folder.display());
    }

    let parent_home = "/home/raymondj";
    // 1. Grant traversal on parent home
    let status = Command::new("setfacl")
        .args(["-m", &format!("u:{username}:rx"), parent_home])
        .status()?;
    if !status.success() {
        bail!("failed to grant traversal on parent home");
    }

    // 2. Grant permissions on target directory
    let perms = if readonly { "rx" } else { "rwx" };
    let status = Command::new("setfacl")
        .args(["-R", "-m", &format!("u:{username}:{perms}"), &folder.display().to_string()])
        .status()?;
    if !status.success() {
        bail!("failed to set ACL on folder");
    }

    // Also set default ACL so new files inherit access
    let _ = Command::new("setfacl")
        .args(["-d", "-R", "-m", &format!("u:{username}:{perms}"), &folder.display().to_string()])
        .status();

    // 3. Create symlink in subaccount's home directory if possible
    if let Some((_, _, home)) = get_user_id(username) {
        if let Some(name) = folder.file_name() {
            let link = home.join(name);
            let cmd = format!("ln -sfn '{}' '{}' && chown -h {username}:{username} '{}'", folder.display(), link.display(), link.display());
            let _ = Command::new("pkexec")
                .args(["sh", "-c", &cmd])
                .status();
        }
    }

    // Update recorded paths
    if let Ok(mut info) = load(username) {
        if !info.shared_paths.iter().any(|p| p == folder) {
            info.shared_paths.push(folder.to_path_buf());
            let _ = save(&info);
        }
    }

    Ok(())
}

/// Revoke a subaccount's access to a shared folder.
pub fn revoke_folder(username: &str, folder: &Path) -> Result<()> {
    let _ = Command::new("setfacl")
        .args(["-R", "-x", &format!("u:{username}"), &folder.display().to_string()])
        .status();
    let _ = Command::new("setfacl")
        .args(["-d", "-R", "-x", &format!("u:{username}"), &folder.display().to_string()])
        .status();

    if let Some((_, _, home)) = get_user_id(username) {
        if let Some(name) = folder.file_name() {
            let link = home.join(name);
            let cmd = format!("rm -f '{}'", link.display());
            let _ = Command::new("pkexec")
                .args(["sh", "-c", &cmd])
                .status();
        }
    }

    if let Ok(mut info) = load(username) {
        info.shared_paths.retain(|p| p != folder);
        let _ = save(&info);
    }

    Ok(())
}

pub fn load(username: &str) -> Result<SubAccountInfo> {
    let path = account_file(username)?;
    if path.exists() {
        let data = fs::read(&path)?;
        return Ok(serde_json::from_slice(&data)?);
    }
    // Fallback for system accounts
    if let Some((uid, gid, home)) = get_user_id(username) {
        return Ok(SubAccountInfo {
            username: username.to_string(),
            uid,
            gid,
            home,
            shared_paths: vec![],
            created_at_unix: 0,
        });
    }
    bail!("no such subaccount '{username}'")
}

/// Grant access to the Wayland seat socket so the subaccount can display onto the seat.
pub fn grant_seat_access(username: &str, socket_path: &Path) -> Result<()> {
    let runtime = crate::session::runtime_dir()?;
    // Traversal on XDG_RUNTIME_DIR
    let _ = Command::new("setfacl")
        .args(["-m", &format!("u:{username}:rx"), &runtime.display().to_string()])
        .status();

    // Read/write on the socket file
    let _ = Command::new("setfacl")
        .args(["-m", &format!("u:{username}:rw"), &socket_path.display().to_string()])
        .status();

    Ok(())
}
