//! Builds the crate root from upstream `cli/src`.
//!
//! Upstream sources are copied into `OUT_DIR` with the edits in [`EDITS`]
//! applied, and `src/main.rs` includes the copy. The checked-in upstream
//! tree is never modified, so syncing with upstream cannot conflict; an
//! edit whose anchor no longer matches fails this build and names the file.

use std::path::{Path, PathBuf};

mod upstream {
    // Generates `cdp_generated.rs` and the dashboard placeholder.
    include!("../cli/build.rs");

    pub fn run() {
        main()
    }
}

const UPSTREAM: &str = "../cli";

enum Edit {
    /// Replace every occurrence, in every file. Must match at least once.
    Everywhere { from: &'static str, to: &'static str },
    /// Replace the single occurrence in `file`.
    Replace {
        file: &'static str,
        from: &'static str,
        to: &'static str,
    },
    /// Insert `text` after the single occurrence of `anchor` in `file`.
    After {
        file: &'static str,
        anchor: &'static str,
        text: &'static str,
    },
}

/// What the wasm32-wasip1 build changes in upstream sources, and why.
const EDITS: &[Edit] = &[
    // std traps in these two functions on WASI. The patterns have no
    // parentheses, so they also match a use as a function value.
    Edit::Everywhere {
        from: "std::env::temp_dir",
        to: "agent_browser_host::temp_dir",
    },
    Edit::Everywhere {
        from: "env::temp_dir",
        to: "agent_browser_host::temp_dir",
    },
    Edit::Everywhere {
        from: "std::process::id",
        to: "agent_browser_host::process_id",
    },
    Edit::Everywhere {
        from: "process::id",
        to: "agent_browser_host::process_id",
    },
    // std has no home directory on WASI. The host sets `HOME`.
    Edit::Everywhere {
        from: "std::env::home_dir",
        to: "agent_browser_host::home_dir",
    },
    // The package version is this crate's, not upstream's.
    Edit::Everywhere {
        from: "env!(\"CARGO_PKG_VERSION\")",
        to: "env!(\"AGENT_BROWSER_VERSION\")",
    },
    // The daemon endpoint. Windows reaches the daemon over loopback TCP and
    // a `.port` file; every target that is not unix can use that path.
    Edit::Replace {
        file: "connection.rs",
        from: "#[cfg(windows)]\nfn get_port_path(",
        to: "#[cfg(not(unix))]\nfn get_port_path(",
    },
    Edit::Replace {
        file: "connection.rs",
        from: "#[cfg(windows)]\nfn port_identity_for_session(",
        to: "#[cfg(not(unix))]\nfn port_identity_for_session(",
    },
    Edit::Replace {
        file: "connection.rs",
        from: "#[cfg(windows)]\npub fn get_port_for_session(",
        to: "#[cfg(not(unix))]\npub fn get_port_for_session(",
    },
    Edit::Replace {
        file: "connection.rs",
        from: "#[cfg(windows)]\npub fn resolve_port(",
        to: "#[cfg(not(unix))]\npub fn resolve_port(",
    },
    Edit::Replace {
        file: "connection.rs",
        from: "    #[cfg(windows)]\n    let endpoint_info = match resolve_port(session) {",
        to: "    #[cfg(not(unix))]\n    let endpoint_info = match resolve_port(session) {",
    },
    Edit::Replace {
        file: "native/daemon.rs",
        from: "#[cfg(windows)]\nasync fn bind_listener(",
        to: "#[cfg(not(unix))]\nasync fn bind_listener(",
    },
    Edit::Replace {
        file: "native/daemon.rs",
        from: "#[cfg(windows)]\nasync fn run_socket_server(",
        to: "#[cfg(not(unix))]\nasync fn run_socket_server(",
    },
    Edit::Replace {
        file: "native/daemon.rs",
        from: "    #[cfg(windows)]\n    {\n        let _ = fs::remove_file(SessionFile::Port.path_in(&socket_dir, session));",
        to: "    #[cfg(not(unix))]\n    {\n        let _ = fs::remove_file(SessionFile::Port.path_in(&socket_dir, session));",
    },
    Edit::Replace {
        file: "native/daemon.rs",
        from: "#[cfg(windows)]\nfn get_port_for_session(",
        to: "#[cfg(not(unix))]\nfn get_port_for_session(",
    },
    Edit::Replace {
        file: "native/stream/http.rs",
        from: "#[cfg(windows)]\nuse crate::connection::resolve_port;",
        to: "#[cfg(not(unix))]\nuse crate::connection::resolve_port;",
    },
    Edit::Replace {
        file: "main.rs",
        from: "    #[cfg(windows)]\n    {\n        fs::write(path, contents)\n    }",
        to: "    #[cfg(not(unix))]\n    {\n        fs::write(path, contents)\n    }",
    },
    // `signal::ctrl_c` never completes in the tokio shim. Without an arm the
    // function returns at once and the daemon shuts down.
    Edit::Replace {
        file: "native/daemon.rs",
        from: "    #[cfg(windows)]\n    {\n        if let Err(e) = signal::ctrl_c().await {",
        to: "    #[cfg(not(unix))]\n    {\n        if let Err(e) = signal::ctrl_c().await {",
    },
    // Functions whose body is one `cfg(unix)` arm and one `cfg(windows)`
    // arm get a third arm. Sockets and processes come from the host.
    Edit::After {
        file: "connection.rs",
        anchor: "        ok && code == windows_sys::Win32::Foundation::STILL_ACTIVE as u32\n    }\n",
        text: "    #[cfg(not(any(unix, windows)))]\n    {\n        agent_browser_host::process_alive(pid)\n    }\n",
    },
    Edit::After {
        file: "connection.rs",
        anchor: "                Duration::from_millis(50),\n            )\n            .is_ok()\n        })\n    }\n",
        text: "    #[cfg(not(any(unix, windows)))]\n    {\n        resolve_port(session).is_some_and(|port| agent_browser_host::tcp_connect(port).is_ok())\n    }\n",
    },
    Edit::After {
        file: "connection.rs",
        anchor: "            .map(Connection::Tcp)\n            .map_err(|e| format!(\"Failed to connect: {}\", e))\n    }\n",
        text: "    #[cfg(not(any(unix, windows)))]\n    {\n        let port = resolve_port(session)\n            .ok_or_else(|| \"Failed to connect: daemon has not written its port\".to_string())?;\n        agent_browser_host::tcp_connect(port)\n            .map(Connection::Tcp)\n            .map_err(|e| format!(\"Failed to connect: {}\", e))\n    }\n",
    },
    // Platform setup that an OS does before `main`.
    Edit::After {
        file: "main.rs",
        anchor: "\nfn main() {\n",
        text: "    agent_browser_host::init();\n",
    },
    // WASI has no file locks. The host provides one that it releases when
    // the file is closed.
    Edit::Replace {
        file: "connection.rs",
        from: "        file.lock()\n            .map_err(|e| format!(\"Failed to lock session {}: {}\", session, e))?;",
        to: "        agent_browser_host::lock_file(&file)\n            .map_err(|e| format!(\"Failed to lock session {}: {}\", session, e))?;",
    },
    // `current_exe` is unsupported on WASI, and the daemon is a second
    // instance of this module that the host starts.
    Edit::Replace {
        file: "connection.rs",
        from: "    let exe_path = env::current_exe().map_err(|e| e.to_string())?;\n",
        to: "    let exe_path = agent_browser_host::current_exe();\n",
    },
    Edit::After {
        file: "connection.rs",
        anchor: "                .map_err(|e| format!(\"Failed to start daemon: {}\", e))?,\n        );\n    }\n\n    let spawned_pid",
        text: " = {\n        let mut cmd = Command::new(&exe_path);\n        cmd.env(\"AGENT_BROWSER_DAEMON\", \"1\");\n        apply_daemon_env(&mut cmd, session, opts);\n        let pid = agent_browser_host::spawn(&cmd)\n            .map_err(|e| format!(\"Failed to start daemon: {}\", e))?;\n        Some(pid)\n    };\n    let _",
    },
    Edit::After {
        file: "install.rs",
        anchor: "        Command::new(\"where\")\n            .arg(cmd)\n            .stdout(Stdio::null())\n            .stderr(Stdio::null())\n            .status()\n            .map(|s| s.success())\n            .unwrap_or(false)\n    }\n",
        text: "    #[cfg(not(any(unix, windows)))]\n    {\n        let _ = cmd;\n        false\n    }\n",
    },
    Edit::Replace {
        file: "native/stream/http.rs",
        from: "    #[cfg(windows)]\n    let stream = {\n        let port = resolve_port(session_name)",
        to: "    #[cfg(not(unix))]\n    let stream = {\n        let port = resolve_port(session_name)",
    },
    Edit::After {
        file: "native/cdp/chrome.rs",
        anchor: "#[cfg(target_os = \"windows\")]\nfn build_playwright_binary_path(chromium_dir: &Path) -> PathBuf {\n    chromium_dir.join(\"chrome-win/chrome.exe\")\n}\n",
        text: "\n#[cfg(not(any(target_os = \"linux\", target_os = \"macos\", target_os = \"windows\")))]\nfn build_playwright_binary_path(chromium_dir: &Path) -> PathBuf {\n    chromium_dir.join(\"chrome-linux/chrome\")\n}\n",
    },
];

