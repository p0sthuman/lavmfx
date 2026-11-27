//! Windows: compile `assets/lavmfx.rc` (icon + version info) and link it into
//! lavmfx.exe. Uses `RC_<target>` / `RC` (e.g. llvm-rc when cross-building),
//! else `llvm-rc`; on MSVC hosts `rc.exe`. Nothing to do on other systems.
use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/lavmfx.rc");
    println!("cargo:rerun-if-changed=assets/lavmfx.ico");
    build_stamp();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let target = env::var("TARGET").unwrap_or_default().replace('-', "_");
    let rc = env::var(format!("RC_{target}"))
        .or_else(|_| env::var("RC"))
        .unwrap_or_else(|_| {
            if target.ends_with("msvc") {
                "rc".into()
            } else {
                "llvm-rc".into()
            }
        });
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("lavmfx.res");
    let assets = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    let status = Command::new(&rc)
        .current_dir(&assets)
        .args(["/nologo", "/fo"])
        .arg(&out)
        .arg("lavmfx.rc")
        .status();
    match status {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", out.display()),
        // No resource compiler: the exe just has no icon.
        other => println!("cargo:warning=no exe icon: {rc} failed ({other:?})"),
    }
}

/// `LAVMFX_BUILD` = "<git commit>[-dirty] <UTC build time>", logged at start
/// so a log tells which build wrote it. Without git (the container build),
/// `LAVMFX_GIT_COMMIT` from the build script gives the commit; else "unknown".
fn build_stamp() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-env-changed=LAVMFX_GIT_COMMIT");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let (commit, dirty) = match git(&["rev-parse", "--short", "HEAD"]) {
        Some(c) => (
            c,
            git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty()),
        ),
        // Already "<commit>[-dirty]".
        None => (
            env::var("LAVMFX_GIT_COMMIT").unwrap_or_else(|_| "unknown".into()),
            false,
        ),
    };
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    // Days since 1970-01-01 → civil date (Howard Hinnant's algorithm).
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    println!(
        "cargo:rustc-env=LAVMFX_BUILD={commit}{} {y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        if dirty { "-dirty" } else { "" },
        rem / 3600,
        rem % 3600 / 60
    );
}
