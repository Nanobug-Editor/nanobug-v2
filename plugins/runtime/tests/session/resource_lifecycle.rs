//! Keep Rust open during repeated Vue activation and sample the actual worker's memory.

use super::*;
use serde_json::{Value, json};

const CYCLES: u64 = 30;

#[cfg(windows)]
fn memory(pid: u32) -> Value {
    use std::os::windows::process::CommandExt;

    let executable = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let query = format!(
        "$process = Get-Process -Id {pid} -ErrorAction Stop; \
         '{{0}},{{1}}' -f $process.PrivateMemorySize64,$process.WorkingSet64"
    );
    let result = std::process::Command::new(executable)
        .creation_flags(0x0800_0000)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &query,
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    let output = String::from_utf8(result.stdout).unwrap();
    let (private, working) = output.trim().split_once(',').unwrap();
    json!({"pid":pid, "private_bytes":private.parse::<u64>().unwrap(), "working_set_bytes":working.parse::<u64>().unwrap()})
}

#[cfg(target_os = "linux")]
fn memory(pid: u32) -> Value {
    let text = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).unwrap();
    let kilobytes = |label: &str| -> u64 {
        text.lines()
            .find_map(|line| line.strip_prefix(label))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    json!({
        "pid":pid,
        "resident_kib":kilobytes("Rss:"),
        "private_resident_kib":kilobytes("Private_Clean:") + kilobytes("Private_Dirty:"),
        "shared_resident_kib":kilobytes("Shared_Clean:") + kilobytes("Shared_Dirty:"),
        "proportional_resident_kib":kilobytes("Pss:"),
    })
}

/// Sample both resident pages and the system's physical footprint for this PID.
#[cfg(target_os = "macos")]
fn memory(pid: u32) -> Value {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v0>::uninit();
    // The selected flavor writes exactly a rusage_info_v0.
    let code = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V0,
            usage.as_mut_ptr().cast(),
        )
    };
    assert_eq!(code, 0, "{}", std::io::Error::last_os_error());
    let usage = unsafe { usage.assume_init() };
    json!({"pid":pid, "resident_bytes":usage.ri_resident_size, "physical_footprint_bytes":usage.ri_phys_footprint})
}

#[cfg(windows)]
fn process_gone(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;

    let executable =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/tasklist.exe");
    let result = std::process::Command::new(executable)
        .creation_flags(0x0800_0000)
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .unwrap();
    assert!(result.status.success());
    !String::from_utf8_lossy(&result.stdout).contains(&format!(",\"{pid}\","))
}

#[cfg(target_os = "linux")]
fn process_gone(pid: u32) -> bool {
    !PathBuf::from(format!("/proc/{pid}")).exists()
}

/// Signal zero asks whether the former worker is still present without signaling it.
#[cfg(target_os = "macos")]
fn process_gone(pid: u32) -> bool {
    let present = unsafe { libc::kill(pid as libc::c_int, 0) } == 0;
    !present && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[test]
fn repeated_rust_and_vue_lifecycle() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let archive = temp.path().join("language.zip");
    bundle(
        &archive,
        "^0.1.0",
        r#"
        const rust = await host.readFile('Cargo.toml');
        if (rust?.text.includes('rust_fixture')) return true;
        const vue = await host.readFile('package.json');
        return vue !== null && JSON.parse(vue.text).dependencies.vue === '3.5.0';
    "#,
        false,
        "1.0.0",
        false,
    );
    let profile = temp.path().join("profile");
    let editor = session(&profile, false);
    let namespace = install(&editor, archive, "1.0.0");
    let rust = project(temp.path(), 1);
    let vue = project(temp.path(), 2);
    fs::write(
        rust.root.join("Cargo.toml"),
        "[package]\nname = 'rust_fixture'\n",
    )?;
    fs::write(
        vue.root.join("package.json"),
        r#"{"dependencies":{"vue":"3.5.0"}}"#,
    )?;

    let rust_owner = key(&namespace, 1);
    editor.projects(Some(1), vec![rust.clone()]);
    let ready = wait(&editor, |view| {
        view.modules.get(&rust_owner) == Some(&State::Ready)
    });
    let worker = ready.workers[&namespace];
    let harness = std::process::id();
    let sample = |cycle, instances| json!({"cycle":cycle, "instances":instances, "worker":memory(worker), "host_harness":memory(harness)});
    let mut measurements = vec![sample(0, 1)];
    for cycle in 1..=CYCLES {
        let vue = Project {
            id: 2 + cycle,
            ..vue.clone()
        };
        let owner = key(&namespace, vue.id);
        editor.projects(Some(vue.id), vec![rust.clone(), vue]);
        let open = wait(&editor, |view| {
            view.modules.get(&owner) == Some(&State::Ready)
                && view
                    .runtime_status
                    .get(&namespace)
                    .is_some_and(|status| status.ready_modules == 2)
        });
        assert_eq!(open.workers[&namespace], worker);
        if cycle == 1 || cycle % 10 == 0 {
            measurements.push(sample(cycle, 2));
        }

        editor.projects(Some(1), vec![rust.clone()]);
        let closed = wait(&editor, |view| {
            !view.modules.contains_key(&owner)
                && view
                    .runtime_status
                    .get(&namespace)
                    .is_some_and(|status| status.ready_modules == 1)
        });
        assert_eq!(closed.modules.get(&rust_owner), Some(&State::Ready));
        assert_eq!(closed.workers[&namespace], worker);
        if cycle == 1 || cycle % 10 == 0 {
            measurements.push(sample(cycle, 1));
        }
    }
    editor.projects(None, Vec::new());
    wait(&editor, |view| view.workers.is_empty());
    assert!(process_gone(worker));
    measurements.push(json!({"cycle":CYCLES,"instances":0,"host_harness":memory(harness)}));
    eprintln!("P7-03 repeated Rust/Vue lifecycle {}", json!(measurements));
    Ok(())
}