fn fail(message: String) -> ! {
    eprintln!("\nagent-browser-wasm: {message}");
    eprintln!("Upstream changed the code this edit targets. Update EDITS in wasm/build.rs.\n");
    std::process::exit(1);
}

fn copy_tree(from: &Path, to: &Path, sources: &mut Vec<PathBuf>) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if source.is_dir() {
            copy_tree(&source, &target, sources);
        } else if source.extension().is_some_and(|ext| ext == "rs") {
            sources.push(target);
        } else {
            // Files that upstream embeds with `include_str!`.
            std::fs::copy(&source, &target).unwrap();
        }
    }
}

fn generate_sources(out: &Path) {
    let upstream_src = Path::new(UPSTREAM).join("src");
    println!("cargo:rerun-if-changed={}", upstream_src.display());
    let root = out.join("src");
    let _ = std::fs::remove_dir_all(&root);
    let mut targets = Vec::new();
    copy_tree(&upstream_src, &root, &mut targets);

    let mut files: Vec<(String, String)> = targets
        .iter()
        .map(|target| {
            let relative = target.strip_prefix(&root).unwrap();
            let name = relative.to_string_lossy().replace('\\', "/");
            let text = std::fs::read_to_string(upstream_src.join(relative)).unwrap();
            (name, text)
        })
        .collect();

    for edit in EDITS {
        match edit {
            Edit::Everywhere { from, to } => {
                let mut hits = 0;
                for (_, text) in &mut files {
                    hits += text.matches(from).count();
                    *text = text.replace(from, to);
                }
                if hits == 0 {
                    fail(format!("`{from}` no longer appears in cli/src"));
                }
            }
            Edit::Replace { file, from, to } => {
                let text = source(&mut files, file);
                expect_once(file, text, from);
                *text = text.replacen(from, to, 1);
            }
            Edit::After { file, anchor, text: insert } => {
                let text = source(&mut files, file);
                expect_once(file, text, anchor);
                let at = text.find(anchor).unwrap() + anchor.len();
                text.insert_str(at, insert);
            }
        }
    }

    for (name, text) in &files {
        std::fs::write(root.join(name), text).unwrap();
    }
}

fn source<'a>(files: &'a mut [(String, String)], file: &str) -> &'a mut String {
    match files.iter_mut().find(|(name, _)| name == file) {
        Some((_, text)) => text,
        None => fail(format!("cli/src/{file} no longer exists")),
    }
}

fn expect_once(file: &str, text: &str, needle: &str) {
    let hits = text.matches(needle).count();
    if hits != 1 {
        fail(format!(
            "expected one match in cli/src/{file}, found {hits}, for:\n{needle}"
        ));
    }
}

/// Reads `name = ...` keys of the `[dependencies]` table.
fn dependency_names(manifest: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(manifest).unwrap();
    let mut names = Vec::new();
    let mut in_table = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_table = line == "[dependencies]";
        } else if in_table && !line.starts_with('#') {
            if let Some((name, _)) = line.split_once('=') {
                names.push(name.trim().to_string());
            }
        }
    }
    names
}

fn check_dependencies() {
    let upstream = Path::new(UPSTREAM).join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", upstream.display());
    let theirs = dependency_names(&upstream);
    let ours = dependency_names(Path::new("Cargo.toml"));
    let missing: Vec<_> = theirs.iter().filter(|name| !ours.contains(name)).collect();
    if !missing.is_empty() {
        eprintln!("\nagent-browser-wasm: cli/Cargo.toml depends on {missing:?}, wasm/Cargo.toml does not.");
        eprintln!("Add each crate to wasm/Cargo.toml, or a shim under wasm/shims if it needs an OS.\n");
        std::process::exit(1);
    }
}

fn upstream_version() -> String {
    let text = std::fs::read_to_string(Path::new(UPSTREAM).join("Cargo.toml")).unwrap();
    text.lines()
        .find_map(|line| line.strip_prefix("version = "))
        .map(|version| version.trim().trim_matches('"').to_string())
        .expect("cli/Cargo.toml has a version")
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    check_dependencies();
    generate_sources(&out);
    println!("cargo:rustc-env=AGENT_BROWSER_VERSION={}", upstream_version());
    println!("cargo:rerun-if-changed=build.rs");
    upstream::run();
}
